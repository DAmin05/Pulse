//! Languages a briefing can be read in: those both translation (DeepL) and
//! speech (ElevenLabs Flash v2.5) support.

pub struct Language {
    /// ISO 639-1, as articles are tagged.
    pub code: &'static str,
    pub name: &'static str,
    /// Endonym, for the picker.
    pub native: &'static str,
    /// DeepL `target_lang` (regional variants where the bare code is deprecated).
    pub deepl: &'static str,
    /// BCP 47 tag for the browser's speech synthesis fallback.
    pub bcp47: &'static str,
}

const fn lang(
    code: &'static str,
    name: &'static str,
    native: &'static str,
    deepl: &'static str,
    bcp47: &'static str,
) -> Language {
    Language {
        code,
        name,
        native,
        deepl,
        bcp47,
    }
}

pub const LANGUAGES: &[Language] = &[
    lang("en", "English", "English", "EN-US", "en-US"),
    lang("es", "Spanish", "Español", "ES", "es-ES"),
    lang("fr", "French", "Français", "FR", "fr-FR"),
    lang("de", "German", "Deutsch", "DE", "de-DE"),
    lang("it", "Italian", "Italiano", "IT", "it-IT"),
    lang("pt", "Portuguese", "Português", "PT-BR", "pt-BR"),
    lang("nl", "Dutch", "Nederlands", "NL", "nl-NL"),
    lang("pl", "Polish", "Polski", "PL", "pl-PL"),
    lang("sv", "Swedish", "Svenska", "SV", "sv-SE"),
    lang("da", "Danish", "Dansk", "DA", "da-DK"),
    lang("no", "Norwegian", "Norsk", "NB", "nb-NO"),
    lang("fi", "Finnish", "Suomi", "FI", "fi-FI"),
    lang("cs", "Czech", "Čeština", "CS", "cs-CZ"),
    lang("sk", "Slovak", "Slovenčina", "SK", "sk-SK"),
    lang("ro", "Romanian", "Română", "RO", "ro-RO"),
    lang("bg", "Bulgarian", "Български", "BG", "bg-BG"),
    lang("el", "Greek", "Ελληνικά", "EL", "el-GR"),
    lang("hu", "Hungarian", "Magyar", "HU", "hu-HU"),
    lang("uk", "Ukrainian", "Українська", "UK", "uk-UA"),
    lang("ru", "Russian", "Русский", "RU", "ru-RU"),
    lang("tr", "Turkish", "Türkçe", "TR", "tr-TR"),
    lang("ar", "Arabic", "العربية", "AR", "ar-SA"),
    lang("id", "Indonesian", "Bahasa Indonesia", "ID", "id-ID"),
    lang("ja", "Japanese", "日本語", "JA", "ja-JP"),
    lang("ko", "Korean", "한국어", "KO", "ko-KR"),
    lang("zh", "Chinese", "中文", "ZH-HANS", "zh-CN"),
];

pub fn find(code: &str) -> Option<&'static Language> {
    let base = base(code);
    LANGUAGES.iter().find(|l| l.code == base)
}

/// "pt-BR" → "pt"; Norwegian Bokmål "nb" → "no".
pub fn base(code: &str) -> &str {
    let base = code.split(['-', '_']).next().unwrap_or(code);
    if base.eq_ignore_ascii_case("nb") || base.eq_ignore_ascii_case("nn") {
        "no"
    } else {
        base
    }
}

/// Whether text tagged `text_lang` is already in `target`.
pub fn same(text_lang: &str, target: &str) -> bool {
    base(text_lang).eq_ignore_ascii_case(base(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regional_and_norwegian_codes_resolve() {
        assert_eq!(find("pt-BR").map(|l| l.deepl), Some("PT-BR"));
        assert_eq!(find("nb").map(|l| l.code), Some("no"));
        assert!(find("xx").is_none());
        assert!(same("en", "en-GB"));
        assert!(!same("es", "pt"));
    }
}
