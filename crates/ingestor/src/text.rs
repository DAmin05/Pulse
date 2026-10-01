//! Turning feed HTML snippets into clean plain text.

/// Tags whose boundaries separate words; replaced by a space.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "br",
    "div",
    "li",
    "ul",
    "ol",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "tr",
    "td",
    "th",
    "blockquote",
    "figure",
    "figcaption",
    "img",
    "hr",
    "section",
    "article",
];

/// Strips tags (dropping `<script>`/`<style>` bodies), decodes entities and
/// collapses whitespace.
pub fn clean(input: &str) -> String {
    let stripped = strip_tags(input);
    let decoded = html_escape::decode_html_entities(&stripped);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tag_and_after = &rest[start..];
        // Only `<` followed by a letter, `/`, `!` or `?` opens markup.
        let opens_tag = tag_and_after[1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
        if !opens_tag {
            out.push('<');
            rest = &tag_and_after[1..];
            continue;
        }
        let Some(end) = tag_and_after.find('>') else {
            // Unterminated `<`: it's text, not markup.
            out.push_str(tag_and_after);
            return out;
        };
        let inner = &tag_and_after[1..end];
        let closing = inner.starts_with('/');
        let name: String = inner
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &tag_and_after[end + 1..];

        if !closing && (name == "script" || name == "style") {
            // ASCII lowercasing keeps byte offsets intact.
            let close = format!("</{name}");
            match rest.to_ascii_lowercase().find(&close) {
                Some(pos) => {
                    rest = &rest[pos..];
                    rest = rest.find('>').map_or("", |gt| &rest[gt + 1..]);
                }
                None => rest = "",
            }
            out.push(' ');
        } else if BLOCK_TAGS.contains(&name.as_str()) {
            out.push(' ');
        }
    }
    out.push_str(rest);
    out
}

/// Truncates to at most `max` characters on a word boundary, adding `…`.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > cut.len() / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_and_decodes() {
        let html = r#"<p>Fuerte sismo en <b>M&eacute;xico</b></p><p>Sin v&#xED;ctimas</p>
            <script>alert("x")</script><img src="a.jpg"/>"#;
        assert_eq!(clean(html), "Fuerte sismo en México Sin víctimas");
    }

    #[test]
    fn keeps_bare_angle_brackets() {
        assert_eq!(clean("2 < 3 and 5 > 4"), "2 < 3 and 5 > 4");
    }

    #[test]
    fn handles_multibyte_text() {
        assert_eq!(clean("<div>東京で地震</div>"), "東京で地震");
    }

    #[test]
    fn truncates_on_word_boundary() {
        assert_eq!(truncate("one two three four", 12), "one two…");
        assert_eq!(truncate("short", 12), "short");
        assert_eq!(truncate("日本語のテキスト", 5), "日本語の…");
    }
}
