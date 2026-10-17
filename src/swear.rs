// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::model::{Prompt, Provider, Session, Ts};

const BUNDLED: [(&str, &str); 2] =
  [("en.txt", include_str!("swear/en.txt")), ("ru.txt", include_str!("swear/ru.txt"))];

/// In this order an exact word wins over a stem of its length, and an exception over both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
  Stem,
  Word,
  NotWord,
  NotStem,
}

/// Swear words: `word` matches the word, `stem*` every word it starts, `!word` and `!stem*` (a
/// stem's false positives) never match. Words compare lowercased, with `ё` as `е`.
#[derive(Debug, Default)]
pub struct Dict {
  /// Sorted: the entries starting with a prefix follow one another.
  entries: Vec<(String, Kind)>,
}

fn normalize(text: &str) -> String {
  text.to_lowercase().replace('ё', "е")
}

impl Dict {
  /// The bundled dictionaries and every `*.txt` in `dir`.
  pub fn load(dir: Option<&Path>) -> Result<Self> {
    let mut dict = Self::default();
    for (name, text) in BUNDLED {
      dict.add(text).with_context(|| format!("failed to parse src/swear/{name}"))?;
    }
    let entries = dir.and_then(|x| std::fs::read_dir(x).ok()).into_iter().flatten().flatten();
    let mut files: Vec<PathBuf> = entries.map(|x| x.path()).collect();
    files.retain(|x| x.extension().is_some_and(|x| x == "txt") && x.is_file());
    files.sort();
    for path in files {
      let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
      dict.add(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    }
    dict.entries.sort();
    dict.entries.dedup();
    Ok(dict)
  }

  /// Adds the entries of a dictionary file: one per line, `#` starts a comment.
  fn add(&mut self, text: &str) -> Result<()> {
    for (i, line) in text.lines().enumerate() {
      let entry = normalize(line.split('#').next().unwrap_or_default().trim());
      let (not, x) = entry.strip_prefix('!').map_or((false, entry.as_str()), |x| (true, x));
      let (stem, x) = x.strip_suffix('*').map_or((false, x), |x| (true, x));
      if entry.is_empty() {
        continue;
      }
      if x.is_empty() || !x.chars().all(char::is_alphabetic) {
        bail!("line {}: `{}` is not a word or a stem", i + 1, line.trim());
      }
      let kinds = [Kind::Word, Kind::Stem, Kind::NotWord, Kind::NotStem];
      self.entries.push((x.to_string(), kinds[usize::from(stem) + 2 * usize::from(not)]));
    }
    Ok(())
  }

  /// The entry a normalized word matches: an exact word, else the longest stem it starts with.
  /// Each longer prefix of the word narrows the entries to those starting with it.
  fn find(&self, word: &str) -> Option<String> {
    let (mut run, mut found) = (self.entries.as_slice(), None);
    for (i, c) in word.char_indices() {
      let end = i + c.len_utf8();
      let prefix = &word[..end];
      run = &run[run.partition_point(|x| x.0.as_str() < prefix)..];
      run = &run[..run.partition_point(|x| x.0.starts_with(prefix))];
      for (_, kind) in run.iter().take_while(|x| x.0 == prefix) {
        match kind {
          Kind::Stem => found = Some(format!("{prefix}*")),
          Kind::Word if end == word.len() => found = Some(prefix.to_string()),
          Kind::NotWord if end == word.len() => return None,
          Kind::NotStem => return None,
          _ => {}
        }
      }
      if run.is_empty() {
        break;
      }
    }
    found
  }

  /// A prompt's counts: its words (split at anything but letters), the swear words among them
  /// and a hash of them all. Its text isn't kept.
  pub fn prompt(&self, ts: Ts, text: &str) -> Prompt {
    let text = normalize(text);
    let words: Vec<&str> =
      text.split(|c: char| !c.is_alphabetic()).filter(|x| !x.is_empty()).collect();
    let mut hash = DefaultHasher::new();
    words.hash(&mut hash);
    let swears = words.iter().filter(|x| self.find(x).is_some()).count();
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    Prompt {
      ts,
      model: None,
      words: count(words.len()),
      swears: count(swears),
      hash: hash.finish(),
    }
  }
}

/// Takes the prompts out of the sessions, which are then as if read without them: the events only
/// for a prompt go, and the sessions they leave empty. Returns the main sessions' prompts, one that
/// another session repeats (a fork, a copied file) once; alike prompts in one session count each,
/// as fx logs a batch of events at one time.
pub fn take_prompts(sessions: &mut Vec<Session>) -> Vec<(Provider, Prompt)> {
  let mut seen = HashMap::new();
  let mut out = vec![];
  let mut keep = vec![];
  for (i, s) in sessions.iter_mut().enumerate() {
    let (provider, main, had) = (s.provider, s.is_main, s.events.len());
    s.events.retain_mut(|e| {
      let Some(x) = e.prompt.take() else { return true };
      if main && *seen.entry((provider, x.ts, x.hash)).or_insert(i) == i {
        out.push((provider, *x));
      }
      // Codex's `UserMessage` items: events of no time and no usage, there for their prompts.
      e.ts.is_some() || e.usage.is_some()
    });
    keep.push(had == 0 || !s.events.is_empty());
  }
  let mut keep = keep.into_iter();
  sessions.retain(|_| keep.next().unwrap_or(true));
  out
}

/// Words at the user's overall rate a model's own words are weighed with: a model with few words
/// typed to it scores near the overall rate, one with many near its own.
pub const PRIOR_WORDS: f64 = 2000.0;
/// Fewest words typed to a model for its rates to show.
pub const MIN_WORDS: u64 = 200;

/// Swear words per 1,000 words typed, shrunk toward the overall rate (swear words per word) by
/// `PRIOR_WORDS`: empirical-Bayes shrinkage, so a barely used model sorts near the average.
pub fn score(swears: u64, words: u64, overall: f64) -> f64 {
  (swears as f64 + PRIOR_WORDS * overall) / (words as f64 + PRIOR_WORDS) * 1000.0
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::Event;

  fn bundled() -> Dict {
    Dict::load(None).unwrap()
  }

  /// The entries the words of a text match, one per word.
  fn matches(dict: &Dict, text: &str) -> Vec<String> {
    let text = normalize(text);
    text.split(|c: char| !c.is_alphabetic()).filter_map(|x| dict.find(x)).collect()
  }

  #[test]
  fn finds_english_swearing() {
    let dict = bundled();
    let hits = [
      ("fuck", "fuck*"),
      ("Fucking", "fuck*"),
      ("FUCKED", "fuck*"),
      ("motherfucker", "motherfuck*"),
      ("shit", "shit*"),
      ("shitty", "shit*"),
      ("bullshit", "bullshit*"),
      ("WTF", "wtf"),
      ("damn", "damn*"),
      ("goddamn", "goddam*"),
      ("dammit", "dammit"),
      ("asshole", "asshole*"),
      ("ass", "ass"),
      ("bitch", "bitch*"),
      ("crap", "crap"),
      ("dumbass", "dumbass*"),
      ("pissed", "pissed"),
      ("wanker", "wank*"),
      ("ffs", "ffs"),
    ];
    for (word, entry) in hits {
      assert_eq!(matches(&dict, word), [entry], "{word}");
    }
    let misses = [
      "class",
      "assert",
      "assistant",
      "pass",
      "bass",
      "assume",
      "scrap",
      "crappie",
      "cocktail",
      "dickens",
      "shiitake",
      "shitake",
      "shitcoin",
      "shitcoins",
      "wankel",
      "hello",
      "Scunthorpe",
      "piston",
      "pissarro",
      "dammam",
      "fuchsia",
      "skill",
      "therapist",
      "f",
      "ck",
    ];
    for word in misses {
      assert_eq!(matches(&dict, word), Vec::<String>::new(), "{word}");
    }
    // Words split at anything but letters: `f*ck` and `sh1t` aren't words of the list.
    assert_eq!(matches(&dict, "f*ck, sh1t. WTF?! fuck_this"), ["wtf", "fuck*"]);
  }

  #[test]
  fn finds_russian_swearing() {
    let dict = bundled();
    let hits = [
      ("хуй", "хуй*"),
      ("Хуйня", "хуй*"),
      ("нахуй", "нахуй*"),
      ("похуй", "похуй*"),
      ("охуеть", "охуе*"),
      ("охуительно", "охуи*"),
      ("ахуенно", "ахуе*"),
      ("нихуя", "нихуя*"),
      ("хуёво", "хуе*"),
      ("пиздец", "пизд*"),
      ("Пиздато", "пизд*"),
      ("распиздяй", "распизд*"),
      ("спиздил", "спизд*"),
      ("ебать", "еб*"),
      ("ёбаный", "еб*"),
      ("ЁБНУТЫЙ", "еб*"),
      ("заебал", "заеб*"),
      ("выёбывается", "выеб*"),
      ("отъебись", "отъеб*"),
      ("уёбок", "уеб*"),
      ("съебался", "съеб*"),
      ("проебал", "проеб*"),
      ("наебалово", "наеб*"),
      ("долбоёб", "долбоеб*"),
      ("бля", "бля*"),
      ("блядь", "бля*"),
      ("блять", "бля*"),
      ("сука", "сук*"),
      ("суки", "сук*"),
      ("сучара", "сучар*"),
      ("мудак", "мудак*"),
      ("мудила", "мудил*"),
      ("говнокод", "говн*"),
      ("жопа", "жоп*"),
      ("пидорас", "пидор*"),
      ("залупа", "залуп*"),
      ("херня", "херн*"),
      ("нахер", "нахер"),
      ("срань", "сран*"),
      ("обосрался", "обосра*"),
      ("дерьмо", "дерьм*"),
      ("епта", "епта"),
      ("blyat", "blyat*"),
    ];
    for (word, entry) in hits {
      assert_eq!(matches(&dict, word), [entry], "{word}");
    }
    // Words that contain a root or start like one.
    let misses = [
      "употреблять",
      "употребление",
      "потребляет",
      "оскорблять",
      "оскорбление",
      "скипидар",
      "рубля",
      "корабля",
      "колеблю",
      "сукно",
      "сукна",
      "суконный",
      "сукровица",
      "суккулент",
      "сук",
      "суку",
      "бляха",
      "бляшка",
      "мудрый",
      "мудрость",
      "хулиган",
      "застрахуй",
      "психуй",
      "небо",
      "хлеб",
      "хлебать",
      "веб",
      "себя",
      "команда",
      "мандарин",
      "мандат",
      "гондола",
      "страна",
      "сравнить",
      "сранвения",
      "херсон",
      "оскорбил",
      "требуется",
      "ребята",
      "бляхой",
    ];
    for word in misses {
      assert_eq!(matches(&dict, word), Vec::<String>::new(), "{word}");
    }
    assert_eq!(matches(&dict, "ну пиздец... а-а-а, сука!"), ["пизд*", "сук*"]);
  }

  #[test]
  fn reads_entries_and_exceptions() {
    let mut dict = Dict::default();
    let text = "# a comment\n\nfoo  # trailing\nba*\nbar*\n!bark*\n!bas\nЁЖ*\nbaz\nbaz*\n";
    dict.add(text).unwrap();
    dict.entries.sort();
    let find = |x: &str| dict.find(x);
    // An exact word over a stem, the longest stem, an exception over both.
    assert_eq!(find("foo"), Some("foo".into()));
    assert_eq!(find("food"), None);
    assert_eq!(find("baz"), Some("baz".into()));
    assert_eq!(find("bazz"), Some("baz*".into()));
    assert_eq!(find("barn"), Some("bar*".into()));
    assert_eq!(find("bat"), Some("ba*".into()));
    assert_eq!(find("bark"), None);
    assert_eq!(find("barking"), None);
    assert_eq!(find("bas"), None);
    assert_eq!(find("bass"), Some("ba*".into()));
    assert_eq!(find("b"), None);
    assert_eq!(find("ежик"), Some("еж*".into()));

    for bad in ["two words", "*", "!", "f*ck", "sh1t"] {
      let err = Dict::default().add(&format!("ok\n{bad}\n")).unwrap_err().to_string();
      assert!(err.contains("line 2:"), "{err}");
    }
  }

  #[test]
  fn adds_the_users_lists() {
    let dir = std::env::temp_dir().join(format!("cctrail-swear-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("de.txt"), "scheiße*\n# Mist\n").unwrap();
    std::fs::write(dir.join("notes.md"), "not a list\n").unwrap();
    let dict = Dict::load(Some(&dir)).unwrap();
    assert_eq!(matches(&dict, "Scheißegal fuck"), ["scheiße*", "fuck*"]);
    std::fs::write(dir.join("xx.txt"), "o k\n").unwrap();
    let err = format!("{:#}", Dict::load(Some(&dir)).unwrap_err());
    assert!(err.contains("xx.txt") && err.contains("line 1:"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
    // A missing directory has no lists.
    assert!(Dict::load(Some(&dir)).is_ok());
  }

  #[test]
  fn counts_a_prompt_without_its_text() {
    let dict = bundled();
    let x = dict.prompt(5, "Ну БЛЯДЬ, опять 3 fucking times");
    assert_eq!((x.ts, x.model, x.words, x.swears), (5, None, 5, 2));
    // Alike words, alike hash, whatever the case and the signs between them.
    assert_eq!(dict.prompt(1, "ну блядь опять fucking times!").hash, x.hash);
    assert_ne!(dict.prompt(1, "ну блядь").hash, x.hash);
    assert_eq!(dict.prompt(1, "…").words, 0);
  }

  #[test]
  fn takes_main_prompts_once() {
    let dict = bundled();
    let prompt = |ts: Ts, text: &str| {
      let p = Prompt { model: Some("m".into()), ..dict.prompt(ts, text) };
      Event { ts: Some(ts), prompt: Some(Box::new(p)), ..Default::default() }
    };
    let only = |ts: Ts, text: &str| Event { ts: None, ..prompt(ts, text) };
    let mut main = Session::new("a", true, "/r", Provider::Claude);
    // Alike prompts in one session count each; the event only for a prompt goes.
    main.events = vec![prompt(1, "fuck"), prompt(1, "fuck"), only(2, "ok"), Event::default()];
    // A copy in another session counts once, an agent's prompt not at all.
    let mut copy = Session::new("b", true, "/r", Provider::Claude);
    copy.events = vec![prompt(1, "fuck"), prompt(3, "new")];
    let mut agent = Session::new("c", false, "/r", Provider::Claude);
    agent.events = vec![prompt(4, "fuck")];
    // A session with only a prompt's event goes; one without events stays, as fx keeps some.
    let mut gone = Session::new("d", true, "/r", Provider::Codex);
    gone.events = vec![only(5, "bye")];
    let empty = Session::new("e", true, "/r", Provider::Fx);
    let mut sessions = vec![main, copy, agent, gone, empty];
    let prompts = take_prompts(&mut sessions);
    let counted: Vec<_> = prompts.iter().map(|x| (x.0, x.1.ts, x.1.swears)).collect();
    let claude = Provider::Claude;
    assert_eq!(
      counted,
      [(claude, 1, 1), (claude, 1, 1), (claude, 2, 0), (claude, 3, 0), (Provider::Codex, 5, 0)]
    );
    let ids: Vec<_> = sessions.iter().map(|x| (x.id.as_str(), x.events.len())).collect();
    assert_eq!(ids, [("a", 3), ("b", 2), ("c", 1), ("e", 0)]);
    assert!(sessions.iter().flat_map(|x| &x.events).all(|x| x.prompt.is_none()));
  }

  #[test]
  fn scores_shrink_toward_the_overall_rate() {
    // An overall 10 swear words per 1,000: a model with 20 words and 10 of them swear words
    // scores barely over it, one with 20,000 words at 50 per 1,000 near its own rate.
    let overall = 0.01;
    let few = score(10, 20, overall);
    assert!((few - 30.0 / 2020.0 * 1000.0).abs() < 1e-9 && few < 15.0, "{few}");
    let many = score(1000, 20_000, overall);
    assert!((many - 1020.0 / 22_000.0 * 1000.0).abs() < 1e-9 && many > 45.0, "{many}");
    // The overall rate scores as itself; no words at all, the overall rate.
    assert!((score(100, 10_000, overall) - 10.0).abs() < 1e-9);
    assert!((score(0, 0, overall) - 10.0).abs() < 1e-9);
  }
}
