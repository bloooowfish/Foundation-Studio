use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::rmp::locator::try_resolve_remakeplace_source_dir;

pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:21062";
pub const DEFAULT_GAME_PATH: &str =
    r"D:\SquareEnix\FINAL FANTASY XIV - A Realm Reborn\game\ffxiv_dx11.exe";
pub const DEFAULT_RMP_DIR: &str = r"D:\ReMakePlace_Latest";
pub const DEFAULT_FOUNDATION_DIR: &str = r"E:\FF14\Repos\PrivateTest\Foundation";
pub const DEFAULT_GLOBAL_SHORTCUT: &str = "Ctrl+Shift+H";
pub const DAWNTRAIL_EXPANSION_ID: i32 = 5;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StudioSettings {
    pub server_url: String,
    pub game_path: String,
    pub username: String,
    pub password: String,
    pub language: i32,
    pub expansion_level: i32,
    pub extra_arguments: String,
    pub include_frontier_host: bool,
    pub close_on_launch: bool,
    pub open_dashboard_on_launch: bool,
    pub remakeplace_path: String,
    pub foundation_root: String,
    pub watched_preset_dirs: Vec<String>,
    pub auto_apply_watched_presets: bool,
    pub global_shortcut: String,
    pub always_on_top: bool,
    pub minimize_to_tray_on_close: bool,
    #[serde(default = "default_ui_locale")]
    pub ui_locale: String,
}

fn default_ui_locale() -> String {
    "auto".to_string()
}

impl Default for StudioSettings {
    fn default() -> Self {
        let game_path = if Path::new(DEFAULT_GAME_PATH).is_file() {
            DEFAULT_GAME_PATH.to_string()
        } else {
            String::new()
        };

        let remakeplace_path = try_resolve_remakeplace_source_dir(DEFAULT_RMP_DIR)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let foundation_root = detect_default_foundation_root()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut watched_preset_dirs = Vec::new();
        let default_save = Path::new(DEFAULT_RMP_DIR).join("MakePlace").join("Save");
        if default_save.is_dir() {
            watched_preset_dirs.push(default_save.to_string_lossy().into_owned());
        }

        Self {
            server_url: DEFAULT_SERVER_URL.to_string(),
            game_path,
            username: String::new(),
            password: String::new(),
            language: 0,
            expansion_level: DAWNTRAIL_EXPANSION_ID,
            extra_arguments: String::new(),
            include_frontier_host: true,
            close_on_launch: false,
            open_dashboard_on_launch: true,
            remakeplace_path,
            foundation_root,
            watched_preset_dirs,
            auto_apply_watched_presets: false,
            global_shortcut: DEFAULT_GLOBAL_SHORTCUT.to_string(),
            always_on_top: false,
            minimize_to_tray_on_close: true,
            ui_locale: default_ui_locale(),
        }
    }
}

/// On-disk JSON representation matching DPAPI secret handling without ever persisting plaintext passwords.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct StudioSettingsFile {
    server_url: String,
    game_path: String,
    username: String,
    protected_password: String,
    language: i32,
    expansion_level: i32,
    extra_arguments: String,
    include_frontier_host: bool,
    close_on_launch: bool,
    open_dashboard_on_launch: bool,
    remakeplace_path: String,
    foundation_root: String,
    watched_preset_dirs: Vec<String>,
    auto_apply_watched_presets: bool,
    global_shortcut: String,
    always_on_top: bool,
    minimize_to_tray_on_close: bool,
    ui_locale: String,
}

impl Default for StudioSettingsFile {
    fn default() -> Self {
        Self::from_settings(&StudioSettings::default())
    }
}

impl StudioSettingsFile {
    fn from_settings(settings: &StudioSettings) -> Self {
        Self {
            server_url: settings.server_url.clone(),
            game_path: settings.game_path.clone(),
            username: settings.username.clone(),
            protected_password: protect_password(&settings.password),
            language: settings.language.clamp(0, 3),
            expansion_level: normalize_expansion_level(settings.expansion_level),
            extra_arguments: settings.extra_arguments.clone(),
            include_frontier_host: settings.include_frontier_host,
            close_on_launch: settings.close_on_launch,
            open_dashboard_on_launch: settings.open_dashboard_on_launch,
            remakeplace_path: settings.remakeplace_path.clone(),
            foundation_root: settings.foundation_root.clone(),
            watched_preset_dirs: dedup_nonempty_paths(&settings.watched_preset_dirs),
            auto_apply_watched_presets: settings.auto_apply_watched_presets,
            global_shortcut: settings.global_shortcut.trim().to_string(),
            always_on_top: settings.always_on_top,
            minimize_to_tray_on_close: settings.minimize_to_tray_on_close,
            ui_locale: crate::i18n::normalize_preference(&settings.ui_locale),
        }
    }

    fn to_settings(&self) -> StudioSettings {
        let mut settings = StudioSettings {
            server_url: if self.server_url.trim().is_empty() {
                DEFAULT_SERVER_URL.to_string()
            } else {
                self.server_url.clone()
            },
            game_path: self.game_path.clone(),
            username: self.username.clone(),
            password: unprotect_password(&self.protected_password, None),
            language: self.language.clamp(0, 3),
            expansion_level: normalize_expansion_level(self.expansion_level),
            extra_arguments: self.extra_arguments.clone(),
            include_frontier_host: self.include_frontier_host,
            close_on_launch: self.close_on_launch,
            open_dashboard_on_launch: self.open_dashboard_on_launch,
            remakeplace_path: self.remakeplace_path.clone(),
            foundation_root: self.foundation_root.clone(),
            watched_preset_dirs: dedup_nonempty_paths(&self.watched_preset_dirs),
            auto_apply_watched_presets: self.auto_apply_watched_presets,
            global_shortcut: if self.global_shortcut.trim().is_empty() {
                DEFAULT_GLOBAL_SHORTCUT.to_string()
            } else {
                self.global_shortcut.trim().to_string()
            },
            always_on_top: self.always_on_top,
            minimize_to_tray_on_close: self.minimize_to_tray_on_close,
            ui_locale: crate::i18n::normalize_preference(&self.ui_locale),
        };
        fill_detected_defaults(&mut settings);
        settings
    }
}

/// Read-only schema for `%AppData%\KawariLauncher\settings.json` first-run import.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct LegacyLauncherSettingsFile {
    server_url: String,
    game_path: String,
    username: String,
    protected_password: String,
    password: Option<String>,
    language: i32,
    expansion_level: i32,
    extra_arguments: String,
    include_frontier_host: bool,
    close_on_launch: bool,
    open_dashboard_on_launch: Option<bool>,
    open_overlay_on_launch: Option<bool>,
    re_make_place_path: String,
    preset_watch_directory: Option<String>,
    overlay_hotkey: Option<String>,
}

impl Default for LegacyLauncherSettingsFile {
    fn default() -> Self {
        Self {
            server_url: DEFAULT_SERVER_URL.to_string(),
            game_path: String::new(),
            username: String::new(),
            protected_password: String::new(),
            password: None,
            language: 0,
            expansion_level: DAWNTRAIL_EXPANSION_ID,
            extra_arguments: String::new(),
            include_frontier_host: true,
            close_on_launch: false,
            open_dashboard_on_launch: None,
            open_overlay_on_launch: None,
            re_make_place_path: String::new(),
            preset_watch_directory: None,
            overlay_hotkey: None,
        }
    }
}

pub fn normalize_expansion_level(level: i32) -> i32 {
    if (0..=DAWNTRAIL_EXPANSION_ID).contains(&level) {
        level
    } else {
        DAWNTRAIL_EXPANSION_ID
    }
}

pub fn default_studio_settings_path() -> PathBuf {
    appdata_dir().join("FoundationStudio").join("settings.json")
}

pub fn default_legacy_kawari_studio_settings_path() -> PathBuf {
    appdata_dir().join("KawariStudio").join("settings.json")
}

pub fn default_legacy_launcher_settings_path() -> PathBuf {
    appdata_dir().join("KawariLauncher").join("settings.json")
}

fn appdata_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn local_appdata_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(appdata_dir)
}

pub fn managed_runtime_root() -> PathBuf {
    local_appdata_dir().join("FoundationStudio").join("runtime")
}

pub fn managed_foundation_dir() -> PathBuf {
    managed_runtime_root().join("foundation")
}

pub fn managed_rmp_bridge_dir() -> PathBuf {
    managed_runtime_root().join("rmp-bridge")
}

fn is_foundation_root_dir(dir: &Path) -> bool {
    dir.join("config.yaml").is_file()
        || dir.join("kawari-run.exe").is_file()
        || dir
            .join("target")
            .join("debug")
            .join("kawari-run.exe")
            .is_file()
        || dir
            .join("target")
            .join("release")
            .join("kawari-run.exe")
            .is_file()
}

pub fn detect_default_foundation_root() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent();
        for _ in 0..8 {
            let Some(dir) = cur else {
                break;
            };
            let sibling = dir.join("Foundation");
            if is_foundation_root_dir(&sibling) {
                return Some(sibling);
            }
            cur = dir.parent();
        }
    }
    let default_path = Path::new(DEFAULT_FOUNDATION_DIR);
    if is_foundation_root_dir(default_path) {
        return Some(default_path.to_path_buf());
    }
    let managed = managed_foundation_dir();
    if is_foundation_root_dir(&managed) {
        Some(managed)
    } else {
        None
    }
}

pub fn load_settings() -> StudioSettings {
    load_settings_from_paths(
        &default_studio_settings_path(),
        Some(&default_legacy_kawari_studio_settings_path()),
        Some(&default_legacy_launcher_settings_path()),
    )
}

pub fn load_settings_from(
    studio_path: &Path,
    legacy_launcher_path: Option<&Path>,
) -> StudioSettings {
    load_settings_from_paths(studio_path, None, legacy_launcher_path)
}

pub fn load_settings_from_paths(
    studio_path: &Path,
    legacy_studio_path: Option<&Path>,
    legacy_launcher_path: Option<&Path>,
) -> StudioSettings {
    if studio_path.is_file()
        && let Ok(raw) = fs::read_to_string(studio_path)
        && let Ok(file) = serde_json::from_str::<StudioSettingsFile>(&raw)
    {
        return file.to_settings();
    }

    if let Some(prev_studio_path) = legacy_studio_path
        && prev_studio_path.is_file()
        && let Ok(raw) = fs::read_to_string(prev_studio_path)
        && let Ok(file) = serde_json::from_str::<StudioSettingsFile>(&raw)
    {
        let migrated = file.to_settings();
        // Persist migrated settings into FoundationStudio path while leaving old file untouched.
        let _ = save_settings_to(&migrated, studio_path);
        return migrated;
    }

    if let Some(legacy_path) = legacy_launcher_path
        && legacy_path.is_file()
        && let Ok(raw) = fs::read_to_string(legacy_path)
        && let Ok(migrated) = import_legacy_launcher_json(&raw)
    {
        // Persist migrated settings into FoundationStudio path while leaving legacy file untouched.
        let _ = save_settings_to(&migrated, studio_path);
        return migrated;
    }

    StudioSettings::default()
}

pub fn save_settings(settings: &StudioSettings) -> Result<(), String> {
    save_settings_to(settings, &default_studio_settings_path())
}

pub fn save_settings_to(settings: &StudioSettings, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create settings directory: {e}"))?;
    }
    let file = StudioSettingsFile::from_settings(settings);
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("Failed to serialize settings: {e}"))?;
    fs::write(path, json).map_err(|e| format!("Failed to save settings ({}): {e}", path.display()))
}

pub fn import_legacy_launcher_json(raw: &str) -> Result<StudioSettings, String> {
    let legacy: LegacyLauncherSettingsFile = serde_json::from_str(raw)
        .map_err(|e| format!("Failed to parse legacy launcher settings: {e}"))?;

    let mut settings = StudioSettings {
        server_url: if legacy.server_url.trim().is_empty() {
            DEFAULT_SERVER_URL.to_string()
        } else {
            legacy.server_url
        },
        game_path: legacy.game_path,
        username: legacy.username,
        password: unprotect_password(&legacy.protected_password, legacy.password.as_deref()),
        language: legacy.language.clamp(0, 3),
        expansion_level: normalize_expansion_level(legacy.expansion_level),
        extra_arguments: legacy.extra_arguments,
        include_frontier_host: legacy.include_frontier_host,
        close_on_launch: legacy.close_on_launch,
        open_dashboard_on_launch: legacy
            .open_dashboard_on_launch
            .or(legacy.open_overlay_on_launch)
            .unwrap_or(true),
        remakeplace_path: legacy.re_make_place_path,
        foundation_root: detect_default_foundation_root()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        watched_preset_dirs: legacy
            .preset_watch_directory
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect(),
        auto_apply_watched_presets: false,
        global_shortcut: legacy
            .overlay_hotkey
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_GLOBAL_SHORTCUT.to_string()),
        always_on_top: false,
        minimize_to_tray_on_close: true,
        ui_locale: default_ui_locale(),
    };

    fill_detected_defaults(&mut settings);
    settings.watched_preset_dirs = dedup_nonempty_paths(&settings.watched_preset_dirs);
    Ok(settings)
}

fn fill_detected_defaults(settings: &mut StudioSettings) {
    if settings.game_path.trim().is_empty() && Path::new(DEFAULT_GAME_PATH).is_file() {
        settings.game_path = DEFAULT_GAME_PATH.to_string();
    }
    if settings.remakeplace_path.trim().is_empty()
        && let Some(resolved) = try_resolve_remakeplace_source_dir(DEFAULT_RMP_DIR)
    {
        settings.remakeplace_path = resolved.to_string_lossy().into_owned();
    }
    if settings.foundation_root.trim().is_empty()
        && let Some(root) = detect_default_foundation_root()
    {
        settings.foundation_root = root.to_string_lossy().into_owned();
    }
}

pub fn dedup_nonempty_paths(paths: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in paths {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let norm = trimmed.trim_end_matches(['\\', '/']);
        let candidate = if norm.ends_with(':') {
            format!("{norm}\\")
        } else {
            norm.to_string()
        };
        if !out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&candidate))
        {
            out.push(candidate);
        }
    }
    out
}

/// Encrypts a password using Windows DPAPI (`CurrentUser`, `optionalEntropy = null`) prefixed with `dpapi:`.
pub fn protect_password(password: &str) -> String {
    if password.is_empty() {
        return String::new();
    }

    #[cfg(windows)]
    {
        match dpapi_protect(password.as_bytes()) {
            Ok(bytes) => format!("dpapi:{}", BASE64.encode(bytes)),
            Err(_) => String::new(),
        }
    }

    #[cfg(not(windows))]
    {
        format!("plain:{}", BASE64.encode(password.as_bytes()))
    }
}

/// Decrypts `"dpapi:<base64>"` or `"plain:<base64>"`, falling back to legacy plaintext `Password` when empty.
pub fn unprotect_password(protected: &str, legacy_plaintext: Option<&str>) -> String {
    let trimmed = protected.trim();
    if trimmed.is_empty() {
        return legacy_plaintext.unwrap_or_default().to_string();
    }

    if let Some(b64) = trimmed.strip_prefix("dpapi:") {
        #[cfg(windows)]
        {
            if let Ok(cipher) = BASE64.decode(b64)
                && let Ok(plain) = dpapi_unprotect(&cipher)
                && let Ok(s) = String::from_utf8(plain)
            {
                return s;
            }
        }
        let _ = b64;
        return String::new();
    }

    if let Some(b64) = trimmed.strip_prefix("plain:")
        && let Ok(bytes) = BASE64.decode(b64)
        && let Ok(s) = String::from_utf8(bytes)
    {
        return s;
    }

    String::new()
}

#[cfg(windows)]
fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>, u32> {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};

    let data_in = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr().cast_mut(),
    };
    let mut data_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    unsafe {
        let ok = CryptProtectData(
            &data_in,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            0,
            &mut data_out,
        );
        if ok == 0 {
            return Err(GetLastError());
        }
        let bytes = std::slice::from_raw_parts(data_out.pbData, data_out.cbData as usize).to_vec();
        LocalFree(data_out.pbData as *mut c_void);
        Ok(bytes)
    }
}

#[cfg(windows)]
fn dpapi_unprotect(cipher: &[u8]) -> Result<Vec<u8>, u32> {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};

    let data_in = CRYPT_INTEGER_BLOB {
        cbData: cipher.len() as u32,
        pbData: cipher.as_ptr().cast_mut(),
    };
    let mut data_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    unsafe {
        let ok = CryptUnprotectData(
            &data_in,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            0,
            &mut data_out,
        );
        if ok == 0 {
            return Err(GetLastError());
        }
        let bytes = std::slice::from_raw_parts(data_out.pbData, data_out.cbData as usize).to_vec();
        LocalFree(data_out.pbData as *mut c_void);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_dpapi_password_without_plaintext_on_disk() {
        let dir =
            std::env::temp_dir().join(format!("foundation_studio_cfg_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");

        let settings = StudioSettings {
            server_url: "http://127.0.0.1:21062".to_string(),
            username: "alice".to_string(),
            password: "super-secret-password-!@#".to_string(),
            open_dashboard_on_launch: false,
            ui_locale: "ja-JP".to_string(),
            ..Default::default()
        };

        save_settings_to(&settings, &path).unwrap();
        let disk_raw = fs::read_to_string(&path).unwrap();
        assert!(!disk_raw.contains("super-secret-password-!@#"));
        assert!(disk_raw.contains("dpapi:") || disk_raw.contains("plain:"));

        let loaded = load_settings_from(&path, None);
        assert_eq!(loaded.username, "alice");
        assert_eq!(loaded.password, "super-secret-password-!@#");
        assert!(!loaded.open_dashboard_on_launch);
        assert_eq!(loaded.ui_locale, "ja");
        assert_eq!(StudioSettings::default().ui_locale, "auto");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_legacy_kawari_launcher_settings_without_modifying_source() {
        let dir =
            std::env::temp_dir().join(format!("foundation_studio_migrate_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let studio_path = dir.join("FoundationStudio").join("settings.json");
        let legacy_path = dir.join("KawariLauncher").join("settings.json");
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();

        let protected = protect_password("my-legacy-pass");
        let legacy_json = serde_json::json!({
            "ServerUrl": "http://192.168.1.50:21062",
            "GamePath": "D:\\FFXIV\\game\\ffxiv_dx11.exe",
            "Username": "bob",
            "ProtectedPassword": protected,
            "Language": 1,
            "ExpansionLevel": 4,
            "OpenOverlayOnLaunch": false,
            "OverlayHotkey": "Ctrl+Alt+H",
            "PresetWatchDirectory": "D:\\Presets\\Room1\\",
            "Password": ""
        });
        let legacy_text = serde_json::to_string_pretty(&legacy_json).unwrap();
        fs::write(&legacy_path, &legacy_text).unwrap();

        let migrated = load_settings_from(&studio_path, Some(&legacy_path));
        assert_eq!(migrated.server_url, "http://192.168.1.50:21062");
        assert_eq!(migrated.username, "bob");
        assert_eq!(migrated.password, "my-legacy-pass");
        assert_eq!(migrated.language, 1);
        assert_eq!(migrated.expansion_level, 4);
        assert!(!migrated.open_dashboard_on_launch);
        assert_eq!(migrated.global_shortcut, "Ctrl+Alt+H");
        assert_eq!(migrated.watched_preset_dirs, vec![r"D:\Presets\Room1"]);
        assert!(studio_path.is_file());
        // Original legacy file remained untouched.
        assert_eq!(fs::read_to_string(&legacy_path).unwrap(), legacy_text);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_previous_kawari_studio_settings_to_foundation_studio() {
        let dir =
            std::env::temp_dir().join(format!("foundation_studio_from_ks_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let foundation_studio_path = dir.join("FoundationStudio").join("settings.json");
        let prev_studio_path = dir.join("KawariStudio").join("settings.json");
        let prev = StudioSettings {
            username: "migrated-user".to_string(),
            password: "migrated-password".to_string(),
            watched_preset_dirs: vec![r"D:\Presets\Custom".to_string()],
            always_on_top: true,
            ..Default::default()
        };
        save_settings_to(&prev, &prev_studio_path).unwrap();
        let prev_raw = fs::read_to_string(&prev_studio_path).unwrap();

        let loaded =
            load_settings_from_paths(&foundation_studio_path, Some(&prev_studio_path), None);
        assert_eq!(loaded.username, "migrated-user");
        assert_eq!(loaded.password, "migrated-password");
        assert_eq!(loaded.watched_preset_dirs, vec![r"D:\Presets\Custom"]);
        assert!(loaded.always_on_top);
        assert!(foundation_studio_path.is_file());
        assert_eq!(fs::read_to_string(&prev_studio_path).unwrap(), prev_raw);

        let _ = fs::remove_dir_all(&dir);
    }
}
