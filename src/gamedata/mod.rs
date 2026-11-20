use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use physis::Language;
use physis::resource::SqPackResource;
use serde::Serialize;

use crate::i18n::{normalize_game_data_locale, normalize_locale_tag, resolve_game_data_locale};
use crate::settings::StudioSettings;

const ITEM_COL_SINGULAR: usize = 0;
const ITEM_COL_NAME: usize = 9;
const STAIN_COL_INLINE_NAME: usize = 5;
const STAIN_COL_NAME: usize = 6;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GameDataInfo {
    pub game_dir_valid: bool,
    pub available_languages: Vec<String>,
    pub requested_preference: String,
    pub requested_locale: String,
    pub effective_locale: String,
    pub requested_in_sqpack: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedGameNames {
    pub effective_locale: String,
    pub available_languages: Vec<String>,
    pub items: HashMap<String, String>,
    pub stains: HashMap<String, String>,
    pub by_name: HashMap<String, String>,
}

#[derive(Debug, Default)]
struct LangTable {
    items: HashMap<u32, String>,
    stains: HashMap<u8, String>,
}

#[derive(Debug, Default)]
struct CacheInner {
    available_langs: HashMap<PathBuf, Arc<Vec<String>>>,
    tables: HashMap<(PathBuf, String), Arc<LangTable>>,
    en_reverse: HashMap<PathBuf, Arc<HashMap<String, u32>>>,
}

#[derive(Debug, Default)]
pub struct GameDataCache {
    inner: Mutex<CacheInner>,
}

/// Resolves the retail `game/` directory containing `sqpack/ffxiv` from a user-configured exe,
/// `game/` directory, or top-level install root.
pub fn resolve_game_data_dir(configured_game_path: &str) -> Option<PathBuf> {
    let trimmed = configured_game_path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }
    let candidate = Path::new(trimmed);

    let direct_candidates: Vec<PathBuf> = if candidate.is_file() {
        candidate
            .parent()
            .into_iter()
            .map(Path::to_path_buf)
            .collect()
    } else if candidate.is_dir() {
        vec![candidate.to_path_buf(), candidate.join("game")]
    } else {
        Vec::new()
    };

    direct_candidates
        .into_iter()
        .find(|dir| dir.join("sqpack").join("ffxiv").is_dir())
}

/// Removes raw `physis` unknown SeString tokens such as `<?:31>` (soft hyphen) and trims spaces.
pub fn strip_sestring_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<?:") {
        out.push_str(&rest[..start]);
        let after_prefix = &rest[start + 3..];
        if let Some(end) = after_prefix.find('>') {
            rest = &after_prefix[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn language_to_code(lang: Language) -> Option<&'static str> {
    match lang {
        Language::Japanese => Some("ja"),
        Language::English => Some("en"),
        Language::German => Some("de"),
        Language::French => Some("fr"),
        Language::Korean => Some("ko"),
        _ => None,
    }
}

fn code_to_language(code: &str) -> Language {
    match normalize_locale_tag(code) {
        "ja" => Language::Japanese,
        "en" => Language::English,
        "de" => Language::German,
        "fr" => Language::French,
        "ko" => Language::Korean,
        _ => Language::English,
    }
}

fn pick_effective_locale(
    requested: &str,
    client_language: i32,
    available: &[String],
) -> (String, bool) {
    if available.is_empty() {
        return (requested.to_string(), false);
    }
    if available.iter().any(|l| l == requested) {
        return (requested.to_string(), true);
    }
    let client_code = crate::i18n::client_language_to_locale(client_language);
    if available.iter().any(|l| l == client_code) {
        return (client_code.to_string(), false);
    }
    if available.iter().any(|l| l == "en") {
        return ("en".to_string(), false);
    }
    (available[0].clone(), false)
}

fn extract_sheet_text(row: &physis::excel::Row, primary_col: usize, fallback_col: usize) -> String {
    if let Some(col) = row.columns.get(primary_col)
        && let Some(se) = col.into_string()
    {
        let clean = strip_sestring_tags(&se.to_string());
        if !clean.is_empty() {
            return clean;
        }
    }
    if let Some(col) = row.columns.get(fallback_col)
        && let Some(se) = col.into_string()
    {
        return strip_sestring_tags(&se.to_string());
    }
    String::new()
}

fn load_available_languages(game_dir: &Path) -> Vec<String> {
    let dir_str = game_dir.to_string_lossy().into_owned();
    let mut res = SqPackResource::from_existing(&dir_str);
    let Ok(exh) = res.read_excel_sheet_header("Item") else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut langs = Vec::new();
    for lang in exh.languages {
        if let Some(code) = language_to_code(lang)
            && seen.insert(code)
        {
            langs.push(code.to_string());
        }
    }
    langs
}

fn load_lang_table(game_dir: &Path, locale: &str) -> LangTable {
    let dir_str = game_dir.to_string_lossy().into_owned();
    let mut res = SqPackResource::from_existing(&dir_str);
    let lang = code_to_language(locale);
    let mut table = LangTable::default();

    if let Ok(item_exh) = res.read_excel_sheet_header("Item")
        && let Ok(item_sheet) = res.read_excel_sheet(&item_exh, "Item", lang)
    {
        for (row_id, subrows) in &item_sheet {
            if let Some((_, row)) = subrows.first() {
                let name = extract_sheet_text(row, ITEM_COL_NAME, ITEM_COL_SINGULAR);
                if !name.is_empty() {
                    table.items.insert(row_id, name);
                }
            }
        }
    }

    if let Ok(stain_exh) = res.read_excel_sheet_header("Stain")
        && let Ok(stain_sheet) = res.read_excel_sheet(&stain_exh, "Stain", lang)
    {
        for (row_id, subrows) in &stain_sheet {
            let Ok(stain_id) = u8::try_from(row_id) else {
                continue;
            };
            if stain_id == 0 {
                continue;
            }
            if let Some((_, row)) = subrows.first() {
                let name = extract_sheet_text(row, STAIN_COL_NAME, STAIN_COL_INLINE_NAME);
                if !name.is_empty() {
                    table.stains.insert(stain_id, name);
                }
            }
        }
    }

    table
}

fn build_reverse_item_map(table: &LangTable) -> HashMap<String, u32> {
    let mut rev = HashMap::with_capacity(table.items.len());
    for (&id, name) in &table.items {
        let key = name.trim().to_ascii_lowercase();
        if !key.is_empty() {
            rev.entry(key).or_insert(id);
        }
    }
    rev
}

impl GameDataCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get_available_languages(&self, game_dir: &Path) -> Arc<Vec<String>> {
        if let Ok(guard) = self.inner.lock()
            && let Some(cached) = guard.available_langs.get(game_dir)
        {
            return Arc::clone(cached);
        }

        let loaded = Arc::new(load_available_languages(game_dir));
        if let Ok(mut guard) = self.inner.lock() {
            let entry = guard
                .available_langs
                .entry(game_dir.to_path_buf())
                .or_insert_with(|| Arc::clone(&loaded));
            return Arc::clone(entry);
        }
        loaded
    }

    fn get_lang_table(&self, game_dir: &Path, locale: &str) -> Arc<LangTable> {
        let key = (game_dir.to_path_buf(), locale.to_string());
        if let Ok(guard) = self.inner.lock()
            && let Some(cached) = guard.tables.get(&key)
        {
            return Arc::clone(cached);
        }

        let loaded = Arc::new(load_lang_table(game_dir, locale));
        if let Ok(mut guard) = self.inner.lock() {
            if locale == "en" {
                guard
                    .en_reverse
                    .entry(game_dir.to_path_buf())
                    .or_insert_with(|| Arc::new(build_reverse_item_map(&loaded)));
            }
            let entry = guard
                .tables
                .entry(key)
                .or_insert_with(|| Arc::clone(&loaded));
            return Arc::clone(entry);
        }
        loaded
    }

    fn get_english_reverse_map(&self, game_dir: &Path) -> Arc<HashMap<String, u32>> {
        if let Ok(guard) = self.inner.lock()
            && let Some(cached) = guard.en_reverse.get(game_dir)
        {
            return Arc::clone(cached);
        }

        let en_table = self.get_lang_table(game_dir, "en");
        let built = Arc::new(build_reverse_item_map(&en_table));
        if let Ok(mut guard) = self.inner.lock() {
            let entry = guard
                .en_reverse
                .entry(game_dir.to_path_buf())
                .or_insert_with(|| Arc::clone(&built));
            return Arc::clone(entry);
        }
        built
    }

    pub fn info(&self, settings: &StudioSettings) -> GameDataInfo {
        let requested_preference = normalize_game_data_locale(&settings.game_data_locale);
        let requested_locale = resolve_game_data_locale(
            &requested_preference,
            settings.language,
            &settings.ui_locale,
        )
        .to_string();

        let Some(game_dir) = resolve_game_data_dir(&settings.game_path) else {
            return GameDataInfo {
                game_dir_valid: false,
                available_languages: Vec::new(),
                requested_preference,
                requested_locale: requested_locale.clone(),
                effective_locale: requested_locale,
                requested_in_sqpack: false,
            };
        };

        let available = self.get_available_languages(&game_dir);
        let (effective_locale, requested_in_sqpack) =
            pick_effective_locale(&requested_locale, settings.language, &available);

        GameDataInfo {
            game_dir_valid: !available.is_empty(),
            available_languages: available.as_ref().clone(),
            requested_preference,
            requested_locale,
            effective_locale,
            requested_in_sqpack,
        }
    }

    pub fn resolve_names(
        &self,
        settings: &StudioSettings,
        item_ids: &[u32],
        stain_ids: &[u8],
        names: &[String],
    ) -> ResolvedGameNames {
        let info = self.info(settings);
        let Some(game_dir) = resolve_game_data_dir(&settings.game_path) else {
            return ResolvedGameNames {
                effective_locale: info.effective_locale,
                available_languages: info.available_languages,
                items: HashMap::new(),
                stains: HashMap::new(),
                by_name: HashMap::new(),
            };
        };
        if info.available_languages.is_empty() {
            return ResolvedGameNames {
                effective_locale: info.effective_locale,
                available_languages: info.available_languages,
                items: HashMap::new(),
                stains: HashMap::new(),
                by_name: HashMap::new(),
            };
        }

        let table = self.get_lang_table(&game_dir, &info.effective_locale);

        let mut items = HashMap::new();
        for &id in item_ids {
            if id != 0
                && let Some(name) = table.items.get(&id)
            {
                items.insert(id.to_string(), name.clone());
            }
        }

        let mut stains = HashMap::new();
        for &id in stain_ids {
            if id != 0
                && let Some(name) = table.stains.get(&id)
            {
                stains.insert(id.to_string(), name.clone());
            }
        }

        let mut by_name = HashMap::new();
        if !names.is_empty() {
            let reverse = if info.available_languages.iter().any(|l| l == "en") {
                Some(self.get_english_reverse_map(&game_dir))
            } else {
                None
            };
            for raw in names {
                let key = raw.trim().to_ascii_lowercase();
                if key.is_empty() {
                    continue;
                }
                if let Some(ref rev) = reverse
                    && let Some(&item_id) = rev.get(&key)
                    && let Some(localized) = table.items.get(&item_id)
                {
                    by_name.insert(raw.clone(), localized.clone());
                }
            }
        }

        ResolvedGameNames {
            effective_locale: info.effective_locale,
            available_languages: info.available_languages,
            items,
            stains,
            by_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{client_language_to_locale, normalize_game_data_locale};
    use std::fs;

    #[test]
    fn strips_sestring_control_tags() {
        assert_eq!(strip_sestring_tags("Hingan Flooring"), "Hingan Flooring");
        assert_eq!(strip_sestring_tags("  Foo<?:31>Bar  "), "FooBar");
        assert_eq!(strip_sestring_tags("A<?:8>B<?:12>C"), "ABC");
        assert_eq!(strip_sestring_tags("Partial<?:31"), "Partial<?:31");
    }

    #[test]
    fn resolves_game_dir_from_exe_game_or_root() {
        let dir =
            std::env::temp_dir().join(format!("foundation_studio_gamedata_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let game_dir = dir.join("game");
        let sqpack_dir = game_dir.join("sqpack").join("ffxiv");
        fs::create_dir_all(&sqpack_dir).unwrap();
        let exe_path = game_dir.join("ffxiv_dx11.exe");
        fs::write(&exe_path, b"exe").unwrap();

        assert_eq!(
            resolve_game_data_dir(&dir.to_string_lossy()),
            Some(game_dir.clone())
        );
        assert_eq!(
            resolve_game_data_dir(&game_dir.to_string_lossy()),
            Some(game_dir.clone())
        );
        assert_eq!(
            resolve_game_data_dir(&format!("\"{}\"", exe_path.display())),
            Some(game_dir)
        );
        assert_eq!(resolve_game_data_dir(""), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalizes_and_resolves_game_data_locale() {
        assert_eq!(normalize_game_data_locale(""), "follow_client");
        assert_eq!(normalize_game_data_locale("follow_client"), "follow_client");
        assert_eq!(normalize_game_data_locale("follow_ui"), "follow_ui");
        assert_eq!(normalize_game_data_locale("ja-JP"), "ja");
        assert_eq!(normalize_game_data_locale("FR"), "fr");
        assert_eq!(normalize_game_data_locale("unknown"), "follow_client");

        assert_eq!(client_language_to_locale(0), "ja");
        assert_eq!(client_language_to_locale(1), "en");
        assert_eq!(client_language_to_locale(2), "de");
        assert_eq!(client_language_to_locale(3), "fr");
        assert_eq!(client_language_to_locale(99), "en");

        assert_eq!(resolve_game_data_locale("follow_client", 0, "ko"), "ja");
        assert_eq!(resolve_game_data_locale("follow_ui", 0, "fr"), "fr");
        assert_eq!(resolve_game_data_locale("de", 0, "en"), "de");
    }

    #[test]
    fn reads_local_retail_item_and_stain_when_available() {
        let local_game = Path::new(r"D:\SquareEnix\FINAL FANTASY XIV - A Realm Reborn\game");
        if !local_game.join("sqpack").join("ffxiv").is_dir() {
            return;
        }

        let cache = GameDataCache::new();
        let ja_settings = StudioSettings {
            game_path: local_game
                .join("ffxiv_dx11.exe")
                .to_string_lossy()
                .into_owned(),
            language: 0,
            game_data_locale: "follow_client".to_string(),
            ..Default::default()
        };
        let ja_names = cache.resolve_names(
            &ja_settings,
            &[20718],
            &[1],
            &["Hingan Flooring".to_string()],
        );
        assert_eq!(ja_names.effective_locale, "ja");
        let ja_floor = ja_names.items.get("20718").expect("Item 20718 in ja");
        assert!(!ja_floor.is_empty());
        assert_ne!(ja_floor, "Hingan Flooring");
        assert_eq!(ja_names.by_name.get("Hingan Flooring"), Some(ja_floor));
        assert!(
            ja_names.stains.get("1").is_some_and(|s| !s.is_empty()),
            "Expected non-empty Stain 1 name in ja"
        );

        let en_settings = StudioSettings {
            game_data_locale: "en".to_string(),
            ..ja_settings
        };
        let en_names = cache.resolve_names(&en_settings, &[20718], &[1], &[]);
        assert_eq!(en_names.effective_locale, "en");
        assert_eq!(
            en_names.items.get("20718").map(String::as_str),
            Some("Hingan Flooring")
        );
    }
}
