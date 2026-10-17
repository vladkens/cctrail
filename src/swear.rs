// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::model::{Prompt, Provider, Session, Ts};

/// The dictionaries by file name: `insult-*` ones hold insults.
const BUNDLED: [(&str, &str); 4] = [
  ("en.txt", include_str!("swear/en.txt")),
  ("ru.txt", include_str!("swear/ru.txt")),
  ("insult-en.txt", include_str!("swear/insult-en.txt")),
  ("insult-ru.txt", include_str!("swear/insult-ru.txt")),
];

/// Consecutive lines that don't read as prose, as a pasted log, stack trace or table has.
const PASTED_LINES: usize = 3;
/// How near a swear word an addressee makes it aimed at the listener, in words.
const NEAR: usize = 3;
/// Addressees, a swear word near one aimed at the listener: second-person pronouns, and the names
/// of models and agents, Russian ones in their cases (the stems would catch `соль`, `астроном`).
const ADDRESSEES: &str = "ты тебе тебя тобой тебой твой твоя твое твои твою твоего твоей твоим \
  твоих you your yours yourself u \
  астра астры астре астру астрой сол сола солу солом соле луна луны луне луну луной терра терры \
  терре терру террой опус опуса опусу опусом опусе соннет соннета соннету соннетом фейбл фейбла \
  фейблу фейблом клод клода клоду клодом клоде кодекс кодекса кодексу кодексом кодексе гпт агент \
  агента агенту агентом агенте агенты агентов модель модели моделью моделей бот бота боту ботом \
  боте боты ботов astra sol luna terra opus sonnet fable claude codex gpt agent agents model \
  models bot bots";

/// What a word is: swearing, swearing only next to an addressee (`>` in an insult file, as
/// `тупишь`), or an insult, aimed at the listener wherever it is. Of entries alike, the later wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
  Swear,
  Near,
  Insult,
}

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
  entries: Vec<(String, Kind, Class)>,
}

fn normalize(text: &str) -> String {
  text.to_lowercase().replace('ё', "е")
}

/// What the user typed in a prompt, without what looks pasted: no harness marks a paste in the
/// history (Claude Code only in its own `history.jsonl`, and not for every prompt), so code in
/// backticks, quoted lines (`> `) and runs of `PASTED_LINES` lines that aren't prose go. Typed or
/// dictated words are prose, usually one long line.
fn typed(text: &str) -> String {
  let mut lines: Vec<&str> = vec![];
  let mut fenced = false;
  for line in text.lines() {
    if line.trim_start().starts_with("```") {
      fenced = !fenced;
    } else if !fenced && !line.trim_start().starts_with('>') {
      lines.push(line);
    }
  }
  // Not prose: few letters, a time or date, a path, `key: value`, a table row, a line of code.
  let pasted = |line: &str| {
    let text: Vec<char> = line.chars().filter(|x| !x.is_whitespace()).collect();
    let letters = text.iter().filter(|x| x.is_alphabetic()).count();
    let digits = |x: &[char]| x.iter().all(|x| x.is_ascii_digit());
    let time = text.windows(5).any(|x| digits(&x[..2]) && x[2] == ':' && digits(&x[3..]));
    let date = text.windows(10).any(|x| digits(&x[..4]) && x[4] == '-' && x[7] == '-');
    let path = line.split_whitespace().any(|x| x.matches('/').count() > 1 || x.contains('\\'));
    let key = line.trim_start().split_once(": ").is_some_and(|x| !x.0.contains(' '));
    let code = line.trim_end().ends_with([';', '{', '}']) || line.matches('|').count() > 1;
    letters * 10 < text.len() * 6 || time || date || path || key || code
  };
  let mut keep = vec![true; lines.len()];
  let mut from = 0;
  for i in 0..=lines.len() {
    if i < lines.len() && pasted(lines[i]) {
      continue;
    }
    if i - from >= PASTED_LINES {
      keep[from..i].fill(false);
    }
    from = i + 1;
  }
  let kept = lines.iter().zip(keep).filter(|x| x.1).map(|x| *x.0);
  // Inline code: every other piece between backticks.
  let text = kept.collect::<Vec<_>>().join("\n");
  text.split('`').step_by(2).collect::<Vec<_>>().join(" ")
}

impl Dict {
  /// The bundled dictionaries and every `*.txt` in `dir`, `insult-*.txt` the insults.
  pub fn load(dir: Option<&Path>) -> Result<Self> {
    let mut dict = Self::default();
    for (name, text) in BUNDLED {
      let insults = name.starts_with("insult-");
      dict.add(text, insults).with_context(|| format!("failed to parse src/swear/{name}"))?;
    }
    let entries = dir.and_then(|x| std::fs::read_dir(x).ok()).into_iter().flatten().flatten();
    let mut files: Vec<PathBuf> = entries.map(|x| x.path()).collect();
    files.retain(|x| x.extension().is_some_and(|x| x == "txt") && x.is_file());
    files.sort();
    for path in files {
      let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
      let insults = path.file_name().is_some_and(|x| x.to_string_lossy().starts_with("insult-"));
      dict.add(&text, insults).with_context(|| format!("failed to parse {}", path.display()))?;
    }
    dict.entries.sort();
    dict.entries.dedup();
    Ok(dict)
  }

  /// Adds the entries of a dictionary file: one per line, `#` starts a comment.
  fn add(&mut self, text: &str, insults: bool) -> Result<()> {
    for (i, line) in text.lines().enumerate() {
      let entry = normalize(line.split('#').next().unwrap_or_default().trim());
      let (near, x) = match entry.strip_prefix('>').filter(|_| insults) {
        Some(x) => (true, x),
        None => (false, entry.as_str()),
      };
      let class =
        [Class::Swear, Class::Insult, Class::Near][usize::from(insults) + usize::from(near)];
      let (not, x) = x.strip_prefix('!').map_or((false, x), |x| (true, x));
      let (stem, x) = x.strip_suffix('*').map_or((false, x), |x| (true, x));
      if entry.is_empty() {
        continue;
      }
      if x.is_empty() || !x.chars().all(char::is_alphabetic) {
        bail!("line {}: `{}` is not a word or a stem", i + 1, line.trim());
      }
      let kinds = [Kind::Word, Kind::Stem, Kind::NotWord, Kind::NotStem];
      self.entries.push((x.to_string(), kinds[usize::from(stem) + 2 * usize::from(not)], class));
    }
    Ok(())
  }

  /// The entry a normalized word matches: an exact word, else the longest stem it starts with.
  /// Each longer prefix of the word narrows the entries to those starting with it.
  fn find(&self, word: &str) -> Option<(String, Class)> {
    let (mut run, mut found) = (self.entries.as_slice(), None);
    for (i, c) in word.char_indices() {
      let end = i + c.len_utf8();
      let prefix = &word[..end];
      run = &run[run.partition_point(|x| x.0.as_str() < prefix)..];
      run = &run[..run.partition_point(|x| x.0.starts_with(prefix))];
      for (_, kind, class) in run.iter().take_while(|x| x.0 == prefix) {
        match kind {
          Kind::Stem => found = Some((format!("{prefix}*"), *class)),
          Kind::Word if end == word.len() => found = Some((prefix.to_string(), *class)),
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

  /// A prompt's counts, of what the user typed (`typed`), in words split at anything but letters:
  /// the words, the swear words and insults among them, whether it swears at the listener (an
  /// insult, or a swear word near a `ты` or a model's name), and a hash of them all. Its text
  /// isn't kept. A word right before a `*` is a dictionary entry written out (`бля*` in a pasted
  /// report), no swearing.
  pub fn prompt(&self, ts: Ts, text: &str) -> Prompt {
    let text = normalize(&typed(text));
    // Each word and whether a `*` follows it.
    let (mut words, mut start): (Vec<(&str, bool)>, Option<usize>) = (vec![], None);
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
      match (c.is_alphabetic(), start) {
        (true, None) => start = Some(i),
        (false, Some(from)) => (words.push((&text[from..i], c == '*')), start = None).1,
        _ => {}
      }
    }
    let mut hash = DefaultHasher::new();
    words.iter().for_each(|x| x.0.hash(&mut hash));
    let class = |x: &(&str, bool)| self.find(x.0).filter(|_| !x.1).map(|x| x.1);
    let classes: Vec<Option<Class>> = words.iter().map(class).collect();
    let swears = classes.iter().filter(|x| matches!(x, Some(Class::Swear | Class::Insult))).count();
    let addressee = |i: usize| ADDRESSEES.split_whitespace().any(|x| x == words[i].0);
    let directed = classes.iter().enumerate().any(|(i, x)| match x {
      Some(Class::Insult) => true,
      Some(_) => (i.saturating_sub(NEAR)..(i + NEAR + 1).min(words.len())).any(addressee),
      None => false,
    });
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    Prompt {
      ts,
      model: None,
      words: count(words.len()),
      swears: count(swears),
      directed,
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

/// Prompts at the user's overall share of prompts swearing at the listener that a model's own
/// prompts are weighed with: a model with few prompts scores near the overall share, one with many
/// near its own. By prompts, so one angry prompt full of swearing counts once.
pub const PRIOR_PROMPTS: f64 = 30.0;
/// Fewest prompts to a model for its rates to show.
pub const MIN_PROMPTS: u64 = 10;

/// The percent of prompts swearing at the listener, shrunk toward the overall share (a fraction) by
/// `PRIOR_PROMPTS`: empirical-Bayes shrinkage, so a barely used model sorts near the average.
pub fn score(swearing: u64, prompts: u64, overall: f64) -> f64 {
  (swearing as f64 + PRIOR_PROMPTS * overall) / (prompts as f64 + PRIOR_PROMPTS) * 100.0
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
    text.split(|c: char| !c.is_alphabetic()).filter_map(|x| dict.find(x).map(|x| x.0)).collect()
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
      ("уёбок", "уебок*"),
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
    dict.add(text, false).unwrap();
    dict.add("insult*\n>near\nfoo\n", true).unwrap();
    dict.entries.sort();
    let find = |x: &str| dict.find(x).map(|x| x.0);
    let class = |x: &str| dict.find(x).map(|x| x.1);
    // Insults and words that count only near an addressee; an insult over a swear word alike.
    assert_eq!(
      (class("insults"), class("near"), class("foo")),
      (Some(Class::Insult), Some(Class::Near), Some(Class::Insult))
    );
    assert_eq!(class("baz"), Some(Class::Swear));
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
      let err = Dict::default().add(&format!("ok\n{bad}\n"), false).unwrap_err().to_string();
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
    // Entries written out, as in a pasted report, are words but no swearing.
    let x = dict.prompt(1, "блядь, top words: бля* 9230, еб* 31, fuck *");
    assert_eq!((x.words, x.swears), (6, 2));
  }

  #[test]
  fn tells_swearing_at_the_listener() {
    let dict = bundled();
    let directed = [
      "ты тупой",
      "что за херню ты сделал",
      "you idiot",
      "fuck you",
      "Ты опять тупишь",
      "агент, блядь, ну сколько можно",
      "claude, wtf",
      "Астра, ну ты и дура",
      "астра тупая",
      "сука, астра, опять сломала",
      "ебаная астра",
      "кодексу пиздец",
      "this model is fucking slow",
      "мразь",
      "ну ты и дебил",
      "this is stupid",
      "what the fuck are you doing",
      "твою мать, блядь",
      "ебаный ты кретин",
      "you're fucking useless",
      "долбоеб, переделай",
    ];
    for x in directed {
      assert!(dict.prompt(0, x).directed, "{x}");
    }
    let passing = [
      "хер пойми как отсортировано",
      "бля, опять дождь",
      "this is fucking great",
      "комп тупит, блядь",
      "тупик в логике, сука",
      "тупо скопируй файл",
      "сделай даунгрейд пакета",
      "shit, the build broke again",
      "блядь, ну и где здесь ошибка, в этом коде",
      "you wrote `бля*` in the list",
      "пересоли, соль блядь не та",
      "астроном, блядь, ошибся",
      "дурацкий баг",
    ];
    for x in passing {
      assert!(!dict.prompt(0, x).directed, "{x}");
    }
    // Three words apart at most; any swearing counts the insults too.
    assert!(dict.prompt(0, "ты сделал это, бля").directed);
    assert!(!dict.prompt(0, "ты сделал это опять вчера, бля").directed);
    assert_eq!(dict.prompt(0, "ты тупишь").swears, 0);
    assert_eq!(dict.prompt(0, "ты идиот, блядь").swears, 2);
  }

  #[test]
  fn counts_only_what_was_typed() {
    let dict = bundled();
    // A pasted log, a quote and code, then the user's own sentence: only the sentence counts.
    let text = "2026-10-17 12:00:01 ERROR db: fuck, connection refused\n\
      2026-10-17 12:00:02 WARN retry: shit\n    at src/db/pool.rs:42\n\
      > fuck you, said the docs\n```\nlet fuck = you;\n```\nрасскажи про `fuck you`\n\
      ну и что за херню ты сделал";
    let x = dict.prompt(0, text);
    assert_eq!((x.words, x.swears, x.directed), (9, 1, true));
    // A typed paragraph, however long, and up to two odd lines count whole.
    let text = "Слушай, ты всё сломал, блядь, давай нормально: прочитай файл, найди ошибку \
      и исправь её, а потом запусти тесты и покажи, что всё работает, без этой херни.";
    let x = dict.prompt(0, text);
    assert_eq!((x.words, x.swears, x.directed), (26, 2, true));
    let x = dict.prompt(0, "fuck you\n/usr/lib/x\nkey: value");
    assert_eq!((x.words, x.swears, x.directed), (7, 1, true));
    // A table: its rows go.
    let x = dict.prompt(0, "| a | b |\n| fuck | you |\n| x | y |\nok");
    assert_eq!((x.words, x.swears), (1, 0));
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
    // Overall 20% of prompts swear: a model with 2 prompts, both swearing, scores barely over
    // it, one with 300 prompts at 50% near its own share.
    let overall = 0.2;
    let few = score(2, 2, overall);
    assert!((few - 8.0 / 32.0 * 100.0).abs() < 1e-9 && few < 26.0, "{few}");
    let many = score(150, 300, overall);
    assert!((many - 156.0 / 330.0 * 100.0).abs() < 1e-9 && many > 47.0, "{many}");
    // The overall share scores as itself; no prompts at all, the overall share.
    assert!((score(20, 100, overall) - 20.0).abs() < 1e-9);
    assert!((score(0, 0, overall) - 20.0).abs() < 1e-9);
  }
}
