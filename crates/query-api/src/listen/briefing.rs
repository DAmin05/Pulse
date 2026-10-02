//! Story briefings: a short, extractive script to be read aloud.
//!
//! headline · how widely it's covered · 1–3 reports, each one source's summary.
//!
//! Deterministic for a given story state, so the audio cache (keyed by the
//! final text) is hit whenever nothing material changed. Reports come from
//! distinct sources, the articles closest to the story's centroid, preferring
//! ones already in the listener's language (no translation needed) and
//! skipping summaries that repeat each other.

use std::collections::{BTreeMap, HashSet};

use pulse_store::reader::ArticleView;
use serde::Serialize;

use super::langs;

/// Reports at most this long (characters) are kept whole.
const REPORT_MAX: usize = 280;
const REPORTS: usize = 3;
/// Two summaries sharing more than this fraction of words say the same thing.
const REDUNDANT: f64 = 0.5;
/// Ranking bonus (in centroid similarity) for articles already in the target language.
const NATIVE_BONUS: f32 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Headline,
    Coverage,
    Report,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceRef {
    pub article_id: String,
    pub name: String,
    pub url: String,
    pub lang: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Segment {
    pub kind: Kind,
    pub text: String,
    /// Language `text` is in.
    pub lang: String,
    pub source: Option<SourceRef>,
}

pub struct Story<'a> {
    pub headline: &'a str,
    pub headline_lang: &'a str,
    pub source_count: i64,
    pub languages: usize,
    pub articles: &'a [ArticleView],
}

/// Composes a briefing for `target`. With `translatable` false only text
/// already in `target` is used; `None` if the story has none.
pub fn compose(
    story: &Story,
    target: &str,
    translatable: bool,
    max_chars: usize,
) -> Option<Vec<Segment>> {
    let candidates = candidates(story.articles, target, translatable);
    if !translatable && candidates.is_empty() {
        return None;
    }

    // Headline: a native-language title from a central article beats translating.
    let best = candidates.first().map_or(0.0, |c| c.article.score);
    let native_title = candidates
        .iter()
        .find(|c| langs::same(&c.article.lang, target) && c.article.score >= best - 0.1);
    let mut segments = vec![match native_title {
        Some(c) => Segment {
            kind: Kind::Headline,
            text: sentence(&clean(&c.article.title)),
            lang: c.article.lang.clone(),
            source: None,
        },
        None => Segment {
            kind: Kind::Headline,
            text: sentence(&clean(story.headline)),
            lang: story.headline_lang.to_owned(),
            source: None,
        },
    }];
    if translatable || langs::same("en", target) {
        segments.push(Segment {
            kind: Kind::Coverage,
            text: coverage(story.source_count, story.languages),
            lang: "en".into(),
            source: None,
        });
    }

    let mut used: usize = segments.iter().map(|s| s.text.chars().count() + 1).sum();
    let mut picked: Vec<HashSet<String>> = Vec::new();
    for c in &candidates {
        if picked.len() == REPORTS {
            break;
        }
        let words = words(&c.summary);
        if picked.iter().any(|p| jaccard(p, &words) > REDUNDANT) {
            continue;
        }
        let name = speakable_source(&c.article.source_id);
        let room = max_chars.saturating_sub(used + name.chars().count() + 2);
        // Always read at least one report, trimmed to fit.
        let summary = if c.summary.chars().count() <= room {
            c.summary.clone()
        } else if picked.is_empty() && room >= 60 {
            fit(&c.summary, room)
        } else {
            continue;
        };
        let text = format!("{name}: {summary}");
        used += text.chars().count() + 1;
        picked.push(words);
        segments.push(Segment {
            kind: Kind::Report,
            text,
            lang: c.article.lang.clone(),
            source: Some(SourceRef {
                article_id: c.article.id.clone(),
                name,
                url: c.article.url.clone(),
                lang: c.article.lang.clone(),
            }),
        });
    }
    Some(segments)
}

struct Candidate<'a> {
    article: &'a ArticleView,
    summary: String,
    rank: f32,
}

/// Usable articles, one per source (its most central), best first.
fn candidates<'a>(
    articles: &'a [ArticleView],
    target: &str,
    translatable: bool,
) -> Vec<Candidate<'a>> {
    let mut by_source: BTreeMap<&str, Candidate<'a>> = BTreeMap::new();
    for a in articles {
        let native = langs::same(&a.lang, target);
        if a.is_duplicate || !(translatable || native) {
            continue;
        }
        let summary = fit(&clean(&a.summary), REPORT_MAX);
        if summary.chars().count() < 40
            || clean(&a.title).starts_with(summary.trim_end_matches('.'))
        {
            continue;
        }
        let rank = a.score + if native { NATIVE_BONUS } else { 0.0 };
        let better = by_source
            .get(a.source_id.as_str())
            .is_none_or(|c| rank > c.rank || (rank == c.rank && a.id < c.article.id));
        if better {
            by_source.insert(
                &a.source_id,
                Candidate {
                    article: a,
                    summary,
                    rank,
                },
            );
        }
    }
    let mut out: Vec<_> = by_source.into_values().collect();
    out.sort_by(|a, b| {
        b.rank
            .total_cmp(&a.rank)
            .then_with(|| a.article.id.cmp(&b.article.id))
    });
    out
}

/// "40 sources in 7 languages are covering this story." Counts are rounded down
/// to tens past ten, so the text (and cached audio) survives a few more sources.
pub fn coverage(sources: i64, languages: usize) -> String {
    let n = if sources < 10 || sources % 10 == 0 {
        format!("{sources}")
    } else {
        format!("More than {}", sources / 10 * 10)
    };
    let subject = if sources == 1 {
        "1 source is".to_owned()
    } else {
        format!("{n} sources")
    };
    let subject = match (sources == 1, languages > 1) {
        (true, _) => subject,
        (false, true) => format!("{subject} in {languages} languages are"),
        (false, false) => format!("{subject} are"),
    };
    format!("{subject} covering this story.")
}

/// Feed text cleaned up for reading aloud: whitespace collapsed, wire
/// datelines ("LONDON (Reuters) - ") and trailing ellipses dropped.
pub fn clean(text: &str) -> String {
    let mut s: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for sep in [" -- ", " — ", " – ", " - "] {
        if let Some(i) = s.find(sep).filter(|&i| i <= 80) {
            let prefix = &s[..i];
            let upper = prefix.chars().filter(|c| c.is_uppercase()).count();
            let letters = prefix.chars().filter(|c| c.is_alphabetic()).count();
            if prefix.ends_with(')') || (letters >= 3 && upper * 10 >= letters * 7) {
                s = s[i + sep.len()..].to_owned();
                break;
            }
        }
    }
    for tail in ["[…]", "[...]", "...", "…"] {
        if let Some(stripped) = s.strip_suffix(tail) {
            // A cut-off sentence: keep up to the last complete one, if there is one.
            let stripped = stripped.trim_end();
            s = match last_sentence_end(stripped) {
                Some(end) if end >= 40 => stripped[..end].to_owned(),
                _ => stripped.to_owned(),
            };
            break;
        }
    }
    s.trim().to_owned()
}

/// Shortens to at most `max` characters, at a sentence end if possible.
fn fit(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return sentence(text);
    }
    let cut: String = text.chars().take(max).collect();
    match last_sentence_end(&cut) {
        Some(end) if end >= max / 3 => cut[..end].to_owned(),
        _ => {
            let at = cut.rfind(' ').unwrap_or(cut.len());
            format!("{}…", cut[..at].trim_end_matches([',', ';', ':']))
        }
    }
}

/// Byte index just past the last sentence-ending punctuation.
fn last_sentence_end(s: &str) -> Option<usize> {
    s.char_indices()
        .filter(|&(i, c)| {
            matches!(c, '.' | '!' | '?' | '。' | '！' | '？')
                && s[i + c.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map(|(i, c)| i + c.len_utf8())
        .next_back()
}

/// Ensures the text ends like a sentence, so the voice pauses after it.
fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.ends_with(['.', '!', '?', '…', '。', '！', '？', '"', '”', '»']) {
        s.to_owned()
    } else {
        format!("{s}.")
    }
}

/// Outlet names as a newsreader would say them, by source id prefix.
const BRANDS: &[(&str, &str)] = &[
    ("abc-au", "ABC Australia"),
    ("abc-us", "ABC News"),
    ("africanews", "Africanews"),
    ("agencia-brasil", "Agência Brasil"),
    ("aljazeera", "Al Jazeera"),
    ("allafrica", "AllAfrica"),
    ("ansa", "ANSA"),
    ("antara", "Antara"),
    ("arstechnica", "Ars Technica"),
    ("bbc", "BBC"),
    ("cbs", "CBS News"),
    ("clarin", "Clarín"),
    ("cnbc", "CNBC"),
    ("dw", "DW"),
    ("elmundo", "El Mundo"),
    ("elpais", "El País"),
    ("eluniversal", "El Universal"),
    ("engadget", "Engadget"),
    ("euronews", "Euronews"),
    ("europapress", "Europa Press"),
    ("faz", "FAZ"),
    ("folha", "Folha de S.Paulo"),
    ("fox", "Fox News"),
    ("france24", "France 24"),
    ("franceinfo", "Franceinfo"),
    ("g1", "G1"),
    ("guardian", "The Guardian"),
    ("hn", "Hacker News"),
    ("hurriyet", "Hürriyet"),
    ("independent", "The Independent"),
    ("infobae", "Infobae"),
    ("japantimes", "The Japan Times"),
    ("kyivindependent", "The Kyiv Independent"),
    ("lanacion", "La Nación"),
    ("latimes", "the Los Angeles Times"),
    ("lefigaro", "Le Figaro"),
    ("lemonde", "Le Monde"),
    ("liberation", "Libération"),
    ("marketwatch", "MarketWatch"),
    ("meduza", "Meduza"),
    ("moscowtimes", "The Moscow Times"),
    ("nasa", "NASA"),
    ("nature", "Nature"),
    ("ndtv", "NDTV"),
    ("nos", "NOS"),
    ("npr", "NPR"),
    ("nyt", "The New York Times"),
    ("nzz", "NZZ"),
    ("orf", "ORF"),
    ("politico", "Politico"),
    ("pravda-ua", "Ukrainska Pravda"),
    ("publico", "Público"),
    ("radio-canada", "Radio-Canada"),
    ("repubblica", "la Repubblica"),
    ("rfi", "RFI"),
    ("sciencedaily", "ScienceDaily"),
    ("scmp", "the South China Morning Post"),
    ("sky", "Sky News"),
    ("spiegel", "Der Spiegel"),
    ("straitstimes", "The Straits Times"),
    ("sueddeutsche", "Süddeutsche Zeitung"),
    ("tagesschau", "Tagesschau"),
    ("techcrunch", "TechCrunch"),
    ("thehill", "The Hill"),
    ("thehindu", "The Hindu"),
    ("theverge", "The Verge"),
    ("timesofisrael", "The Times of Israel"),
    ("toi", "The Times of India"),
    ("tvn24", "TVN24"),
    ("un", "UN News"),
    ("wapo", "The Washington Post"),
    ("wired", "Wired"),
    ("yonhap", "Yonhap"),
    ("zeit", "Die Zeit"),
    ("20minutos", "20minutos"),
];

/// Feed sections, not part of an outlet's name ("nyt-world" is The New York Times).
const SECTIONS: &[&str] = &[
    "world",
    "top",
    "home",
    "business",
    "tech",
    "science",
    "health",
    "environment",
    "politics",
    "us",
    "uk",
    "intl",
    "news",
    "frontpage",
    "une",
    "latest",
];

/// "nyt-home" → "The New York Times", "bbc-mundo" → "BBC Mundo",
/// "gdelt-translingual:lemonde.fr" → "lemonde.fr"; unknown ids are title-cased.
pub fn speakable_source(id: &str) -> String {
    if let Some((_, domain)) = id.split_once(':') {
        return domain.to_owned();
    }
    let tokens: Vec<&str> = id.split('-').collect();
    for take in [2, 1] {
        if tokens.len() < take {
            continue;
        }
        let prefix = tokens[..take].join("-");
        if let Some((_, brand)) = BRANDS.iter().find(|(p, _)| *p == prefix) {
            // BBC's language services are outlets of their own: BBC Mundo, BBC Afrique.
            return match tokens.get(take) {
                Some(&service)
                    if *brand == "BBC"
                        && !SECTIONS.contains(&service)
                        && langs::find(service).is_none() =>
                {
                    format!(
                        "BBC {}",
                        if service == "turkce" {
                            "Türkçe".into()
                        } else {
                            capitalize(service)
                        }
                    )
                }
                _ => (*brand).to_owned(),
            };
        }
    }
    let mut words = tokens;
    // A trailing language code is for us, not the listener.
    if words.len() > 1 && words.last().is_some_and(|w| langs::find(w).is_some()) {
        words.pop();
    }
    words
        .iter()
        .map(|w| capitalize(w))
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 2)
        .map(str::to_lowercase)
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn article(
        id: &str,
        source: &str,
        lang: &str,
        score: f32,
        title: &str,
        summary: &str,
    ) -> ArticleView {
        ArticleView {
            id: id.into(),
            title: title.into(),
            summary: summary.into(),
            url: format!("https://example.com/{id}"),
            source_id: source.into(),
            lang: lang.into(),
            published_at: Utc::now(),
            is_duplicate: false,
            late: false,
            score,
        }
    }

    fn fixture() -> Vec<ArticleView> {
        vec![
            article(
                "a1",
                "bbc-world",
                "en",
                0.91,
                "Ship sinks off Crete",
                "A cargo ship sank off the coast of Crete on Tuesday after a fire broke out in its engine room, the coast guard said.",
            ),
            article(
                "a2",
                "bbc-world",
                "en",
                0.80,
                "Crete ship: what we know",
                "Everything we know so far about the cargo ship that sank near the Greek island.",
            ),
            article(
                "a3",
                "elpais",
                "es",
                0.88,
                "Se hunde un carguero frente a Creta",
                "Un buque de carga se hundió frente a Creta tras un incendio en la sala de máquinas, según los guardacostas griegos.",
            ),
            article(
                "a4",
                "reuters-world",
                "en",
                0.90,
                "Cargo ship sinks near Crete",
                "ATHENS (Reuters) - A cargo ship sank off the coast of Crete on Tuesday after a fire broke out in its engine room, the coast guard said...",
            ),
            article(
                "a5",
                "dw-de",
                "de",
                0.86,
                "Frachter vor Kreta gesunken",
                "Vor der griechischen Insel Kreta ist ein Frachtschiff gesunken. Alle zwölf Besatzungsmitglieder wurden gerettet.",
            ),
        ]
    }

    fn story(articles: &[ArticleView]) -> Story<'_> {
        Story {
            headline: "Ship sinks off Crete",
            headline_lang: "en",
            source_count: 4,
            languages: 3,
            articles,
        }
    }

    #[test]
    fn english_briefing_reads_distinct_sources_without_repeats() {
        let articles = fixture();
        let b = compose(&story(&articles), "en", true, 700).unwrap();
        let kinds: Vec<_> = b.iter().map(|s| s.kind).collect();
        assert_eq!(kinds[..2], [Kind::Headline, Kind::Coverage]);
        assert_eq!(
            b[1].text,
            "4 sources in 3 languages are covering this story."
        );
        let sources: Vec<_> = b
            .iter()
            .filter_map(|s| s.source.as_ref().map(|r| r.name.as_str()))
            .collect();
        // bbc-world's most central article; Reuters says the same thing, so it's skipped.
        assert_eq!(sources, ["BBC", "El País", "DW"]);
        assert!(b.iter().all(|s| !s.text.contains("Reuters")));
    }

    #[test]
    fn prefers_native_language_text() {
        let articles = fixture();
        let b = compose(&story(&articles), "es", true, 700).unwrap();
        assert_eq!(b[0].text, "Se hunde un carguero frente a Creta.");
        assert_eq!(b[0].lang, "es");
        assert_eq!(
            b[2].source.as_ref().unwrap().lang,
            "es",
            "the Spanish report leads"
        );
    }

    #[test]
    fn without_translation_only_native_text_is_used() {
        let articles = fixture();
        let b = compose(&story(&articles), "de", false, 700).unwrap();
        assert!(b.iter().all(|s| s.lang == "de"), "{b:?}");
        assert!(
            b.iter().all(|s| s.kind != Kind::Coverage),
            "the coverage line is English"
        );
        assert!(compose(&story(&articles), "ja", false, 700).is_none());
    }

    #[test]
    fn respects_the_length_budget_but_reads_one_report() {
        let articles = fixture();
        let b = compose(&story(&articles), "en", true, 160).unwrap();
        let total: usize = b.iter().map(|s| s.text.chars().count() + 1).sum();
        assert!(total <= 170, "{total}: {b:?}");
        assert_eq!(b.iter().filter(|s| s.kind == Kind::Report).count(), 1);
    }

    #[test]
    fn is_deterministic() {
        let articles = fixture();
        let mut reversed = articles.clone();
        reversed.reverse();
        assert_eq!(
            compose(&story(&articles), "fr", true, 700),
            compose(&story(&reversed), "fr", true, 700)
        );
    }

    #[test]
    fn cleans_feed_text() {
        assert_eq!(clean("ATHENS (Reuters) - Ship  sank."), "Ship sank.");
        assert_eq!(
            clean("OKAZAKI, Japan, Oct. 2 (Yonhap) -- South Korea won. It was the second gold..."),
            "South Korea won. It was the second gold"
        );
        assert_eq!(
            clean("Asking AI companies - a great way to pretend."),
            "Asking AI companies - a great way to pretend."
        );
        assert_eq!(
            clean("Short cut off text that ends..."),
            "Short cut off text that ends"
        );
        assert_eq!(
            fit("One two three four five six seven", 15),
            "One two three…"
        );
    }

    #[test]
    fn coverage_rounds_large_counts() {
        assert_eq!(
            coverage(47, 6),
            "More than 40 sources in 6 languages are covering this story."
        );
        assert_eq!(coverage(40, 1), "40 sources are covering this story.");
        assert_eq!(coverage(1, 1), "1 source is covering this story.");
    }

    #[test]
    fn source_names_read_naturally() {
        assert_eq!(speakable_source("france24-fr"), "France 24");
        assert_eq!(speakable_source("thehindu-intl"), "The Hindu");
        assert_eq!(speakable_source("nyt-home"), "The New York Times");
        assert_eq!(speakable_source("bbc-world"), "BBC");
        assert_eq!(speakable_source("bbc-mundo"), "BBC Mundo");
        assert_eq!(speakable_source("bbc-turkce"), "BBC Türkçe");
        assert_eq!(speakable_source("abc-us-intl"), "ABC News");
        assert_eq!(speakable_source("pravda-ua"), "Ukrainska Pravda");
        assert_eq!(speakable_source("some-new-feed-en"), "Some New Feed");
        assert_eq!(
            speakable_source("gdelt-translingual:lemonde.fr"),
            "lemonde.fr"
        );
    }
}
