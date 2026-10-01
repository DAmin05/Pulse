//! Language codes: everything is normalized to ISO 639-1 ("en", "pt", "zh").

use isolang::Language;

/// Language code for "undetermined".
pub const UNDETERMINED: &str = "und";

/// Normalizes a BCP 47 tag ("pt-BR", "en_US", "zh-Hans") or an ISO 639-3 code
/// ("spa", "zho") to ISO 639-1.
pub fn normalize(tag: &str) -> Option<String> {
    let primary = tag.trim().split(['-', '_']).next()?.to_ascii_lowercase();
    match primary.len() {
        2 => Language::from_639_1(&primary).map(|_| primary),
        3 => Language::from_639_3(&primary)
            .and_then(|l| l.to_639_1())
            .map(str::to_owned),
        _ => None,
    }
}

/// Detects the language of `text`, returning `None` unless detection is reliable.
pub fn detect(text: &str) -> Option<String> {
    let info = whatlang::detect(text)?;
    if !info.is_reliable() {
        return None;
    }
    normalize(info.lang().code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_tags() {
        assert_eq!(normalize("en-US").as_deref(), Some("en"));
        assert_eq!(normalize("pt_BR").as_deref(), Some("pt"));
        assert_eq!(normalize("zh-Hans").as_deref(), Some("zh"));
        assert_eq!(normalize("spa").as_deref(), Some("es"));
        assert_eq!(normalize("ell").as_deref(), Some("el"));
        assert_eq!(normalize("xx"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn detects_clear_text() {
        let es =
            "El gobierno anunció nuevas medidas económicas para enfrentar la inflación este año";
        assert_eq!(detect(es).as_deref(), Some("es"));
        let de =
            "Die Bundesregierung hat am Mittwoch neue Maßnahmen gegen die Inflation beschlossen";
        assert_eq!(detect(de).as_deref(), Some("de"));
    }
}
