//! Text matching for songs from outside the library, the same way music-lookup / spotify.py matched them:
//! any alphabet (Cyrillic is transliterated), accents dropped, version words and "feat." ignored.

use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

static VERSION_TAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+-\s+.*(remaster|version|edit|mix|mono|stereo|live|demo|bonus|deluxe|anniversary).*$").unwrap()
});
static VERSION_PAREN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[(\[][^)\]]*(feat|with|remaster|version|edit|deluxe|bonus|explicit|anniversary)[^)\]]*[)\]]").unwrap()
});
static FEAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bfeat\.?.*$|\bft\..*$").unwrap());
static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\W_]+").unwrap());
static TAIL_OR_PAREN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+-\s+.*$|\s*[(\[].*?[)\]]").unwrap());
pub static EDITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(deluxe|legacy|anniversary|expanded|remaster(ed)?|edition|bonus|special|collector|live|mono|stereo|super|version|reissue)\b").unwrap()
});
/// Words of an edition with more (or other) songs than the album as it came out.
static EXPANDED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(deluxe|legacy|anniversary|expanded|bonus|special|collector'?s?|live|super|complete|box)\b").unwrap()
});
/// Words of the same album again: remastered (also "Re-Mastered"), reissued, in mono.
static REMASTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(re-?master(ed)?|reissued?|mono|stereo|version|edition)\b").unwrap());

/// How far an edition is from the album as it first came out: 0 the album itself ("Sticky Fingers"), 1 the same
/// songs again ("(Remastered)", "(2009 Re-Mastered)"), 2 an edition with more or other songs ("(Super Deluxe)",
/// "(50th Anniversary Edition)", "(Live)").
pub fn edition_tier(title: &str) -> u8 {
    if EXPANDED.is_match(title) {
        2
    } else if REMASTER.is_match(title) || EDITION.is_match(title) {
        1
    } else {
        0
    }
}

pub static JUNK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(demos?|outtakes?|rarities|karaoke|tribute|interviews?|broadcast|bootleg|remix(es)?|made famous|in the style of|cover versions?)\b").unwrap()
});
pub const VERSION_WORDS: &[&str] = &[
    "live",
    "remix",
    "karaoke",
    "instrumental",
    "cover",
    "acoustic",
    "demo",
    "nightcore",
    "sped up",
    "slowed",
    "tribute",
    "8 bit",
    "8-bit",
    "reprise",
    "rehearsal",
];

/// "Beyoncé" → "beyonce"; every script kept; version / feat. parts and punctuation dropped.
pub fn norm(s: &str) -> String {
    let s: String = s.nfkd().filter(|c| !is_combining_mark(*c)).collect::<String>().to_lowercase();
    let s = VERSION_TAIL.replace(&s, "");
    let s = VERSION_PAREN.replace_all(&s, "");
    let s = FEAT.replace(&s, "");
    let s = s.replace('&', "and");
    NON_WORD.replace_all(&s, "").into_owned()
}

pub fn translit(s: &str) -> String {
    let mut o = String::new();
    for c in s.to_lowercase().chars() {
        let t = match c {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'ё' | 'э' | 'є' => "e",
            'ж' => "zh",
            'з' => "z",
            'и' | 'і' => "i",
            'й' | 'ы' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "kh",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' | 'ь' => "",
            'ю' => "yu",
            'я' => "ya",
            'ї' => "yi",
            'ґ' => "g",
            _ => {
                o.push(c);
                continue;
            }
        };
        o.push_str(t);
    }
    o
}

/// Folds common transliteration differences: aquarium/akvarium, krematorij/krematoriy, maxim/maksim.
pub fn loose(s: &str) -> String {
    let mut s = s.to_string();
    for (x, y) in
        [("qu", "kv"), ("ph", "f"), ("kh", "h"), ("x", "ks"), ("w", "v"), ("j", "y"), ("c", "k"), ("ii", "i"), ("iy", "i")]
    {
        s = s.replace(x, y);
    }
    s
}

pub fn clean_title(name: &str) -> String {
    let c = TAIL_OR_PAREN.replace_all(name, "").trim().to_string();
    if c.is_empty() { name.to_string() } else { c }
}

pub fn clean_album(name: &str) -> String {
    clean_title(name)
}

/// Title key: "Movin' Out (Anthony's Song) - 2008 Remaster" → "movinout".
pub fn tk(title: &str) -> String {
    norm(&clean_title(title))
}
/// Artist key, any alphabet.
pub fn ak(name: &str) -> String {
    loose(&norm(&translit(name)))
}
/// Album key.
pub fn alk(name: &str) -> String {
    norm(&clean_album(name))
}

/// The first-named of artists credited together ("Андрей Макаревич & Папоротник", "A и группа 'B'", "A feat. B",
/// "A, B", "A x B"), which is often the only name a catalog has the record under; None for one name.
pub fn lead_artist(name: &str) -> Option<String> {
    const JOINS: [&str; 17] = [
        " & ", " and ", " и ", " feat. ", " feat ", " ft. ", " featuring ", " x ", " × ", ", ", " / ", " + ", " with ",
        " vs. ", " vs ", " при участии ", " при уч. ",
    ];
    let lower = name.to_lowercase();
    if lower.len() != name.len() {
        return None; // (a name whose lowercase is a different length: not split, rather than split wrongly)
    }
    let at = JOINS.iter().filter_map(|j| lower.find(j)).min()?;
    let lead = name[..at].trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '«' || c == '»').trim();
    (lead.chars().count() >= 2 && lead != name.trim()).then(|| lead.to_string())
}

/// "Various Artists" (Deezer's name for compilations and records with guests on most songs), in any alphabet.
pub fn is_various(name: &str) -> bool {
    matches!(ak(name).as_str(), "variousartists" | "various" | "va" | "raznyeispolniteli" | "sbornik")
}

pub fn same_artist(a: &str, b: &str) -> bool {
    let (a, b) = (ak(a), ak(b));
    !a.is_empty() && !b.is_empty() && (a == b || (a.len().min(b.len()) >= 4 && (a.contains(&b) || b.contains(&a))))
}

/// Does what you typed appear in these fields (in any alphabet)?
pub fn mentions(query: &str, fields: &[&str]) -> bool {
    let hay: String = fields.iter().map(|f| format!("{} {}", norm(&translit(f)), norm(f))).collect::<Vec<_>>().join(" ");
    norm(&translit(query)).split_whitespace().all(|w| hay.contains(w))
}

/// Same artist? Exact, contained, or close enough after folding transliteration differences.
pub fn names_match(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (a, b) = (loose(a), loose(b));
    if a == b || (a.len().min(b.len()) >= 4 && (a.contains(&b) || b.contains(&a))) {
        return true;
    }
    a.chars().count().min(b.chars().count()) >= 5 && ratio(&a, &b) >= 0.8
}

/// How alike two strings are, 0..1 (like Python's SequenceMatcher.ratio, close enough for these thresholds).
pub fn ratio(a: &str, b: &str) -> f64 {
    strsim::normalized_levenshtein(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn like_music_lookup() {
        assert_eq!(norm("Beyoncé"), "beyonce");
        assert_eq!(tk("Movin' Out (Anthony's Song) - 2008 Remaster"), "movinout");
        assert_eq!(norm("Song (feat. Someone)"), "song");
        assert_eq!(norm("Simon & Garfunkel"), "simonandgarfunkel");
        assert_eq!(norm("Место"), "место");
        assert_eq!(ak("Аквариум"), ak("Akvarium"));
        assert!(same_artist("The Beatles", "Beatles"));
        assert!(!same_artist("Muse", "Blur"));
        assert!(same_artist("Abba", "Black Sabbath")); // as in music-lookup: a 4-letter name inside another counts
        assert!(mentions("talking heads", &["Talking Heads", "Remain in Light"]));
        assert!(names_match("krematoriy", "krematorij"));
        assert_eq!(lead_artist("Андрей Макаревич & Папоротник").as_deref(), Some("Андрей Макаревич"));
        assert_eq!(lead_artist("Андрей Макаревич и группа 'Папоротник'").as_deref(), Some("Андрей Макаревич"));
        assert_eq!(lead_artist("Daft Punk feat. Pharrell Williams").as_deref(), Some("Daft Punk"));
        assert_eq!(lead_artist("Simon & Garfunkel").as_deref(), Some("Simon"));
        assert_eq!(lead_artist("Аквариум"), None);
        assert_eq!(lead_artist("AC/DC"), None);
        assert!(same_artist("Андрей Макаревич & Папоротник", "Андрей Макаревич"));
        assert!(is_various("Various Artists") && is_various("Разные исполнители") && !is_various("Various Cruelties"));
        assert!(EDITION.is_match("Remain in Light (Deluxe Version)"));
        for (t, tier) in [
            ("Sticky Fingers", 0),
            ("Sticky Fingers (Remastered)", 1),
            ("Sticky Fingers (2009 Re-Mastered)", 1),
            ("Let It Bleed (Mono Version)", 1),
            ("Sticky Fingers (Super Deluxe)", 2),
            ("Let It Bleed (50th Anniversary Edition)", 2),
            ("Exile on Main St. (Deluxe Version)", 2),
            ("Get Yer Ya-Ya's Out! (Live)", 2),
        ] {
            assert_eq!(edition_tier(t), tier, "{t}");
        }
        assert!(JUNK.is_match("Karaoke - Talking Heads"));
    }
}
