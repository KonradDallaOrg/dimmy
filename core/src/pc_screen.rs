//! What the window in front shows, as a person points at it — EXPERIMENT
//! (branch `exp/pc-actions`).
//!
//! "Apri il primo video", "il terzo risultato", "il video di Michele Cotti",
//! or a title read aloud: the answer is on the screen, not in a model. The
//! items come from UI Automation with their rectangle (`pc_appcmds`); here
//! they are put in reading order and a spoken reference picks one, in about
//! a millisecond. Measured 2026-11-10: the same requests through the Claude
//! CLI took 2.4-4.9 s and came back as a new search or as nothing, because
//! the capture had dropped every name longer than 60 characters, which is
//! every video title.
//!
//! "The first" is the first on the SCREEN: an item scrolled out of the
//! window is not in the list.

/// What an item is, as far as a click cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Link,
    Button,
    Tab,
    ListItem,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Link => "link",
            Kind::Button => "button",
            Kind::Tab => "tab",
            Kind::ListItem => "item",
            Kind::Other => "control",
        }
    }
}

/// Something on the screen a click reaches: its text and its rectangle in
/// screen pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub text: String,
    pub kind: Kind,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Item {
    pub fn center(&self) -> (i32, i32) {
        ((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }

    fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// Rows top to bottom, each row left to right: the order a person counts in.
pub fn in_reading_order(mut items: Vec<Item>) -> Vec<Item> {
    items.sort_by_key(|i| (i.top, i.left));
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    let mut row: Vec<Item> = Vec::new();
    let mut row_top = i32::MIN;
    let mut slack = 0;
    for item in items {
        if row.is_empty() || item.top > row_top + slack {
            row.sort_by_key(|i| i.left);
            out.append(&mut row);
            row_top = item.top;
            // Two things on one line rarely start at the same pixel.
            slack = (item.height() / 2).clamp(8, 40);
        }
        row.push(item);
    }
    row.sort_by_key(|i| i.left);
    out.append(&mut row);
    out
}

/// The thing a command names: "video", "risultato", "file".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Noun {
    Video,
    Link,
    File,
    Button,
    Tab,
    Any,
}

/// What a spoken reference resolves to.
#[derive(Clone, Debug, PartialEq)]
pub enum Pointed {
    /// The index of the item in the list that was searched.
    Item(usize),
    /// A browser tab by its place: the keys that go to it.
    Keys(String),
}

const ORDINALS: &[(&str, usize)] = &[
    ("primo", 0),
    ("prima", 0),
    ("first", 0),
    ("secondo", 1),
    ("seconda", 1),
    ("second", 1),
    ("terzo", 2),
    ("terza", 2),
    ("third", 2),
    ("quarto", 3),
    ("quarta", 3),
    ("fourth", 3),
    ("quinto", 4),
    ("quinta", 4),
    ("fifth", 4),
    ("sesto", 5),
    ("sesta", 5),
    ("sixth", 5),
    ("settimo", 6),
    ("settima", 6),
    ("ottavo", 7),
    ("ottava", 7),
    ("nono", 8),
    ("nona", 8),
    ("decimo", 9),
    ("decima", 9),
];

const LAST: &[&str] = &["ultimo", "ultima", "last"];

/// Verbs that ask for a click on something shown.
const CARRIERS: &[&str] = &[
    "apri",
    "aprimi",
    "apre",
    "clicca",
    "click",
    "premi",
    "guarda",
    "guardiamo",
    "vediamo",
    "vedere",
    "partire",
    "riproduci",
    "play",
    "metti",
    "seleziona",
    "scegli",
    "open",
    "watch",
    "select",
    "vai",
    "go",
];

/// Words that carry no part of a title or a name.
const EMPTY: &[&str] = &[
    "il",
    "lo",
    "la",
    "i",
    "gli",
    "le",
    "l",
    "un",
    "una",
    "uno",
    "di",
    "del",
    "della",
    "dello",
    "dei",
    "delle",
    "da",
    "dal",
    "a",
    "al",
    "alla",
    "su",
    "sul",
    "sulla",
    "in",
    "con",
    "per",
    "e",
    "ed",
    "che",
    "quello",
    "quella",
    "questo",
    "questa",
    "fammi",
    "fai",
    "mi",
    "me",
    "the",
    "of",
    "by",
    "to",
    "on",
    "that",
    "this",
    "one",
    "per",
    "favore",
    "video",
    "videos",
    "filmato",
    "link",
    "risultato",
    "risultati",
    "result",
    "results",
    "canale",
    "channel",
    "pulsante",
    "bottone",
    "button",
    "file",
    "titolo",
    "intitolato",
    "chiamato",
    "numero",
];

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn noun_of(w: &[String]) -> Noun {
    let has = |list: &[&str]| w.iter().any(|t| list.contains(&t.as_str()));
    if has(&["scheda", "tab", "schede"]) {
        Noun::Tab
    } else if has(&["video", "videos", "filmato", "clip"]) {
        Noun::Video
    } else if has(&[
        "risultato",
        "risultati",
        "link",
        "collegamento",
        "articolo",
        "notizia",
        "prodotto",
        "post",
        "immagine",
        "foto",
        "result",
        "results",
    ]) {
        Noun::Link
    } else if has(&[
        "file",
        "documento",
        "cartella",
        "elemento",
        "voce",
        "riga",
        "mail",
        "email",
        "messaggio",
        "conversazione",
        "chat",
        "contatto",
        "evento",
        "folder",
        "item",
        "row",
        "message",
    ]) {
        Noun::File
    } else if has(&["pulsante", "bottone", "button"]) {
        Noun::Button
    } else {
        Noun::Any
    }
}

/// The items a noun can mean, as indexes, in reading order, each thing once
/// (a video is two links: its picture and its title).
fn candidates(items: &[Item], noun: Noun) -> Vec<usize> {
    let long = |i: &Item, n: usize| i.kind == Kind::Link && i.text.chars().count() >= n;
    let pick = |keep: &dyn Fn(&Item) -> bool| -> Vec<usize> {
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (k, item) in items.iter().enumerate() {
            if !keep(item) {
                continue;
            }
            let key: String = item.text.to_lowercase().chars().take(40).collect();
            if !seen.contains(&key) {
                seen.push(key);
                out.push(k);
            }
        }
        out
    };
    let first_of = |tries: &[&dyn Fn(&Item) -> bool]| -> Vec<usize> {
        tries
            .iter()
            .map(|keep| pick(*keep))
            .find(|found| !found.is_empty())
            .unwrap_or_default()
    };
    match noun {
        // A video's link carries its title, and often its channel and its
        // length: it is long. A channel's name is short.
        Noun::Video => first_of(&[&|i| long(i, 25), &|i| long(i, 12)]),
        Noun::Link => first_of(&[&|i| long(i, 12), &|i| i.kind == Kind::Link]),
        // A row of a list or a table; a web app often draws its rows as links.
        Noun::File => first_of(&[&|i| i.kind == Kind::ListItem, &|i| long(i, 12)]),
        Noun::Button => first_of(&[&|i| i.kind == Kind::Button]),
        Noun::Tab => Vec::new(),
        Noun::Any => first_of(&[&|i| long(i, 15), &|i| i.kind == Kind::ListItem, &|i| {
            i.kind == Kind::Link
        }]),
    }
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The same word, give or take what speech recognition does to a name:
/// "contrus" for "contro", "Cotti" for "Cotto".
fn same_word(said: &str, shown: &str) -> bool {
    if said == shown {
        return true;
    }
    let (n, m) = (said.chars().count(), shown.chars().count());
    if n.min(m) < 4 {
        return false;
    }
    let allowed = if n.min(m) >= 8 { 2 } else { 1 };
    distance(said, shown) <= allowed
}

/// The item whose text holds the words said, if they point at one clearly
/// enough. `asked`: the command had a verb or a noun that asks for a click;
/// a title merely read aloud has to match on more words.
fn by_text(w: &[String], items: &[Item], noun: Noun, asked: bool) -> Option<usize> {
    let said: Vec<&str> = w
        .iter()
        .map(String::as_str)
        .filter(|t| !EMPTY.contains(t) && !CARRIERS.contains(t))
        .collect();
    let (need_hits, need_share) = if asked { (1, 0.6) } else { (3, 0.7) };
    if said.is_empty() || (!asked && said.len() < 3) {
        return None;
    }
    // One word is a name only when it is long enough not to be a filler.
    if said.len() == 1 && said[0].chars().count() < 4 {
        return None;
    }
    let pool: Vec<usize> = match noun {
        Noun::Video | Noun::Link | Noun::File | Noun::Button => {
            let c = candidates(items, noun);
            if c.is_empty() {
                (0..items.len()).collect()
            } else {
                c
            }
        }
        _ => (0..items.len()).collect(),
    };
    let mut best: Option<(usize, usize, usize)> = None; // (hits, text length, index)
    for k in pool {
        let shown = words(&items[k].text);
        let hits = said
            .iter()
            .filter(|t| shown.iter().any(|u| same_word(t, u)))
            .count();
        if hits < need_hits || (hits as f32) < need_share * said.len() as f32 {
            continue;
        }
        let len = items[k].text.chars().count();
        // More words found wins; then the longer text (a title that names
        // its author, not the author's own link); then the first on screen.
        let better = match best {
            None => true,
            Some((h, l, _)) => hits > h || (hits == h && asked && len > l && noun != Noun::Any),
        };
        if better {
            best = Some((hits, len, k));
        }
    }
    best.map(|(_, _, k)| k)
}

/// What the words point at on the screen, if they point at anything.
/// `items` must be in reading order.
pub fn point(spoken: &str, items: &[Item]) -> Option<Pointed> {
    let w = words(spoken);
    if w.is_empty() {
        return None;
    }
    let noun = noun_of(&w);
    let carrier = w.iter().any(|t| CARRIERS.contains(&t.as_str()));
    let nth = w
        .iter()
        .find_map(|t| ORDINALS.iter().find(|(o, _)| o == t).map(|(_, n)| *n));
    let last = w.iter().any(|t| LAST.contains(&t.as_str()));
    if noun == Noun::Tab {
        // Ctrl+1..8 are the first eight tabs, Ctrl+9 is the last one.
        return match (nth, last) {
            (Some(n), _) if n < 8 => Some(Pointed::Keys(format!("ctrl+{}", n + 1))),
            (None, true) => Some(Pointed::Keys("ctrl+9".into())),
            _ => None,
        };
    }
    // "Il primo" alone counts nothing; with a thing named, or a verb that
    // asks for a click, it counts the things on the screen.
    if (nth.is_some() || last) && (noun != Noun::Any || carrier) {
        let found = candidates(items, noun);
        let k = match nth {
            Some(n) => found.get(n).copied(),
            None => found.last().copied(),
        };
        return k.map(Pointed::Item);
    }
    by_text(&w, items, noun, carrier || noun != Noun::Any).map(Pointed::Item)
}

/// A file, an icon or a row opens with two clicks; one only selects it.
/// "Seleziona il secondo file" stays one click.
pub fn opens_with_two_clicks(spoken: &str, item: &Item) -> bool {
    const OPEN: &[&str] = &["apri", "aprimi", "open", "avvia", "lancia", "partire"];
    item.kind == Kind::ListItem && words(spoken).iter().any(|t| OPEN.contains(&t.as_str()))
}

/// The list as the planner reads it: one numbered line per item.
pub fn lines(items: &[Item], max: usize) -> Vec<String> {
    items
        .iter()
        .take(max)
        .enumerate()
        .map(|(k, i)| {
            let text: String = i.text.chars().take(90).collect();
            format!("[{}] {}: {}", k + 1, i.kind.label(), text)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, kind: Kind, left: i32, top: i32, w: i32, h: i32) -> Item {
        Item {
            text: text.into(),
            kind,
            left,
            top,
            right: left + w,
            bottom: top + h,
        }
    }

    /// A YouTube results page, as UI Automation gives it: the side menu,
    /// the filters, then per video its picture, its title (with channel and
    /// length in the name) and its channel.
    fn youtube() -> Vec<Item> {
        let video = |n: i32, title: &str, channel: &str| {
            let top = 200 + n * 220;
            vec![
                item(title, Kind::Link, 300, top, 360, 200),
                item(title, Kind::Link, 680, top + 4, 600, 44),
                item(channel, Kind::Link, 680, top + 80, 160, 20),
            ]
        };
        let mut v = vec![
            item("Home", Kind::Link, 20, 120, 80, 30),
            item("Shorts", Kind::Link, 20, 160, 80, 30),
            item("Iscrizioni", Kind::Link, 20, 200, 80, 30),
            item("Tutti", Kind::Button, 300, 130, 60, 30),
            item("Video", Kind::Button, 370, 130, 60, 30),
        ];
        v.extend(video(
            0,
            "Gatti che suonano il pianoforte di Animali Buffi 2 anni fa 4 minuti",
            "Animali Buffi",
        ));
        v.extend(video(
            1,
            "Fable 5 contro 5 su 6 casi reali di Michele Cotti 3 giorni fa 18 minuti",
            "Michele Cotti",
        ));
        v.extend(video(
            2,
            "Come addestrare un gatto in dieci minuti di Vita da Gatti 1 mese fa",
            "Vita da Gatti",
        ));
        in_reading_order(v)
    }

    fn text_of(spoken: &str, items: &[Item]) -> Option<String> {
        match point(spoken, items)? {
            Pointed::Item(k) => Some(items[k].text.chars().take(12).collect()),
            Pointed::Keys(k) => Some(k),
        }
    }

    #[test]
    fn reading_order_is_rows_then_columns() {
        let v = in_reading_order(vec![
            item("c", Kind::Link, 500, 104, 100, 40),
            item("d", Kind::Link, 10, 300, 100, 40),
            item("a", Kind::Link, 10, 100, 100, 40),
            item("b", Kind::Link, 250, 96, 100, 40),
        ]);
        let order: Vec<&str> = v.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(order, ["a", "b", "c", "d"]);
    }

    // The user's own words, 2026-11-10: each went to Claude for 2.4-4.9 s
    // and came back as a new search, a scroll to the top, or nothing.
    #[test]
    fn a_counted_video_is_the_one_on_the_screen() {
        let yt = youtube();
        assert_eq!(
            text_of("Fammi vedere il primo video.", &yt).as_deref(),
            Some("Gatti che su")
        );
        assert_eq!(
            text_of("fai partire il secondo video", &yt).as_deref(),
            Some("Fable 5 cont")
        );
        assert_eq!(
            text_of("apri l'ultimo", &yt).as_deref(),
            Some("Come addestr")
        );
        // Heard in English by the recognizer, the place and the thing still stand.
        assert_eq!(
            text_of("Yeah, up to primo video.", &yt).as_deref(),
            Some("Gatti che su")
        );
        assert_eq!(text_of("apri il nono video", &yt), None, "there are three");
        // No thing named and no verb: "il primo" counts nothing.
        assert_eq!(text_of("il primo", &yt), None);
    }

    #[test]
    fn a_video_is_found_by_its_author_or_its_title() {
        let yt = youtube();
        // The video, not the channel's own link.
        assert_eq!(
            text_of("Apri il video di Michele Cotti.", &yt).as_deref(),
            Some("Fable 5 cont")
        );
        // A title read aloud, with the recognizer's own spelling.
        assert_eq!(
            text_of("Fable 5 contrus 5 su 6 casi reali.", &yt).as_deref(),
            Some("Fable 5 cont")
        );
        assert_eq!(
            text_of("guarda quello sul pianoforte", &yt).as_deref(),
            Some("Gatti che su")
        );
        // Words that are on no item point at nothing.
        assert_eq!(text_of("apri il video di Mario Rossi", &yt), None);
        assert_eq!(text_of("che tempo fa domani a Milano", &yt), None);
        // Two words of a title, read with no verb, are not enough to click.
        assert_eq!(text_of("gatti buffi", &yt), None);
    }

    #[test]
    fn tabs_go_by_their_keys_and_files_by_the_list() {
        assert_eq!(
            point("vai alla terza scheda", &[]),
            Some(Pointed::Keys("ctrl+3".into()))
        );
        assert_eq!(
            point("ultima scheda", &[]),
            Some(Pointed::Keys("ctrl+9".into()))
        );
        let files = in_reading_order(vec![
            item("Documenti", Kind::Other, 10, 100, 100, 20),
            item("preventivo.pdf", Kind::ListItem, 300, 100, 300, 20),
            item("foto.jpg", Kind::ListItem, 300, 124, 300, 20),
        ]);
        assert_eq!(
            text_of("apri il secondo file", &files).as_deref(),
            Some("foto.jpg")
        );
        assert_eq!(
            text_of("apri il file preventivo", &files).as_deref(),
            Some("preventivo.p")
        );
    }

    #[test]
    fn a_file_opens_with_two_clicks_a_link_with_one() {
        let file = item("preventivo.pdf", Kind::ListItem, 0, 0, 100, 20);
        let link = item("Gatti che suonano il pianoforte", Kind::Link, 0, 0, 100, 20);
        assert!(opens_with_two_clicks("apri il secondo file", &file));
        assert!(!opens_with_two_clicks("seleziona il secondo file", &file));
        assert!(!opens_with_two_clicks("apri il primo video", &link));
    }

    // A mail list, a table: the rows are things to count too.
    #[test]
    fn rows_of_any_list_are_counted() {
        let mail = in_reading_order(vec![
            item("Posta in arrivo", Kind::Other, 0, 100, 150, 20),
            item(
                "Mario Rossi Preventivo aggiornato 10:32",
                Kind::ListItem,
                200,
                100,
                600,
                40,
            ),
            item(
                "Anna Bianchi Riunione di domani 09:15",
                Kind::ListItem,
                200,
                144,
                600,
                40,
            ),
            item("Rispondi", Kind::Button, 900, 60, 80, 30),
        ]);
        assert_eq!(
            text_of("apri la seconda mail", &mail).as_deref(),
            Some("Anna Bianchi")
        );
        assert_eq!(
            text_of("apri il messaggio di Mario Rossi", &mail).as_deref(),
            Some("Mario Rossi ")
        );
        assert_eq!(
            text_of("clicca su rispondi", &mail).as_deref(),
            Some("Rispondi")
        );
    }

    #[test]
    fn the_planner_reads_numbered_lines() {
        let yt = youtube();
        let l = lines(&yt, 4);
        assert_eq!(l.len(), 4);
        assert_eq!(l[0], "[1] link: Home");
        assert!(l.iter().all(|x| x.chars().count() <= 110));
    }
}
