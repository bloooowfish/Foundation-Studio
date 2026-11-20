pub mod housing;
pub mod i18n;
pub mod kawari;
pub mod presets;
pub mod rmp;
pub mod server;
pub mod settings;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kawari::launch::{
    LaunchOptions, build_launch_arguments, find_running_ffxiv_pids, mask_session_id,
    read_game_version, resolve_game_executable, spawn_game_client,
};
use kawari::uri::{ResolvedEndpoints, normalize_server_url, resolve_endpoints};
use presets::watcher::{
    LocalPresetEntry, PresetWatcherManager, read_local_preset_entry, reveal_in_explorer,
    scan_watched_dirs, write_text_atomic,
};
use rmp::controller::{RmpController, RmpSyncStatus};
use rmp::locator::try_resolve_remakeplace_source_dir;
use serde::Serialize;
use server::{FoundationServerController, FoundationServerStatus, ServerLogEntry};
use settings::{
    StudioSettings, dedup_nonempty_paths, load_settings, normalize_expansion_level, save_settings,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent, Wry};
use tauri_plugin_dialog::{DialogExt, FilePath};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_opener::OpenerExt;

pub struct TrayMenuHandles {
    pub show: MenuItem<Wry>,
    pub pin: MenuItem<Wry>,
    pub quit: MenuItem<Wry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastKeyPayload {
    pub key: String,
    pub params: serde_json::Map<String, serde_json::Value>,
}

pub struct AppState {
    pub settings: Arc<Mutex<StudioSettings>>,
    pub housing: Arc<housing::HousingClientState>,
    pub preset_watcher: Arc<PresetWatcherManager>,
    pub rmp: Arc<RmpController>,
    pub server: Arc<FoundationServerController>,
    pub tray_menu: Arc<Mutex<Option<TrayMenuHandles>>>,
}

fn update_tray_menu_locale(state: &AppState, locale_pref: &str) {
    if let Ok(guard) = state.tray_menu.lock()
        && let Some(handles) = guard.as_ref()
    {
        let _ = handles
            .show
            .set_text(i18n::tr(locale_pref, "tray.show_hide"));
        let _ = handles
            .pin
            .set_text(i18n::tr(locale_pref, "tray.toggle_pin"));
        let _ = handles.quit.set_text(i18n::tr(locale_pref, "tray.quit"));
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckServerResult {
    pub normalized_url: String,
    pub endpoints: ResolvedEndpoints,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchGameResult {
    pub executable_path: String,
    pub masked_arguments: String,
    pub pid: u32,
    pub endpoints: ResolvedEndpoints,
    pub warnings: Vec<String>,
    pub effective_expansion: i32,
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> StudioSettings {
    state.settings.lock().map(|g| g.clone()).unwrap_or_default()
}

#[tauri::command]
fn get_system_locale_cmd() -> String {
    i18n::detect_system_locale().to_string()
}

#[tauri::command]
async fn set_ui_locale_cmd(state: State<'_, AppState>, locale: String) -> Result<String, String> {
    let pref = i18n::normalize_preference(&locale);
    let updated = {
        let mut guard = state
            .settings
            .lock()
            .map_err(|_| "Failed to lock settings".to_string())?;
        guard.ui_locale = pref.clone();
        guard.clone()
    };
    save_settings(&updated)?;
    update_tray_menu_locale(&state, &pref);
    Ok(i18n::resolve_locale(&pref).to_string())
}

#[tauri::command]
fn save_settings_cmd(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: StudioSettings,
) -> Result<StudioSettings, String> {
    let mut normalized = settings;
    normalized.watched_preset_dirs = dedup_nonempty_paths(&normalized.watched_preset_dirs);
    normalized.language = normalized.language.clamp(0, 3);
    normalized.expansion_level = normalize_expansion_level(normalized.expansion_level);
    normalized.ui_locale = i18n::normalize_preference(&normalized.ui_locale);
    save_settings(&normalized)?;

    if let Ok(mut guard) = state.settings.lock() {
        *guard = normalized.clone();
    }
    update_tray_menu_locale(&state, &normalized.ui_locale);
    let _ = state
        .preset_watcher
        .update_dirs_with_app(&normalized.watched_preset_dirs, app.clone());
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_always_on_top(normalized.always_on_top);
    }
    Ok(normalized)
}

#[tauri::command]
fn read_game_version_cmd(game_path: String) -> Result<String, String> {
    read_game_version(&game_path)
}

#[tauri::command]
fn resolve_remakeplace_path_cmd(configured_path: String) -> Option<String> {
    try_resolve_remakeplace_source_dir(&configured_path).map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
async fn check_kawari_server(
    state: State<'_, AppState>,
    server_url: String,
) -> Result<CheckServerResult, String> {
    let normalized = normalize_server_url(&server_url)?;
    let autoconfig = kawari::client::fetch_auto_config(&state.housing.http, &normalized).await?;
    let endpoints = resolve_endpoints(&autoconfig, &normalized)?;
    state.housing.set_simulator_url(&endpoints.simulator_server);

    Ok(CheckServerResult {
        normalized_url: normalized.to_string().trim_end_matches('/').to_string(),
        endpoints,
    })
}

#[tauri::command]
async fn launch_game(
    app: AppHandle,
    state: State<'_, AppState>,
    mut settings: StudioSettings,
    manual_version: Option<String>,
) -> Result<LaunchGameResult, String> {
    let mut warnings = Vec::new();
    let exe_path = resolve_game_executable(&settings.game_path)?;

    let existing_pids = find_running_ffxiv_pids();
    for pid in existing_pids {
        warnings.push(format!("ffxiv_dx11.exe is already running (PID {pid})."));
    }

    let game_version = match read_game_version(&settings.game_path) {
        Ok(ver) => ver,
        Err(_) => {
            let fallback = manual_version.unwrap_or_default().trim().to_string();
            if fallback.is_empty() {
                return Err(
                    "Unable to determine game version (check ffxivgame.ver or enter manually)."
                        .to_string(),
                );
            }
            fallback
        }
    };

    let server_url = normalize_server_url(&settings.server_url)?;
    let autoconfig = kawari::client::fetch_auto_config(&state.housing.http, &server_url).await?;
    let endpoints = resolve_endpoints(&autoconfig, &server_url)?;
    state.housing.set_simulator_url(&endpoints.simulator_server);

    let login_url = normalize_server_url(&endpoints.login_server)?;
    let login_res = kawari::client::login(
        &state.housing.http,
        &login_url,
        settings.username.trim(),
        &settings.password,
    )
    .await?;

    if !login_res.success {
        return Err(login_res
            .error
            .unwrap_or_else(|| "Login failed.".to_string()));
    }
    let session_id = login_res
        .session_id
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "Login response did not include a session ID (SID).".to_string())?;

    let requested_exp = normalize_expansion_level(settings.expansion_level);
    let effective_expansion = requested_exp.min(normalize_expansion_level(login_res.max_expansion));
    if effective_expansion < requested_exp {
        warnings.push(format!(
            "Expansion level adjusted to {effective_expansion} to match account maximum ({effective_expansion})."
        ));
    }

    let raw_args = build_launch_arguments(&LaunchOptions {
        session_id: session_id.clone(),
        lobby_host: endpoints.lobby_host.clone(),
        lobby_port: endpoints.lobby_port,
        frontier_host_argument: endpoints.frontier_host_argument.clone(),
        language: settings.language,
        expansion_level: effective_expansion,
        game_version,
        extra_arguments: settings.extra_arguments.clone(),
        include_frontier_host: settings.include_frontier_host,
    })?;

    let masked_args = raw_args.replace(&session_id, &mask_session_id(&session_id));
    let child = spawn_game_client(&exe_path, &raw_args)?;
    let pid = child.id();

    settings.server_url = server_url.to_string().trim_end_matches('/').to_string();
    settings.watched_preset_dirs = dedup_nonempty_paths(&settings.watched_preset_dirs);
    let _ = save_settings(&settings);
    if let Ok(mut guard) = state.settings.lock() {
        *guard = settings.clone();
    }

    if settings.close_on_launch
        && let Some(win) = app.get_webview_window("main")
    {
        if state.rmp.is_running() || state.server.is_managed_running() {
            let _ = win.hide();
        } else {
            let _ = win.close();
        }
    }

    Ok(LaunchGameResult {
        executable_path: exe_path.to_string_lossy().into_owned(),
        masked_arguments: masked_args,
        pid,
        endpoints,
        warnings,
        effective_expansion,
    })
}

#[tauri::command]
async fn housing_api_request(
    state: State<'_, AppState>,
    method: String,
    path: String,
    body: Option<String>,
) -> Result<housing::proxy::HttpProxyResponse, String> {
    housing::proxy::api_request(&state.housing, &method, &path, body).await
}

#[tauri::command]
fn housing_get_simulator_url(state: State<'_, AppState>) -> String {
    state.housing.simulator_base_url()
}

#[tauri::command]
fn housing_set_simulator_url(state: State<'_, AppState>, url: String) -> String {
    state.housing.set_simulator_url(&url)
}

#[tauri::command]
fn housing_reconnect_sse(state: State<'_, AppState>) {
    state.housing.force_reconnect_sse();
}

#[tauri::command]
async fn housing_preview_local_preset_file(
    state: State<'_, AppState>,
    file_path: String,
    scope: String,
    land_ident: Option<i32>,
) -> Result<housing::proxy::HttpProxyResponse, String> {
    housing::proxy::preview_local_preset_file(&state.housing, &file_path, &scope, land_ident).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn housing_apply_local_preset_file(
    state: State<'_, AppState>,
    land_ident: i32,
    file_path: String,
    scope: String,
    reload_mode: String,
    preserve_pos: bool,
    force_enter: bool,
    label: Option<String>,
) -> Result<housing::proxy::HttpProxyResponse, String> {
    housing::proxy::apply_local_preset_file(
        &state.housing,
        land_ident,
        &file_path,
        &scope,
        &reload_mode,
        preserve_pos,
        force_enter,
        label,
    )
    .await
}

#[tauri::command]
fn scan_local_presets(state: State<'_, AppState>) -> Vec<LocalPresetEntry> {
    let dirs = state
        .settings
        .lock()
        .map(|g| g.watched_preset_dirs.clone())
        .unwrap_or_default();
    scan_watched_dirs(&dirs)
}

#[tauri::command]
fn add_watched_preset_dir(
    app: AppHandle,
    state: State<'_, AppState>,
    dir: String,
) -> Result<Vec<String>, String> {
    let candidate = Path::new(dir.trim());
    let resolved_dir = if candidate.is_file() {
        candidate.parent().unwrap_or(candidate).to_path_buf()
    } else {
        candidate.to_path_buf()
    };
    if !resolved_dir.is_dir() {
        return Err(format!("Not a valid directory: {}", resolved_dir.display()));
    }

    let dirs = {
        let mut guard = state
            .settings
            .lock()
            .map_err(|_| "Failed to lock settings".to_string())?;
        guard
            .watched_preset_dirs
            .push(resolved_dir.to_string_lossy().into_owned());
        guard.watched_preset_dirs = dedup_nonempty_paths(&guard.watched_preset_dirs);
        save_settings(&guard)?;
        guard.watched_preset_dirs.clone()
    };

    state.preset_watcher.update_dirs_with_app(&dirs, app)?;
    Ok(dirs)
}

#[tauri::command]
fn remove_watched_preset_dir(
    app: AppHandle,
    state: State<'_, AppState>,
    dir: String,
) -> Result<Vec<String>, String> {
    let target = dir.trim();
    let dirs = {
        let mut guard = state
            .settings
            .lock()
            .map_err(|_| "Failed to lock settings".to_string())?;
        guard
            .watched_preset_dirs
            .retain(|d| !d.eq_ignore_ascii_case(target));
        save_settings(&guard)?;
        guard.watched_preset_dirs.clone()
    };

    state.preset_watcher.update_dirs_with_app(&dirs, app)?;
    Ok(dirs)
}

#[tauri::command]
fn inspect_preset_file(file_path: String) -> Result<LocalPresetEntry, String> {
    read_local_preset_entry(Path::new(file_path.trim()))
        .ok_or_else(|| format!("Failed to read preset file: {file_path}"))
}

#[tauri::command]
async fn export_preset_to_file(
    state: State<'_, AppState>,
    land_ident: i32,
    scope: String,
    target_path: String,
) -> Result<String, String> {
    let resp = housing::proxy::api_request(
        &state.housing,
        "GET",
        &format!("/estates/{land_ident}/export/remakeplace?scope={scope}"),
        None,
    )
    .await?;
    if !resp.ok {
        return Err(resp.body);
    }

    let formatted = serde_json::from_str::<serde_json::Value>(&resp.body)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or(resp.body);

    let target = PathBuf::from(target_path.trim());
    state.preset_watcher.suppress_path(&target);
    write_text_atomic(&target, &formatted)?;
    Ok(target.to_string_lossy().into_owned())
}

#[tauri::command]
fn reveal_in_explorer_cmd(path: String) -> Result<(), String> {
    reveal_in_explorer(Path::new(path.trim()))
}

#[tauri::command]
fn get_rmp_status(state: State<'_, AppState>) -> RmpSyncStatus {
    state.rmp.current_status()
}

#[tauri::command]
async fn start_rmp_sync(
    app: AppHandle,
    state: State<'_, AppState>,
    remakeplace_path: String,
) -> Result<RmpSyncStatus, String> {
    let resolved = try_resolve_remakeplace_source_dir(&remakeplace_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(remakeplace_path);

    {
        if let Ok(mut guard) = state.settings.lock()
            && guard.remakeplace_path != resolved
        {
            guard.remakeplace_path = resolved.clone();
            let _ = save_settings(&guard);
        }
    }

    state
        .rmp
        .start_sync(app, resolved, Arc::clone(&state.housing))
        .await
}

#[tauri::command]
fn stop_rmp_sync(
    app: AppHandle,
    state: State<'_, AppState>,
    kill_shadow: Option<bool>,
) -> RmpSyncStatus {
    state
        .rmp
        .stop_sync(Some(&app), &state.housing, kill_shadow.unwrap_or(true))
}

#[tauri::command]
async fn get_server_status(
    state: State<'_, AppState>,
    profile: Option<String>,
) -> Result<FoundationServerStatus, String> {
    let root = state
        .settings
        .lock()
        .map(|g| g.foundation_root.clone())
        .unwrap_or_default();
    let sim_url = state.housing.simulator_base_url();
    Ok(state
        .server
        .get_status(&root, profile.as_deref().unwrap_or("debug"), &sim_url)
        .await)
}

#[tauri::command]
async fn start_foundation_server(
    app: AppHandle,
    state: State<'_, AppState>,
    foundation_root: String,
    profile: String,
) -> Result<FoundationServerStatus, String> {
    {
        if let Ok(mut guard) = state.settings.lock() {
            guard.foundation_root = foundation_root.clone();
            let _ = save_settings(&guard);
        }
    }
    let sim_url = state.housing.simulator_base_url();
    let res = state
        .server
        .start_server(app.clone(), &foundation_root, &profile, &sim_url)
        .await?;
    state.housing.force_reconnect_sse();
    Ok(res)
}

#[tauri::command]
fn stop_foundation_server(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.server.stop_server(Some(&app))
}

#[tauri::command]
fn get_server_logs(state: State<'_, AppState>) -> Vec<ServerLogEntry> {
    state.server.get_logs()
}

#[tauri::command]
fn clear_server_logs(state: State<'_, AppState>) {
    state.server.clear_logs();
}

#[tauri::command]
fn set_always_on_top_cmd(
    app: AppHandle,
    state: State<'_, AppState>,
    pinned: bool,
) -> Result<bool, String> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_always_on_top(pinned)
            .map_err(|e| format!("Failed to update always-on-top: {e}"))?;
    }
    if let Ok(mut guard) = state.settings.lock() {
        guard.always_on_top = pinned;
        let _ = save_settings(&guard);
    }
    Ok(pinned)
}

#[tauri::command]
fn update_global_shortcut_cmd(
    app: AppHandle,
    state: State<'_, AppState>,
    shortcut: String,
) -> Result<String, String> {
    let trimmed = shortcut.trim().to_string();
    register_global_shortcut(&app, &trimmed)?;
    if let Ok(mut guard) = state.settings.lock() {
        guard.global_shortcut = trimmed.clone();
        save_settings(&guard)?;
    }
    Ok(trimmed)
}

#[tauri::command]
fn hide_to_tray_cmd(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

#[tauri::command]
fn open_external_url_cmd(app: AppHandle, url: String) -> Result<(), String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("Only HTTP(S) URLs can be opened.".to_string());
    }
    app.opener()
        .open_url(trimmed, None::<&str>)
        .map_err(|e| format!("Failed to open URL: {e}"))
}

#[tauri::command]
async fn pick_file_dialog(
    app: AppHandle,
    title: String,
    default_path: Option<String>,
    filter_name: Option<String>,
    extensions: Option<Vec<String>>,
) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut builder = app.dialog().file().set_title(title);
    if let Some(dp) = default_path.filter(|s| !s.trim().is_empty()) {
        let p = Path::new(&dp);
        let dir = if p.is_file() {
            p.parent()
        } else if p.is_dir() {
            Some(p)
        } else {
            p.parent().filter(|x| x.is_dir())
        };
        if let Some(d) = dir {
            builder = builder.set_directory(d);
        }
    }
    if let (Some(name), Some(exts)) = (filter_name, extensions) {
        let refs: Vec<&str> = exts.iter().map(String::as_str).collect();
        builder = builder.add_filter(name, &refs);
    }
    builder.pick_file(move |picked| {
        let path_str = picked.and_then(|fp| match fp {
            FilePath::Path(p) => Some(p.to_string_lossy().into_owned()),
            FilePath::Url(u) => u
                .to_file_path()
                .ok()
                .map(|p| p.to_string_lossy().into_owned()),
        });
        let _ = tx.send(path_str);
    });
    rx.await.ok().flatten()
}

#[tauri::command]
async fn pick_folder_dialog(
    app: AppHandle,
    title: String,
    default_path: Option<String>,
) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut builder = app.dialog().file().set_title(title);
    if let Some(dp) = default_path.filter(|s| !s.trim().is_empty()) {
        let p = Path::new(&dp);
        if p.is_dir() {
            builder = builder.set_directory(p);
        } else if let Some(parent) = p.parent().filter(|x| x.is_dir()) {
            builder = builder.set_directory(parent);
        }
    }
    builder.pick_folder(move |picked| {
        let path_str = picked.and_then(|fp| match fp {
            FilePath::Path(p) => Some(p.to_string_lossy().into_owned()),
            FilePath::Url(u) => u
                .to_file_path()
                .ok()
                .map(|p| p.to_string_lossy().into_owned()),
        });
        let _ = tx.send(path_str);
    });
    rx.await.ok().flatten()
}

#[tauri::command]
async fn save_preset_dialog(
    app: AppHandle,
    title: String,
    default_dir: Option<String>,
    default_name: String,
) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut builder = app
        .dialog()
        .file()
        .set_title(title)
        .set_file_name(default_name)
        .add_filter("ReMakePlace JSON", &["json"]);
    if let Some(dir) = default_dir.filter(|s| !s.trim().is_empty()) {
        let p = Path::new(&dir);
        if p.is_dir() {
            builder = builder.set_directory(p);
        }
    }
    builder.save_file(move |picked| {
        let path_str = picked.and_then(|fp| match fp {
            FilePath::Path(p) => Some(p.to_string_lossy().into_owned()),
            FilePath::Url(u) => u
                .to_file_path()
                .ok()
                .map(|p| p.to_string_lossy().into_owned()),
        });
        let _ = tx.send(path_str);
    });
    rx.await.ok().flatten()
}

fn toggle_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let visible = window.is_visible().unwrap_or(false);
        let minimized = window.is_minimized().unwrap_or(false);
        if visible && !minimized {
            let _ = window.hide();
        } else {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

fn register_global_shortcut(app: &AppHandle, raw_shortcut: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if raw_shortcut.trim().is_empty() {
        return Ok(());
    }

    let normalized = normalize_shortcut_string(raw_shortcut);
    let shortcut: Shortcut = normalized
        .parse()
        .map_err(|e| format!("Invalid shortcut format '{raw_shortcut}': {e}"))?;
    gs.on_shortcut(shortcut, |app_handle, _shortcut, event| {
        if event.state() == ShortcutState::Pressed {
            toggle_main_window(app_handle);
        }
    })
    .map_err(|e| format!("Failed to register global shortcut ({normalized}): {e}"))
}

fn normalize_shortcut_string(raw: &str) -> String {
    raw.split('+')
        .map(|part| {
            let p = part.trim();
            if p.eq_ignore_ascii_case("ctrl") {
                "Control"
            } else {
                p
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

fn shutdown_managed_children(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.rmp.stop_sync(None, &state.housing, true);
        let _ = state.server.stop_server(None);
        state.preset_watcher.stop();
    }
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let initial_settings = load_settings();
    let app_state = AppState {
        settings: Arc::new(Mutex::new(initial_settings.clone())),
        housing: Arc::new(housing::HousingClientState::new(
            &initial_settings.server_url,
        )),
        preset_watcher: Arc::new(PresetWatcherManager::new()),
        rmp: Arc::new(RmpController::new()),
        server: Arc::new(FoundationServerController::new()),
        tray_menu: Arc::new(Mutex::new(None)),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(app_state)
        .setup(move |app| {
            let handle = app.handle().clone();
            let state = app.state::<AppState>();

            housing::sse::spawn_sse_bridge(handle.clone(), Arc::clone(&state.housing));
            if let Err(err) = state
                .preset_watcher
                .update_dirs_with_app(&initial_settings.watched_preset_dirs, handle.clone())
            {
                tracing::warn!("Preset watcher setup warning: {err}");
            }

            if let Some(window) = app.get_webview_window("main")
                && initial_settings.always_on_top
            {
                let _ = window.set_always_on_top(true);
            }

            if !initial_settings.global_shortcut.trim().is_empty()
                && let Err(err) =
                    register_global_shortcut(&handle, &initial_settings.global_shortcut)
            {
                tracing::warn!("{err}");
            }

            let show_item = MenuItem::with_id(
                app,
                "tray_show",
                i18n::tr(&initial_settings.ui_locale, "tray.show_hide"),
                true,
                None::<&str>,
            )?;
            let pin_item = MenuItem::with_id(
                app,
                "tray_pin",
                i18n::tr(&initial_settings.ui_locale, "tray.toggle_pin"),
                true,
                None::<&str>,
            )?;
            let quit_item = MenuItem::with_id(
                app,
                "tray_quit",
                i18n::tr(&initial_settings.ui_locale, "tray.quit"),
                true,
                None::<&str>,
            )?;
            if let Ok(mut tray_guard) = state.tray_menu.lock() {
                *tray_guard = Some(TrayMenuHandles {
                    show: show_item.clone(),
                    pin: pin_item.clone(),
                    quit: quit_item.clone(),
                });
            }
            let tray_menu = Menu::with_items(app, &[&show_item, &pin_item, &quit_item])?;

            let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                .menu(&tray_menu)
                .tooltip("Foundation Studio")
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_main_window(tray.app_handle());
                    }
                })
                .on_menu_event(|app_handle, event| match event.id.as_ref() {
                    "tray_show" => toggle_main_window(app_handle),
                    "tray_pin" => {
                        if let Some(window) = app_handle.get_webview_window("main") {
                            let next = !window.is_always_on_top().unwrap_or(false);
                            let _ = window.set_always_on_top(next);
                            if let Some(st) = app_handle.try_state::<AppState>()
                                && let Ok(mut g) = st.settings.lock()
                            {
                                g.always_on_top = next;
                                let _ = save_settings(&g);
                            }
                            let _ = app_handle.emit("always-on-top-changed", next);
                        }
                    }
                    "tray_quit" => {
                        shutdown_managed_children(app_handle);
                        app_handle.exit(0);
                    }
                    _ => {}
                });

            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }
            tray_builder.build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app_handle = window.app_handle();
                let should_minimize = if let Some(state) = app_handle.try_state::<AppState>() {
                    let min_pref = state
                        .settings
                        .lock()
                        .map(|g| g.minimize_to_tray_on_close)
                        .unwrap_or(true);
                    let active_bg = state.rmp.is_running() || state.server.is_managed_running();
                    min_pref && active_bg
                } else {
                    false
                };

                if should_minimize {
                    api.prevent_close();
                    let _ = window.hide();
                    let _ = app_handle.emit(
                        "app-toast",
                        ToastKeyPayload {
                            key: "tray.minimized_notice".to_string(),
                            params: serde_json::Map::new(),
                        },
                    );
                } else {
                    shutdown_managed_children(app_handle);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_system_locale_cmd,
            set_ui_locale_cmd,
            save_settings_cmd,
            read_game_version_cmd,
            resolve_remakeplace_path_cmd,
            check_kawari_server,
            launch_game,
            housing_api_request,
            housing_get_simulator_url,
            housing_set_simulator_url,
            housing_reconnect_sse,
            housing_preview_local_preset_file,
            housing_apply_local_preset_file,
            scan_local_presets,
            add_watched_preset_dir,
            remove_watched_preset_dir,
            inspect_preset_file,
            export_preset_to_file,
            reveal_in_explorer_cmd,
            get_rmp_status,
            start_rmp_sync,
            stop_rmp_sync,
            get_server_status,
            start_foundation_server,
            stop_foundation_server,
            get_server_logs,
            clear_server_logs,
            set_always_on_top_cmd,
            update_global_shortcut_cmd,
            hide_to_tray_cmd,
            open_external_url_cmd,
            pick_file_dialog,
            pick_folder_dialog,
            save_preset_dialog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Foundation Studio");
}
