// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::metrics::DateRange;
use crate::model::{Event, Session};

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

  /// The entries the words of a text match, one per word: words split at anything but letters.
  pub fn matches(&self, text: &str) -> Vec<String> {
    normalize(text).split(|c: char| !c.is_alphabetic()).filter_map(|x| self.find(x)).collect()
  }
}

/// The prompts counted under one name (a model or a project) and those with swearing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tally {
  pub name: String,
  pub prompts: u64,
  pub swearing: u64,
  /// The entries matched, by count, most first.
  pub words: Vec<(String, u64)>,
}

impl Tally {
  pub fn share(&self) -> f64 {
    self.swearing as f64 / self.prompts.max(1) as f64
  }

  fn add(&mut self, words: &[String]) {
    self.prompts += 1;
    self.swearing += u64::from(!words.is_empty());
    for w in words {
      match self.words.iter_mut().find(|x| x.0 == *w) {
        Some(x) => x.1 += 1,
        None => self.words.push((w.clone(), 1)),
      }
    }
  }
}

/// The prompts of main sessions in the range by name, and their total. A prompt a fork or a copied
/// file repeats counts once; agents' prompts, which a model wrote, not at all. Sorted by the share
/// of prompts with swearing, then by their number.
pub fn tally(
  sessions: &[Session],
  dict: &Dict,
  range: &DateRange,
  name: impl Fn(&Session, &Event) -> String,
) -> (Vec<Tally>, Tally) {
  // A prompt by its harness, time and words, and the session that holds it first.
  let mut seen = HashMap::new();
  let mut groups: HashMap<String, Tally> = HashMap::new();
  let mut total = Tally { name: "total".into(), ..Default::default() };
  for (i, s) in sessions.iter().enumerate().filter(|x| x.1.is_main) {
    for e in &s.events {
      let (Some(text), Some(ts)) = (e.prompt.as_deref(), e.ts) else { continue };
      // Alike prompts in one session are each typed: fx logs a batch of events at one time.
      if !range.contains(ts) || *seen.entry((s.provider, ts, text)).or_insert(i) != i {
        continue;
      }
      let words = dict.matches(text);
      groups.entry(name(s, e)).or_default().add(&words);
      total.add(&words);
    }
  }
  let mut groups: Vec<Tally> = groups.into_iter().map(|(name, x)| Tally { name, ..x }).collect();
  for x in groups.iter_mut().chain([&mut total]) {
    x.words.sort_by(|a, b| (b.1, &a.0).cmp(&(a.1, &b.0)));
  }
  // Shares compared exactly, as fractions.
  let share = |x: &Tally, y: &Tally| x.swearing as u128 * y.prompts.max(1) as u128;
  groups.sort_by(|a, b| {
    share(b, a).cmp(&share(a, b)).then(b.swearing.cmp(&a.swearing)).then(a.name.cmp(&b.name))
  });
  (groups, total)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::Provider;

  fn bundled() -> Dict {
    Dict::load(None).unwrap()
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
      assert_eq!(dict.matches(word), [entry], "{word}");
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
      assert_eq!(dict.matches(word), Vec::<String>::new(), "{word}");
    }
    // Words split at anything but letters: `f*ck` and `sh1t` aren't words of the list.
    assert_eq!(dict.matches("f*ck, sh1t. WTF?! fuck_this"), ["wtf", "fuck*"]);
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
      assert_eq!(dict.matches(word), [entry], "{word}");
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
      assert_eq!(dict.matches(word), Vec::<String>::new(), "{word}");
    }
    assert_eq!(dict.matches("ну пиздец... а-а-а, сука!"), ["пизд*", "сук*"]);
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
    assert_eq!(dict.matches("Scheißegal fuck"), ["scheiße*", "fuck*"]);
    std::fs::write(dir.join("xx.txt"), "o k\n").unwrap();
    let err = format!("{:#}", Dict::load(Some(&dir)).unwrap_err());
    assert!(err.contains("xx.txt") && err.contains("line 1:"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
    // A missing directory has no lists.
    assert!(Dict::load(Some(&dir)).is_ok());
  }

  #[test]
  fn tallies_main_prompts_once() {
    let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_microsecond();
    let prompt = |ts: &str, model: &str, text: &str| Event {
      ts: Some(at(ts)),
      model: Some(model.into()),
      prompt: Some(text.into()),
      ..Default::default()
    };
    let mut main = Session::new("a", true, "/r", Provider::Claude);
    main.events = vec![
      prompt("2026-01-01T10:00:00Z", "x", "fuck this shit"),
      prompt("2026-01-01T10:01:00Z", "x", "ok"),
      prompt("2026-01-01T10:02:00Z", "y", "блядь"),
      prompt("2026-01-01T10:02:00Z", "y", "блядь"),
      prompt("2026-01-01T10:03:00Z", "z", "fine"),
      prompt("2026-01-01T10:04:00Z", "z", "good"),
      Event::activity(at("2026-01-01T10:05:00Z")),
      prompt("2026-03-01T10:00:00Z", "y", "fuck"),
    ];
    // A copy of a prompt in another session counts once, an agent's not at all; alike prompts in
    // one session count each.
    let mut copy = Session::new("b", true, "/r", Provider::Claude);
    copy.events = vec![prompt("2026-01-01T10:02:00Z", "w", "блядь")];
    let mut agent = Session::new("c", false, "/r", Provider::Claude);
    agent.events = vec![prompt("2026-01-01T10:06:00Z", "y", "fuck")];
    let sessions = [main, copy, agent];
    let range = DateRange { since: None, until: Some(jiff::civil::date(2026, 2, 1)) };
    let model = |_: &Session, e: &Event| e.model.clone().unwrap();
    let (groups, total) = tally(&sessions, &bundled(), &range, model);

    let rows: Vec<_> = groups.iter().map(|x| (x.name.as_str(), x.prompts, x.swearing)).collect();
    // By share, then by the prompts with swearing.
    assert_eq!(rows, [("y", 2, 2), ("x", 2, 1), ("z", 2, 0)]);
    assert_eq!(groups[1].words, [("fuck*".to_string(), 1), ("shit*".to_string(), 1)]);
    assert_eq!((total.prompts, total.swearing, total.share()), (6, 3, 0.5));
    let words: Vec<_> = total.words.iter().map(|x| (x.0.as_str(), x.1)).collect();
    assert_eq!(words, [("бля*", 2), ("fuck*", 1), ("shit*", 1)]);
    assert_eq!(groups[2].share(), 0.0);
  }
}
