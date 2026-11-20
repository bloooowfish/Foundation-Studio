use std::collections::HashMap;
use std::sync::OnceLock;

pub const SUPPORTED_LOCALES: &[&str] = &["ko", "en", "ja", "de", "fr"];

const EN_JSON: &str = include_str!("../frontend/locales/en.json");
const KO_JSON: &str = include_str!("../frontend/locales/ko.json");
const JA_JSON: &str = include_str!("../frontend/locales/ja.json");
const DE_JSON: &str = include_str!("../frontend/locales/de.json");
const FR_JSON: &str = include_str!("../frontend/locales/fr.json");

static DICTS: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();

fn dictionaries() -> &'static HashMap<&'static str, HashMap<String, String>> {
    DICTS.get_or_init(|| {
        let mut map = HashMap::new();
        for (code, raw) in [
            ("en", EN_JSON),
            ("ko", KO_JSON),
            ("ja", JA_JSON),
            ("de", DE_JSON),
            ("fr", FR_JSON),
        ] {
            let parsed: HashMap<String, String> = serde_json::from_str(raw).unwrap_or_default();
            map.insert(code, parsed);
        }
        map
    })
}

pub fn normalize_locale_tag(tag: &str) -> &'static str {
    let lower = tag.trim().to_ascii_lowercase();
    let primary = lower.split(['-', '_', '.']).next().unwrap_or_default();
    match primary {
        "ko" => "ko",
        "ja" => "ja",
        "de" => "de",
        "fr" => "fr",
        "en" => "en",
        _ => "en",
    }
}

pub fn detect_system_locale() -> &'static str {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
        let mut buf = [0u16; 85];
        let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if len > 1 {
            let tag = String::from_utf16_lossy(&buf[..(len as usize - 1)]);
            return normalize_locale_tag(&tag);
        }
    }

    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(val) = std::env::var(var)
            && !val.trim().is_empty()
        {
            return normalize_locale_tag(&val);
        }
    }

    "en"
}

pub fn resolve_locale(pref: &str) -> &'static str {
    let trimmed = pref.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        detect_system_locale()
    } else {
        normalize_locale_tag(trimmed)
    }
}

pub fn normalize_preference(pref: &str) -> String {
    let trimmed = pref.trim().to_ascii_lowercase();
    if trimmed.is_empty() || trimmed == "auto" {
        return "auto".to_string();
    }
    resolve_locale(&trimmed).to_string()
}

pub fn normalize_game_data_locale(pref: &str) -> String {
    let trimmed = pref.trim().to_ascii_lowercase();
    if trimmed.is_empty() || trimmed == "follow_client" {
        return "follow_client".to_string();
    }
    if trimmed == "follow_ui" {
        return "follow_ui".to_string();
    }
    let primary = trimmed.split(['-', '_', '.']).next().unwrap_or_default();
    match primary {
        "ja" | "en" | "de" | "fr" | "ko" => primary.to_string(),
        _ => "follow_client".to_string(),
    }
}

pub fn client_language_to_locale(language: i32) -> &'static str {
    match language {
        0 => "ja",
        1 => "en",
        2 => "de",
        3 => "fr",
        _ => "en",
    }
}

pub fn resolve_game_data_locale(
    pref: &str,
    client_language: i32,
    ui_locale_pref: &str,
) -> &'static str {
    match normalize_game_data_locale(pref).as_str() {
        "follow_client" => client_language_to_locale(client_language),
        "follow_ui" => resolve_locale(ui_locale_pref),
        other => normalize_locale_tag(other),
    }
}

pub fn tr(locale_pref: &str, key: &str) -> String {
    let code = resolve_locale(locale_pref);
    let dicts = dictionaries();
    if let Some(val) = dicts.get(code).and_then(|d| d.get(key)) {
        return val.clone();
    }
    if let Some(val) = dicts.get("en").and_then(|d| d.get(key)) {
        return val.clone();
    }
    key.to_string()
}

pub fn tr_fmt(locale_pref: &str, key: &str, args: &[(&str, &str)]) -> String {
    let mut text = tr(locale_pref, key);
    for (k, v) in args {
        text = text.replace(&format!("{{{k}}}"), v);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn extract_placeholders(value: &str) -> BTreeSet<String> {
        let mut set = BTreeSet::new();
        let mut start = None;
        for (idx, ch) in value.char_indices() {
            match ch {
                '{' => start = Some(idx + 1),
                '}' => {
                    if let Some(s) = start.take() {
                        set.insert(value[s..idx].to_string());
                    }
                }
                _ => {}
            }
        }
        set
    }

    #[test]
    fn all_locale_files_have_identical_keys_and_placeholders() {
        let raw_files = [
            ("en", EN_JSON),
            ("ko", KO_JSON),
            ("ja", JA_JSON),
            ("de", DE_JSON),
            ("fr", FR_JSON),
        ];

        let mut parsed: Vec<(&str, HashMap<String, String>)> = Vec::new();
        for (code, raw) in raw_files {
            let map: HashMap<String, String> = serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("Invalid JSON in locale {code}.json: {e}"));
            parsed.push((code, map));
        }

        let en_map = &parsed[0].1;
        let en_keys: BTreeSet<&String> = en_map.keys().collect();
        assert!(!en_keys.is_empty());

        for (code, map) in &parsed {
            let keys: BTreeSet<&String> = map.keys().collect();
            let missing: Vec<_> = en_keys.difference(&keys).collect();
            let extra: Vec<_> = keys.difference(&en_keys).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "Locale {code}.json key mismatch: missing={missing:?}, extra={extra:?}"
            );

            for (k, v) in map {
                assert!(
                    !v.trim().is_empty(),
                    "Empty translation for {code}.json key '{k}'"
                );
                let expected_ph = extract_placeholders(&en_map[k]);
                let actual_ph = extract_placeholders(v);
                assert_eq!(
                    actual_ph, expected_ph,
                    "Placeholder mismatch in {code}.json key '{k}': expected {expected_ph:?}, got {actual_ph:?}"
                );
            }
        }
    }

    #[test]
    fn index_html_i18n_keys_exist_in_locale_files() {
        let html = include_str!("../frontend/index.html");
        let en_map: HashMap<String, String> = serde_json::from_str(EN_JSON).unwrap();

        let attrs = [
            "data-i18n=\"",
            "data-i18n-placeholder=\"",
            "data-i18n-title=\"",
            "data-i18n-aria-label=\"",
        ];

        let mut missing = Vec::new();
        for attr in attrs {
            let mut search_from = 0usize;
            while let Some(rel) = html[search_from..].find(attr) {
                let start = search_from + rel + attr.len();
                if let Some(end_rel) = html[start..].find('"') {
                    let key = &html[start..start + end_rel];
                    if !en_map.contains_key(key) {
                        missing.push(key.to_string());
                    }
                    search_from = start + end_rel + 1;
                } else {
                    break;
                }
            }
        }

        assert!(
            missing.is_empty(),
            "Missing keys referenced in frontend/index.html: {missing:?}"
        );
    }
}
