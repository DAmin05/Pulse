//! Languages a briefing can be read in: the main language of (nearly) every
//! country, wherever DeepL can translate into it.
//!
//! Voices: ElevenLabs Flash v2.5 covers 32 of them, Eleven v3 about 70 more
//! (the account's model list is checked at runtime; the sets below are the
//! fallback). Languages neither covers are read by the browser's own voice when
//! the device has one, and shown as a transcript otherwise.
//!
//! Not offered because DeepL can't translate into them yet: Amharic (Ethiopia),
//! Somali, Khmer, Lao, Sinhala, Kinyarwanda, Tigrinya, Dzongkha, Dhivehi.

pub struct Language {
    /// ISO 639-1 (as articles are tagged).
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

/// Sorted by English name.
pub const LANGUAGES: &[Language] = &[
    lang("af", "Afrikaans", "Afrikaans", "AF", "af-ZA"),
    lang("sq", "Albanian", "Shqip", "SQ", "sq-AL"),
    lang("ar", "Arabic", "العربية", "AR", "ar-SA"),
    lang("hy", "Armenian", "Հայերեն", "HY", "hy-AM"),
    lang("az", "Azerbaijani", "Azərbaycanca", "AZ", "az-AZ"),
    lang("be", "Belarusian", "Беларуская", "BE", "be-BY"),
    lang("bn", "Bengali", "বাংলা", "BN", "bn-BD"),
    lang("bs", "Bosnian", "Bosanski", "BS", "bs-BA"),
    lang("bg", "Bulgarian", "Български", "BG", "bg-BG"),
    lang("my", "Burmese", "မြန်မာ", "MY", "my-MM"),
    lang("ca", "Catalan", "Català", "CA", "ca-ES"),
    lang("zh", "Chinese", "中文", "ZH-HANS", "zh-CN"),
    lang("hr", "Croatian", "Hrvatski", "HR", "hr-HR"),
    lang("cs", "Czech", "Čeština", "CS", "cs-CZ"),
    lang("da", "Danish", "Dansk", "DA", "da-DK"),
    lang("nl", "Dutch", "Nederlands", "NL", "nl-NL"),
    lang("en", "English", "English", "EN-US", "en-US"),
    lang("et", "Estonian", "Eesti", "ET", "et-EE"),
    lang("tl", "Filipino", "Filipino", "TL", "fil-PH"),
    lang("fi", "Finnish", "Suomi", "FI", "fi-FI"),
    lang("fr", "French", "Français", "FR", "fr-FR"),
    lang("ka", "Georgian", "ქართული", "KA", "ka-GE"),
    lang("de", "German", "Deutsch", "DE", "de-DE"),
    lang("el", "Greek", "Ελληνικά", "EL", "el-GR"),
    lang("gn", "Guarani", "Avañe'ẽ", "GN", "gn-PY"),
    lang("ht", "Haitian Creole", "Kreyòl ayisyen", "HT", "ht-HT"),
    lang("ha", "Hausa", "Hausa", "HA", "ha-NG"),
    lang("he", "Hebrew", "עברית", "HE", "he-IL"),
    lang("hi", "Hindi", "हिन्दी", "HI", "hi-IN"),
    lang("hu", "Hungarian", "Magyar", "HU", "hu-HU"),
    lang("is", "Icelandic", "Íslenska", "IS", "is-IS"),
    lang("id", "Indonesian", "Bahasa Indonesia", "ID", "id-ID"),
    lang("it", "Italian", "Italiano", "IT", "it-IT"),
    lang("ja", "Japanese", "日本語", "JA", "ja-JP"),
    lang("kk", "Kazakh", "Қазақ тілі", "KK", "kk-KZ"),
    lang("ko", "Korean", "한국어", "KO", "ko-KR"),
    lang("ky", "Kyrgyz", "Кыргызча", "KY", "ky-KG"),
    lang("lv", "Latvian", "Latviešu", "LV", "lv-LV"),
    lang("lt", "Lithuanian", "Lietuvių", "LT", "lt-LT"),
    lang("lb", "Luxembourgish", "Lëtzebuergesch", "LB", "lb-LU"),
    lang("mk", "Macedonian", "Македонски", "MK", "mk-MK"),
    lang("mg", "Malagasy", "Malagasy", "MG", "mg-MG"),
    lang("ms", "Malay", "Bahasa Melayu", "MS", "ms-MY"),
    lang("mt", "Maltese", "Malti", "MT", "mt-MT"),
    lang("mn", "Mongolian", "Монгол", "MN", "mn-MN"),
    lang("ne", "Nepali", "नेपाली", "NE", "ne-NP"),
    lang("no", "Norwegian", "Norsk", "NB", "nb-NO"),
    lang("ps", "Pashto", "پښتو", "PS", "ps-AF"),
    lang("fa", "Persian", "فارسی", "FA", "fa-IR"),
    lang("pl", "Polish", "Polski", "PL", "pl-PL"),
    lang("pt", "Portuguese", "Português", "PT-BR", "pt-BR"),
    lang("ro", "Romanian", "Română", "RO", "ro-RO"),
    lang("ru", "Russian", "Русский", "RU", "ru-RU"),
    lang("sr", "Serbian", "Српски", "SR", "sr-RS"),
    lang("st", "Sesotho", "Sesotho", "ST", "st-LS"),
    lang("tn", "Setswana", "Setswana", "TN", "tn-BW"),
    lang("sk", "Slovak", "Slovenčina", "SK", "sk-SK"),
    lang("sl", "Slovenian", "Slovenščina", "SL", "sl-SI"),
    lang("es", "Spanish", "Español", "ES", "es-ES"),
    lang("sw", "Swahili", "Kiswahili", "SW", "sw-KE"),
    lang("sv", "Swedish", "Svenska", "SV", "sv-SE"),
    lang("tg", "Tajik", "Тоҷикӣ", "TG", "tg-TJ"),
    lang("ta", "Tamil", "தமிழ்", "TA", "ta-LK"),
    lang("th", "Thai", "ไทย", "TH", "th-TH"),
    lang("tr", "Turkish", "Türkçe", "TR", "tr-TR"),
    lang("tk", "Turkmen", "Türkmençe", "TK", "tk-TM"),
    lang("uk", "Ukrainian", "Українська", "UK", "uk-UA"),
    lang("ur", "Urdu", "اردو", "UR", "ur-PK"),
    lang("uz", "Uzbek", "Oʻzbekcha", "UZ", "uz-UZ"),
    lang("vi", "Vietnamese", "Tiếng Việt", "VI", "vi-VN"),
    lang("zu", "Zulu", "isiZulu", "ZU", "zu-ZA"),
];

/// Languages ElevenLabs Flash v2.5 speaks (fallback when the account's model
/// list can't be read).
pub const FLASH_V2_5: &[&str] = &[
    "en", "ja", "zh", "de", "hi", "fr", "ko", "pt", "it", "es", "id", "nl", "tr", "tl", "pl", "sv",
    "bg", "ro", "ar", "cs", "el", "fi", "hr", "ms", "sk", "da", "ta", "uk", "ru", "hu", "no", "vi",
];

/// Languages Eleven v3 speaks (same fallback role).
pub const V3: &[&str] = &[
    "af", "ar", "hy", "as", "az", "be", "bn", "bs", "bg", "ca", "ceb", "ny", "hr", "cs", "da",
    "nl", "en", "et", "tl", "fi", "fr", "gl", "ka", "de", "el", "gu", "ha", "he", "hi", "hu", "is",
    "id", "ga", "it", "ja", "jv", "kn", "kk", "ky", "ko", "lv", "ln", "lt", "lb", "mk", "ms", "ml",
    "zh", "mr", "ne", "no", "ps", "fa", "pl", "pt", "pa", "ro", "ru", "sr", "sd", "sk", "sl", "so",
    "es", "sw", "sv", "ta", "te", "th", "tr", "uk", "ur", "vi", "cy",
];

pub fn find(code: &str) -> Option<&'static Language> {
    let base = base(code);
    LANGUAGES.iter().find(|l| l.code == base)
}

/// Normalizes a language tag to our code: "pt-BR" → "pt", Norwegian Bokmål
/// "nb" → "no", Filipino "fil" → "tl".
pub fn base(code: &str) -> &str {
    let base = code.split(['-', '_']).next().unwrap_or(code);
    if base.eq_ignore_ascii_case("nb") || base.eq_ignore_ascii_case("nn") {
        "no"
    } else if base.eq_ignore_ascii_case("fil") {
        "tl"
    } else {
        base
    }
}

/// Whether text tagged `text_lang` is already in `target`.
pub fn same(text_lang: &str, target: &str) -> bool {
    base(text_lang).eq_ignore_ascii_case(base(target))
}

/// The code ElevenLabs uses for a language.
pub fn elevenlabs_code(code: &str) -> &str {
    if code == "tl" { "fil" } else { code }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn regional_and_alternate_codes_resolve() {
        assert_eq!(find("pt-BR").map(|l| l.deepl), Some("PT-BR"));
        assert_eq!(find("nb").map(|l| l.code), Some("no"));
        assert_eq!(find("fil").map(|l| l.code), Some("tl"));
        assert_eq!(elevenlabs_code("tl"), "fil");
        assert!(find("xx").is_none());
        assert!(same("en", "en-GB"));
        assert!(!same("es", "pt"));
    }

    #[test]
    fn table_is_consistent() {
        let codes: HashSet<_> = LANGUAGES.iter().map(|l| l.code).collect();
        assert_eq!(codes.len(), LANGUAGES.len(), "duplicate codes");
        let names: Vec<_> = LANGUAGES.iter().map(|l| l.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "keep the table sorted by English name");
        for l in LANGUAGES {
            assert!(
                l.bcp47.contains('-'),
                "{}: region in the browser tag",
                l.code
            );
        }
        // speech.rs treats all but the last three as Multilingual v2's list.
        assert_eq!(FLASH_V2_5[29..], ["hu", "no", "vi"]);
        // Every Flash language is offered.
        for code in FLASH_V2_5 {
            assert!(
                codes.contains(code),
                "{code} is spoken by Flash v2.5 but not offered"
            );
        }
    }
}
