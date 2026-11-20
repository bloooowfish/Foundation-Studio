pub mod account;
pub mod config_yaml;
pub mod locations;
pub mod remakeplace;
pub mod updates;

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use zip::ZipArchive;

use crate::kawari::launch::resolve_game_executable;
use crate::rmp::locator::{find_bridge_root_dir, try_resolve_remakeplace_source_dir};
use crate::server::{resolve_binary_dir, resolve_foundation_root, world_binary_has_oodle};
use crate::settings::{StudioSettings, managed_foundation_dir, managed_rmp_bridge_dir};

pub const FOUNDATION_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/bloooowfish/Foundation/releases/latest";
pub const OODLE_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/WorkingRobot/OodleUE/releases/latest";
pub const OODLE_FALLBACK_URL: &str = "https://github.com/WorkingRobot/OodleUE/releases/download/2026-06-04-1357/msvc-x64-release.zip";
pub const OODLE_FALLBACK_SHA256: &str =
    "08844ec260e40134ce6089ffe2003d6f862cd2abf1ebfb62a3fa8d2f69f48a2b";
pub const OODLE_FALLBACK_TAG: &str = "2026-06-04-1357";
/// Written next to `oodle/oodle-network-shared.dll` with the release tag it came from.
pub const OODLE_VERSION_FILE: &str = "version.txt";
pub const RMP_BRIDGE_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/bloooowfish/rmp-bridge/releases/latest";
pub const RMP_BRIDGE_ARCHIVE_URL: &str =
    "https://github.com/bloooowfish/rmp-bridge/archive/refs/heads/main.zip";
/// Append `<sha>.zip` for an exact-commit source archive.
pub const RMP_BRIDGE_ARCHIVE_BASE: &str = "https://github.com/bloooowfish/rmp-bridge/archive/";
pub const RMP_BRIDGE_MAIN_COMMIT_API: &str =
    "https://api.github.com/repos/bloooowfish/rmp-bridge/commits/main";
pub const UE4SS_VERSION: &str = "v4.0.0";
pub const UE4SS_URL: &str =
    "https://github.com/UE4SS-RE/RE-UE4SS/releases/download/v4.0.0/UE4SS_v4.0.0.zip";
pub const UE4SS_SHA256: &str = "251ad0d1381a241750e036f5918cbf2b5d7ec58ebc608cb2dd09d459d0c81e45";

const FOUNDATION_REQUIRED_BINS: &[&str] = &[
    "kawari-run.exe",
    "kawari-world.exe",
    "kawari-lobby.exe",
    "kawari-login.exe",
    "kawari-frontier.exe",
    "kawari-web.exe",
    "kawari-admin.exe",
    "kawari-patch.exe",
];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    pub managed_foundation_dir: String,
    pub active_foundation_dir: String,
    pub managed_bridge_dir: String,
    pub active_bridge_dir: String,
    /// `managed` | `custom` | `dev` | `missing`.
    pub foundation_source: String,
    /// `managed` | `custom` | `dev` | `missing`.
    pub bridge_source: String,
    pub foundation_installed: bool,
    pub foundation_layout: String,
    pub foundation_version: Option<String>,
    pub foundation_has_config: bool,
    pub foundation_has_oodle_feature: bool,
    pub oodle_installed: bool,
    pub rmp_bridge_installed: bool,
    pub ue4ss_installed: bool,
    pub game_path_valid: bool,
    pub remakeplace_valid: bool,
    pub remakeplace_installed_version: Option<String>,
    pub config_localhost_only: bool,
    pub busy: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupProgressEvent {
    pub component: String,
    pub phase: String,
    pub percent: Option<u8>,
    pub bytes_downloaded: u64,
    pub bytes_total: Option<u64>,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupInstallReport {
    pub installed: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    pub foundation_root: String,
    pub bridge_root: String,
    pub status: SetupStatus,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: Option<String>,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

pub struct SetupManager {
    busy: AtomicBool,
    http: Client,
    /// Client without a total request timeout for multi-GB archive downloads.
    download_http: Client,
}

impl Default for SetupManager {
    fn default() -> Self {
        Self::new()
    }
}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl SetupManager {
    pub fn new() -> Self {
        let http = Client::builder()
            .user_agent(concat!("Foundation-Studio/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| Client::new());

        let download_http = Client::builder()
            .user_agent(concat!("Foundation-Studio/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            busy: AtomicBool::new(false),
            http,
            download_http,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn inspect_status(&self, settings: &StudioSettings, profile: &str) -> SetupStatus {
        let managed_foundation = managed_foundation_dir();
        let managed_bridge = managed_rmp_bridge_dir();
        let active_foundation_path = locations::active_foundation_root(&settings.foundation_root);
        let resolved_foundation = resolve_foundation_root(&settings.foundation_root);

        let resolved_bridge = find_bridge_root_dir(None);
        let active_bridge_path = resolved_bridge
            .clone()
            .unwrap_or_else(|| managed_bridge.clone());

        let (bin_dir, effective_profile) = resolve_binary_dir(&active_foundation_path, profile);
        let has_core_bins = ["kawari-run.exe", "kawari-world.exe", "kawari-lobby.exe"]
            .iter()
            .all(|b| bin_dir.join(b).is_file());

        let foundation_layout = if !has_core_bins {
            "missing".to_string()
        } else if effective_profile == "prebuilt" {
            "prebuilt".to_string()
        } else {
            "cargo".to_string()
        };

        let foundation_version = read_version_marker(&active_foundation_path);
        let foundation_has_config = active_foundation_path.join("config.yaml").is_file();
        let world_exe = bin_dir.join("kawari-world.exe");
        let foundation_has_oodle_feature =
            world_exe.is_file() && world_binary_has_oodle(&world_exe);
        let oodle_installed = bin_dir.join("oodle-network-shared.dll").is_file()
            || active_foundation_path
                .join("oodle")
                .join("oodle-network-shared.dll")
                .is_file();

        let rmp_bridge_installed = resolved_bridge
            .as_deref()
            .is_some_and(locations::bridge_files_present);
        let foundation_source = locations::classify_source(
            Some(&active_foundation_path),
            locations::CheckoutKind::Foundation,
            has_core_bins,
        )
        .to_string();
        let bridge_source = locations::classify_source(
            resolved_bridge.as_deref(),
            locations::CheckoutKind::Bridge,
            rmp_bridge_installed,
        )
        .to_string();

        let ue4ss_dir = active_bridge_path.join("vendor").join("ue4ss-v4.0.0");
        let ue4ss_installed = ue4ss_dir.join("dwmapi.dll").is_file()
            && ue4ss_dir.join("ue4ss").join("UE4SS.dll").is_file();

        let game_path_valid = resolve_game_executable(&settings.game_path).is_ok();
        let remakeplace_valid =
            try_resolve_remakeplace_source_dir(&settings.remakeplace_path).is_some();
        let remakeplace_installed_version =
            remakeplace::read_installed_version(&settings.remakeplace_path);
        let config_localhost_only = foundation_has_config
            && config_yaml::config_file_is_localhost_only(
                &active_foundation_path.join("config.yaml"),
            );

        SetupStatus {
            managed_foundation_dir: managed_foundation.to_string_lossy().into_owned(),
            active_foundation_dir: resolved_foundation
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| settings.foundation_root.trim().to_string()),
            managed_bridge_dir: managed_bridge.to_string_lossy().into_owned(),
            active_bridge_dir: resolved_bridge
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            foundation_source,
            bridge_source,
            foundation_installed: has_core_bins,
            foundation_layout,
            foundation_version,
            foundation_has_config,
            foundation_has_oodle_feature,
            oodle_installed,
            rmp_bridge_installed,
            ue4ss_installed,
            game_path_valid,
            remakeplace_valid,
            remakeplace_installed_version,
            config_localhost_only,
            busy: self.is_busy(),
        }
    }

    pub async fn install_components(
        &self,
        app: &AppHandle,
        settings: &StudioSettings,
        components: &[String],
        use_managed_dir: bool,
        profile: &str,
    ) -> Result<SetupInstallReport, String> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("Runtime setup is already in progress.".to_string());
        }
        let _guard = BusyGuard(&self.busy);

        let managed_foundation = managed_foundation_dir();
        let managed_bridge = managed_rmp_bridge_dir();

        let foundation_target = if use_managed_dir || settings.foundation_root.trim().is_empty() {
            managed_foundation.clone()
        } else {
            PathBuf::from(settings.foundation_root.trim().trim_matches('"'))
        };

        let bridge_target = if use_managed_dir {
            managed_bridge.clone()
        } else {
            find_bridge_root_dir(None).unwrap_or_else(|| managed_bridge.clone())
        };

        let requested = normalize_component_list(components);
        let mut installed = Vec::new();
        let mut skipped = Vec::new();
        let mut errors = Vec::new();

        for comp in requested {
            let guard = guard_dev_checkout_target(&comp, &foundation_target, &bridge_target);
            let res = match comp.as_str() {
                _ if guard.is_err() => guard,
                "foundation" => self.install_foundation(Some(app), &foundation_target).await,
                "oodle" => {
                    self.install_oodle(Some(app), &foundation_target, profile)
                        .await
                }
                "rmp_bridge" => self.install_rmp_bridge(Some(app), &bridge_target).await,
                "ue4ss" => self.install_ue4ss(Some(app), &bridge_target).await,
                "config" => generate_foundation_config(
                    &foundation_target,
                    &settings.game_path,
                    settings.foundation_root.trim().is_empty()
                        || !foundation_target.join("config.yaml").is_file(),
                )
                .map(|_| ()),
                other => Err(format!("Unknown setup component: {other}")),
            };

            match res {
                Ok(()) => {
                    emit_progress(
                        Some(app),
                        &comp,
                        "done",
                        Some(100),
                        0,
                        None,
                        format!("Installed {comp}"),
                    );
                    installed.push(comp);
                }
                Err(err) if err.starts_with("SKIP:") => {
                    skipped.push(comp);
                }
                Err(err) => {
                    emit_progress(Some(app), &comp, "error", None, 0, None, err.clone());
                    errors.push(format!("{comp}: {err}"));
                }
            }
        }

        // Auto-generate config.yaml after installing Foundation binaries if game_path is configured and config.yaml is absent.
        if installed.iter().any(|c| c == "foundation")
            && !foundation_target.join("config.yaml").is_file()
            && !settings.game_path.trim().is_empty()
            && let Ok(_cfg_path) =
                generate_foundation_config(&foundation_target, &settings.game_path, false)
        {
            installed.push("config".to_string());
        }

        let mut next_settings = settings.clone();
        if foundation_target.exists()
            && (use_managed_dir || next_settings.foundation_root.trim().is_empty())
        {
            next_settings.foundation_root = foundation_target.to_string_lossy().into_owned();
        }
        let status = self.inspect_status(&next_settings, profile);

        Ok(SetupInstallReport {
            installed,
            skipped,
            errors,
            foundation_root: foundation_target.to_string_lossy().into_owned(),
            bridge_root: bridge_target.to_string_lossy().into_owned(),
            status,
        })
    }

    async fn install_foundation(
        &self,
        app: Option<&AppHandle>,
        target_dir: &Path,
    ) -> Result<(), String> {
        emit_progress(
            app,
            "foundation",
            "downloading",
            None,
            0,
            None,
            "Checking latest bloooowfish/Foundation release...".to_string(),
        );
        let release = self
            .fetch_latest_github_release(FOUNDATION_LATEST_RELEASE_API)
            .await
            .map_err(|err| {
                if err.contains("404") {
                    "No prebuilt Foundation release is published yet on bloooowfish/Foundation (tag v* to publish via GitHub Actions, or point to a local build folder).".to_string()
                } else {
                    err
                }
            })?;

        let asset = release
            .assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case("foundation-windows-x64.zip"))
            .ok_or_else(|| {
                "Latest Foundation release does not contain foundation-windows-x64.zip.".to_string()
            })?;

        let mut expected_sha = parse_sha256_digest(asset.digest.as_deref());
        if expected_sha.is_none()
            && let Some(sums_asset) = release
                .assets
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"))
        {
            expected_sha = self
                .fetch_sha256_from_sums_asset(&sums_asset.browser_download_url, &asset.name)
                .await;
        }

        let tmp_zip = temp_download_path("foundation-windows-x64.zip");
        let _dl = self
            .download_file_verified(
                app,
                "foundation",
                &asset.browser_download_url,
                &tmp_zip,
                expected_sha.as_deref(),
            )
            .await;
        if let Err(err) = _dl {
            let _ = fs::remove_file(&tmp_zip);
            return Err(err);
        }

        emit_progress(
            app,
            "foundation",
            "extracting",
            Some(95),
            0,
            None,
            "Extracting Foundation binaries & resources...".to_string(),
        );
        let preserved = FOUNDATION_PRESERVED;
        // Drop the previous version marker so an update records the new release tag
        // (restored if extraction fails).
        let previous_marker = read_version_marker(target_dir);
        let _ = fs::remove_file(target_dir.join("version.txt"));
        let extract_res = extract_zip_to_dir(&tmp_zip, target_dir, false, preserved);
        let _ = fs::remove_file(&tmp_zip);
        if let Err(err) = extract_res {
            if let Some(prev) = previous_marker {
                let _ = fs::write(target_dir.join("version.txt"), prev);
            }
            return Err(err);
        }

        if let Some(tag) = release.tag_name.as_deref()
            && !tag.trim().is_empty()
            && !target_dir.join("version.txt").is_file()
        {
            let _ = fs::write(target_dir.join("version.txt"), tag.trim());
        }

        for bin in ["kawari-run.exe", "kawari-world.exe", "kawari-lobby.exe"] {
            if !target_dir.join(bin).is_file() {
                return Err(format!("Extracted archive missing {bin}"));
            }
        }
        let _ = FOUNDATION_REQUIRED_BINS;
        Ok(())
    }

    async fn install_oodle(
        &self,
        app: Option<&AppHandle>,
        foundation_dir: &Path,
        profile: &str,
    ) -> Result<(), String> {
        emit_progress(
            app,
            "oodle",
            "downloading",
            None,
            0,
            None,
            "Resolving WorkingRobot/OodleUE official release...".to_string(),
        );
        let (url, expected_sha, tag) = match self
            .fetch_latest_github_release(OODLE_LATEST_RELEASE_API)
            .await
        {
            Ok(rel) => {
                if let Some(asset) = rel
                    .assets
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case("msvc-x64-release.zip"))
                {
                    (
                        asset.browser_download_url.clone(),
                        parse_sha256_digest(asset.digest.as_deref()),
                        rel.tag_name.clone(),
                    )
                } else {
                    (
                        OODLE_FALLBACK_URL.to_string(),
                        Some(OODLE_FALLBACK_SHA256.to_string()),
                        Some(OODLE_FALLBACK_TAG.to_string()),
                    )
                }
            }
            Err(_) => (
                OODLE_FALLBACK_URL.to_string(),
                Some(OODLE_FALLBACK_SHA256.to_string()),
                Some(OODLE_FALLBACK_TAG.to_string()),
            ),
        };

        let tmp_zip = temp_download_path("oodle-msvc-x64-release.zip");
        if let Err(err) = self
            .download_file_verified(app, "oodle", &url, &tmp_zip, expected_sha.as_deref())
            .await
        {
            let _ = fs::remove_file(&tmp_zip);
            return Err(err);
        }

        emit_progress(
            app,
            "oodle",
            "extracting",
            Some(95),
            0,
            None,
            "Installing oodle-network-shared.dll...".to_string(),
        );

        let mut dests = vec![
            foundation_dir
                .join("oodle")
                .join("oodle-network-shared.dll"),
        ];
        let (bin_dir, _) = resolve_binary_dir(foundation_dir, profile);
        let bin_dll = bin_dir.join("oodle-network-shared.dll");
        if !dests.contains(&bin_dll) {
            dests.push(bin_dll);
        }
        let res = extract_oodle_dll_from_zip(&tmp_zip, &dests);
        let _ = fs::remove_file(&tmp_zip);
        res?;
        if let Some(tag) = tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            let _ = fs::write(foundation_dir.join("oodle").join(OODLE_VERSION_FILE), tag);
        }
        Ok(())
    }

    async fn install_rmp_bridge(
        &self,
        app: Option<&AppHandle>,
        bridge_dir: &Path,
    ) -> Result<(), String> {
        emit_progress(
            app,
            "rmp_bridge",
            "downloading",
            None,
            0,
            None,
            "Downloading bloooowfish/rmp-bridge scripts & mods...".to_string(),
        );

        let release = match self
            .fetch_latest_github_release(RMP_BRIDGE_LATEST_RELEASE_API)
            .await
        {
            Ok(rel) => rel.assets.iter().find_map(|asset| {
                if asset.name.to_ascii_lowercase().ends_with(".zip") {
                    Some((
                        asset.browser_download_url.clone(),
                        parse_sha256_digest(asset.digest.as_deref()),
                        rel.tag_name.clone().filter(|t| !t.trim().is_empty()),
                    ))
                } else {
                    None
                }
            }),
            Err(_) => None,
        };

        // (url, sha256, strip_top, marker (ref, kind))
        let (url, expected_sha, strip_top, marker) = match release {
            Some((url, sha, tag)) => (
                url,
                sha,
                false,
                tag.map(|t| (t.trim().to_string(), locations::BRIDGE_KIND_RELEASE)),
            ),
            None => match self.fetch_main_commit_sha(RMP_BRIDGE_MAIN_COMMIT_API).await {
                Ok(sha) => (
                    format!("{RMP_BRIDGE_ARCHIVE_BASE}{sha}.zip"),
                    None,
                    true,
                    Some((locations::short_sha(&sha), locations::BRIDGE_KIND_COMMIT)),
                ),
                Err(_) => (RMP_BRIDGE_ARCHIVE_URL.to_string(), None, true, None),
            },
        };

        let tmp_zip = temp_download_path("rmp-bridge.zip");
        if let Err(err) = self
            .download_file_verified(app, "rmp_bridge", &url, &tmp_zip, expected_sha.as_deref())
            .await
        {
            let _ = fs::remove_file(&tmp_zip);
            return Err(err);
        }

        emit_progress(
            app,
            "rmp_bridge",
            "extracting",
            Some(95),
            0,
            None,
            "Extracting rmp-bridge files...".to_string(),
        );
        let res = extract_zip_to_dir_filtered(
            &tmp_zip,
            bridge_dir,
            strip_top,
            BRIDGE_PRESERVED,
            is_rmp_bridge_runtime_path,
        );
        let _ = fs::remove_file(&tmp_zip);
        res?;

        match marker {
            Some((git_ref, kind)) => locations::write_bridge_marker(
                bridge_dir,
                &locations::BridgeMarker {
                    git_ref,
                    kind: kind.to_string(),
                    installed_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                },
            )?,
            None => {
                // Unknown revision (commit lookup failed): drop any stale marker.
                let _ = fs::remove_file(bridge_dir.join(locations::BRIDGE_MARKER_FILE));
            }
        }
        Ok(())
    }

    async fn fetch_main_commit_sha(&self, api_url: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct CommitInfo {
            sha: String,
        }
        let resp = self
            .http
            .get(api_url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("Failed querying GitHub commit API ({api_url}): {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GitHub API returned HTTP {status} for {api_url}"));
        }
        let info = resp
            .json::<CommitInfo>()
            .await
            .map_err(|e| format!("Failed parsing GitHub commit metadata ({api_url}): {e}"))?;
        let sha = info.sha.trim().to_ascii_lowercase();
        if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("Unexpected commit sha from {api_url}"));
        }
        Ok(sha)
    }

    async fn install_ue4ss(
        &self,
        app: Option<&AppHandle>,
        bridge_dir: &Path,
    ) -> Result<(), String> {
        emit_progress(
            app,
            "ue4ss",
            "downloading",
            None,
            0,
            None,
            format!("Downloading RE-UE4SS {UE4SS_VERSION}..."),
        );

        let tmp_zip = temp_download_path("UE4SS_v4.0.0.zip");
        if let Err(err) = self
            .download_file_verified(app, "ue4ss", UE4SS_URL, &tmp_zip, Some(UE4SS_SHA256))
            .await
        {
            let _ = fs::remove_file(&tmp_zip);
            return Err(err);
        }

        emit_progress(
            app,
            "ue4ss",
            "extracting",
            Some(95),
            0,
            None,
            "Extracting RE-UE4SS runtime...".to_string(),
        );
        let ue4ss_dir = bridge_dir.join("vendor").join("ue4ss-v4.0.0");
        let res = extract_zip_to_dir(&tmp_zip, &ue4ss_dir, false, &[]);
        let _ = fs::remove_file(&tmp_zip);
        res?;

        if !ue4ss_dir.join("dwmapi.dll").is_file()
            || !ue4ss_dir.join("ue4ss").join("UE4SS.dll").is_file()
        {
            return Err("Extracted RE-UE4SS archive missing dwmapi.dll or UE4SS.dll.".to_string());
        }
        Ok(())
    }

    async fn fetch_latest_github_release(&self, api_url: &str) -> Result<GithubRelease, String> {
        let resp = self
            .http
            .get(api_url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("Failed querying GitHub release API ({api_url}): {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GitHub API returned HTTP {status} for {api_url}"));
        }
        resp.json::<GithubRelease>()
            .await
            .map_err(|e| format!("Failed parsing GitHub release metadata ({api_url}): {e}"))
    }

    async fn fetch_sha256_from_sums_asset(&self, url: &str, filename: &str) -> Option<String> {
        let resp = self.http.get(url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let text = resp.text().await.ok()?;
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2
                && parts[0].len() == 64
                && parts[0].chars().all(|c| c.is_ascii_hexdigit())
                && parts[1]
                    .trim_start_matches('*')
                    .eq_ignore_ascii_case(filename)
            {
                return Some(parts[0].to_ascii_lowercase());
            }
        }
        None
    }

    pub async fn download_file_verified(
        &self,
        app: Option<&AppHandle>,
        component: &str,
        url: &str,
        dest_path: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<String, String> {
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed creating temp directory: {e}"))?;
        }

        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Download request failed ({url}): {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("Download failed with HTTP {status} ({url})"));
        }

        let total_bytes = resp.content_length();
        let mut out =
            File::create(dest_path).map_err(|e| format!("Failed creating temp file: {e}"))?;
        let mut hasher = Sha256::new();
        let mut downloaded: u64 = 0;
        let mut last_emit = Instant::now();
        let mut stream = resp.bytes_stream();

        while let Some(chunk_res) = stream.next().await {
            let chunk = match chunk_res {
                Ok(c) => c,
                Err(e) => {
                    drop(out);
                    let _ = fs::remove_file(dest_path);
                    return Err(format!("Download stream error: {e}"));
                }
            };
            if let Err(e) = out.write_all(&chunk) {
                drop(out);
                let _ = fs::remove_file(dest_path);
                return Err(format!("Failed writing temp download file: {e}"));
            }
            hasher.update(&chunk);
            downloaded += chunk.len() as u64;

            if last_emit.elapsed() >= Duration::from_millis(90) {
                let pct = total_bytes.and_then(|tot| {
                    if tot > 0 {
                        Some(((downloaded.saturating_mul(100)) / tot).min(99) as u8)
                    } else {
                        None
                    }
                });
                emit_progress(
                    app,
                    component,
                    "downloading",
                    pct,
                    downloaded,
                    total_bytes,
                    url.to_string(),
                );
                last_emit = Instant::now();
            }
        }
        out.flush()
            .map_err(|e| format!("Failed flushing download file: {e}"))?;

        let actual_sha = format!("{:x}", hasher.finalize());
        if let Some(expected) = expected_sha256 {
            let expected_norm = expected.trim().to_ascii_lowercase();
            emit_progress(
                app,
                component,
                "verifying",
                Some(92),
                downloaded,
                total_bytes,
                "Verifying SHA-256 checksum...".to_string(),
            );
            if actual_sha != expected_norm {
                let _ = fs::remove_file(dest_path);
                return Err(format!(
                    "SHA-256 verification failed (expected {expected_norm}, got {actual_sha})"
                ));
            }
        }

        Ok(actual_sha)
    }
}

pub fn generate_foundation_config(
    foundation_root: &Path,
    raw_game_path: &str,
    overwrite: bool,
) -> Result<PathBuf, String> {
    let config_path = foundation_root.join("config.yaml");
    if config_path.is_file() && !overwrite {
        return Ok(config_path);
    }

    let game_dir = resolve_game_directory(raw_game_path)?;
    fs::create_dir_all(foundation_root)
        .map_err(|e| format!("Failed creating Foundation folder: {e}"))?;

    let escaped_game_dir = serde_json::to_string(&game_dir.to_string_lossy().into_owned())
        .map_err(|e| format!("Failed escaping game directory: {e}"))?;

    let yaml = format!(
        "filesystem:\n  game_path: {escaped_game_dir}\nadmin:\n  listen_address: \"127.0.0.1\"\n  server_name: \"http://127.0.0.1:21057\"\nfrontier:\n  listen_address: \"127.0.0.1\"\n  server_name: \"http://127.0.0.1:21058\"\nlobby:\n  listen_address: \"127.0.0.1\"\n  server_name: \"127.0.0.1\"\nlogin:\n  listen_address: \"127.0.0.1\"\n  server_name: \"http://127.0.0.1:21060\"\npatch:\n  listen_address: \"127.0.0.1\"\n  server_name: \"http://127.0.0.1:21061\"\nweb:\n  listen_address: \"127.0.0.1\"\n  server_name: \"http://127.0.0.1:21062\"\nworld:\n  listen_address: \"127.0.0.1\"\n  server_name: \"127.0.0.1\"\n"
    );

    fs::write(&config_path, yaml).map_err(|e| {
        format!(
            "Failed writing config.yaml ({}): {e}",
            config_path.display()
        )
    })?;
    Ok(config_path)
}

fn resolve_game_directory(raw_game_path: &str) -> Result<PathBuf, String> {
    let trimmed = raw_game_path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err(
            "Please configure a valid FFXIV Game Path (ffxiv_dx11.exe or install folder) before generating config.yaml."
                .to_string(),
        );
    }

    if let Ok(exe) = resolve_game_executable(trimmed)
        && let Some(parent) = exe.parent()
    {
        return Ok(clean_windows_path(parent));
    }

    let candidate = Path::new(trimmed);
    if candidate.is_dir() {
        if candidate.join("ffxiv_dx11.exe").is_file() || candidate.join("ffxivgame.ver").is_file() {
            return Ok(clean_windows_path(candidate));
        }
        let nested = candidate.join("game");
        if nested.join("ffxiv_dx11.exe").is_file() || nested.join("ffxivgame.ver").is_file() {
            return Ok(clean_windows_path(&nested));
        }
    }

    Err(format!(
        "Could not locate FFXIV game directory from '{trimmed}'."
    ))
}

fn clean_windows_path(p: &Path) -> PathBuf {
    let resolved = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = resolved.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

pub fn extract_oodle_dll_from_zip(zip_path: &Path, dest_paths: &[PathBuf]) -> Result<(), String> {
    if dest_paths.is_empty() {
        return Err("No destination path supplied for oodle-network-shared.dll.".to_string());
    }
    let file = File::open(zip_path).map_err(|e| format!("Failed opening Oodle zip: {e}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| format!("Invalid Oodle zip archive: {e}"))?;

    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|e| format!("Failed reading zip entry #{idx}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let Some(rel) = entry
            .enclosed_name()
            .map(|p| PathBuf::from(p.as_os_str()))
            .filter(|p| is_clean_relative_path(p))
        else {
            continue;
        };
        let is_oodle = rel
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("oodle-network-shared.dll"));
        if !is_oodle {
            continue;
        }

        let mut bytes = Vec::new();
        io::copy(&mut entry, &mut bytes)
            .map_err(|e| format!("Failed reading oodle-network-shared.dll from zip: {e}"))?;

        for dest in dest_paths {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed creating Oodle directory: {e}"))?;
            }
            fs::write(dest, &bytes)
                .map_err(|e| format!("Failed writing {}: {e}", dest.display()))?;
        }
        return Ok(());
    }

    Err("Archive did not contain oodle-network-shared.dll.".to_string())
}

pub fn is_rmp_bridge_runtime_path(rel: &Path) -> bool {
    let rel_norm = rel
        .components()
        .filter_map(|comp| match comp {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
        .to_ascii_lowercase();

    rel_norm == "scripts/prepare-shadow.ps1"
        || rel_norm == "license"
        || rel_norm == "readme.md"
        || rel_norm == "mods/foundationbridge"
        || rel_norm.starts_with("mods/foundationbridge/")
}

pub fn extract_zip_to_dir(
    zip_path: &Path,
    target_dir: &Path,
    strip_top_dir: bool,
    preserved_top_names: &[&str],
) -> Result<(), String> {
    extract_zip_to_dir_filtered(
        zip_path,
        target_dir,
        strip_top_dir,
        preserved_top_names,
        |_| true,
    )
}

pub fn extract_zip_to_dir_filtered<F>(
    zip_path: &Path,
    target_dir: &Path,
    strip_top_dir: bool,
    preserved_top_names: &[&str],
    mut allow_rel: F,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    fs::create_dir_all(target_dir).map_err(|e| {
        format!(
            "Failed creating target directory {}: {e}",
            target_dir.display()
        )
    })?;

    let file = File::open(zip_path).map_err(|e| format!("Failed opening archive: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("Invalid zip archive: {e}"))?;

    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|e| format!("Failed reading archive entry #{idx}: {e}"))?;
        let Some(raw_rel) = entry.enclosed_name().map(|p| PathBuf::from(p.as_os_str())) else {
            return Err("Refusing archive with unsafe path traversal entry.".to_string());
        };
        if !is_clean_relative_path(&raw_rel) {
            return Err("Refusing archive with invalid relative path entry.".to_string());
        }

        let rel = if strip_top_dir {
            let mut comps = raw_rel.components();
            comps.next();
            let stripped: PathBuf = comps.collect();
            if stripped.as_os_str().is_empty() {
                continue;
            }
            stripped
        } else {
            raw_rel
        };

        if !allow_rel(&rel) {
            continue;
        }

        if let Some(Component::Normal(first)) = rel.components().next() {
            let first_str = first.to_string_lossy();
            if preserved_top_names
                .iter()
                .any(|p| preserved_name_matches(p, &first_str))
            {
                let preserved_path = target_dir.join(first);
                if preserved_path.exists() {
                    continue;
                }
            }
        }

        let out_path = target_dir.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)
                .map_err(|e| format!("Failed creating directory {}: {e}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed creating parent directory: {e}"))?;
        }

        let mut out_file = File::create(&out_path)
            .map_err(|e| format!("Failed creating extracted file {}: {e}", out_path.display()))?;
        io::copy(&mut entry, &mut out_file)
            .map_err(|e| format!("Failed writing extracted file {}: {e}", out_path.display()))?;
    }

    Ok(())
}

fn is_clean_relative_path(path: &Path) -> bool {
    let mut has_normal = false;
    for comp in path.components() {
        match comp {
            Component::Normal(_) => has_normal = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    has_normal
}

/// Refuses archive extraction of `foundation` / `rmp_bridge` into a developer checkout.
/// Oodle, RE-UE4SS and config remain allowed (they only touch gitignored paths).
pub fn guard_dev_checkout_target(
    component: &str,
    foundation_target: &Path,
    bridge_target: &Path,
) -> Result<(), String> {
    match component {
        "foundation"
            if locations::is_dev_checkout(
                foundation_target,
                locations::CheckoutKind::Foundation,
            ) =>
        {
            Err(locations::dev_checkout_error(foundation_target))
        }
        "rmp_bridge"
            if locations::is_dev_checkout(bridge_target, locations::CheckoutKind::Bridge) =>
        {
            Err(locations::dev_checkout_error(bridge_target))
        }
        _ => Ok(()),
    }
}

/// Top-level names kept on Foundation updates (`*` prefix = case-insensitive suffix match).
pub const FOUNDATION_PRESERVED: &[&str] = &["config.yaml", "*.db", "oodle"];
/// Top-level names kept on rmp-bridge updates.
pub const BRIDGE_PRESERVED: &[&str] = &["vendor", "runtime", "dumps"];

fn preserved_name_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => name
            .to_ascii_lowercase()
            .ends_with(&suffix.to_ascii_lowercase()),
        None => pattern.eq_ignore_ascii_case(name),
    }
}

fn normalize_component_list(components: &[String]) -> Vec<String> {
    let canonical_order = ["foundation", "oodle", "rmp_bridge", "ue4ss", "config"];
    if components.is_empty() {
        return canonical_order.iter().map(|s| (*s).to_string()).collect();
    }
    let set: HashSet<String> = components
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    let mut out: Vec<String> = canonical_order
        .iter()
        .filter(|k| set.contains(**k))
        .map(|k| (*k).to_string())
        .collect();
    for item in components {
        let lower = item.trim().to_ascii_lowercase();
        if !canonical_order.contains(&lower.as_str()) && !out.contains(&lower) {
            out.push(lower);
        }
    }
    out
}

fn parse_sha256_digest(digest: Option<&str>) -> Option<String> {
    let raw = digest?.trim();
    let hex = raw.strip_prefix("sha256:").unwrap_or(raw).trim();
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

fn read_version_marker(root: &Path) -> Option<String> {
    let content = fs::read_to_string(root.join("version.txt")).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Oodle release tag recorded at install time; `None` if unknown or the DLL is missing.
fn read_oodle_version(foundation_root: &Path, bin_dir: &Path) -> Option<String> {
    let oodle_dir = foundation_root.join("oodle");
    let has_dll = oodle_dir.join("oodle-network-shared.dll").is_file()
        || bin_dir.join("oodle-network-shared.dll").is_file();
    if !has_dll {
        return None;
    }
    read_version_marker(&oodle_dir)
}

fn temp_download_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "foundation_studio_dl_{}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        name
    ))
}

fn emit_progress(
    app: Option<&AppHandle>,
    component: &str,
    phase: &str,
    percent: Option<u8>,
    bytes_downloaded: u64,
    bytes_total: Option<u64>,
    detail: String,
) {
    let Some(handle) = app else {
        return;
    };
    let _ = handle.emit(
        "setup-progress",
        SetupProgressEvent {
            component: component.to_string(),
            phase: phase.to_string(),
            percent,
            bytes_downloaded,
            bytes_total,
            detail,
        },
    );
}

pub fn verify_bytes_sha256(bytes: &[u8], expected_hex: &str) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = format!("{:x}", hasher.finalize());
    let expected = expected_hex.trim().to_ascii_lowercase();
    if actual == expected {
        Ok(actual)
    } else {
        Err(format!(
            "SHA-256 verification failed (expected {expected}, got {actual})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn make_zip(entries: &[(&str, &[u8])]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "foundation_studio_setup_test_{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        let zip_path = dir.join(format!(
            "test_{}.zip",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let file = File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, data) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
        zip_path
    }

    #[test]
    fn extracts_nested_oodle_dll_and_rejects_zip_slip() {
        let base =
            std::env::temp_dir().join(format!("foundation_setup_oodle_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let good_zip = make_zip(&[
            ("msvc-x64-release/readme.txt", b"hello"),
            (
                "msvc-x64-release/bin/oodle-network-shared.dll",
                b"oodle-dll-bytes",
            ),
        ]);
        let dest1 = base.join("oodle").join("oodle-network-shared.dll");
        let dest2 = base.join("bin").join("oodle-network-shared.dll");
        extract_oodle_dll_from_zip(&good_zip, &[dest1.clone(), dest2.clone()]).unwrap();
        assert_eq!(fs::read(&dest1).unwrap(), b"oodle-dll-bytes");
        assert_eq!(fs::read(&dest2).unwrap(), b"oodle-dll-bytes");

        let slip_zip = make_zip(&[("../../evil.txt", b"bad")]);
        let err = extract_zip_to_dir(&slip_zip, &base.join("out"), false, &[]).unwrap_err();
        assert!(err.contains("unsafe path traversal"));

        let _ = fs::remove_file(&good_zip);
        let _ = fs::remove_file(&slip_zip);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn strips_github_archive_prefix_and_preserves_existing_user_files() {
        let base =
            std::env::temp_dir().join(format!("foundation_setup_strip_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let target = base.join("target");
        fs::create_dir_all(target.join("vendor")).unwrap();
        fs::write(target.join("config.yaml"), b"user-config").unwrap();
        fs::write(target.join("vendor").join("keep.txt"), b"keep-me").unwrap();

        let zip_path = make_zip(&[
            ("rmp-bridge-main/README.md", b"new-readme"),
            ("rmp-bridge-main/scripts/prepare-shadow.ps1", b"ps1"),
            ("rmp-bridge-main/config.yaml", b"overwrite-attempt"),
            ("rmp-bridge-main/vendor/keep.txt", b"overwrite-vendor"),
        ]);

        extract_zip_to_dir(&zip_path, &target, true, &["config.yaml", "vendor"]).unwrap();
        assert_eq!(fs::read(target.join("README.md")).unwrap(), b"new-readme");
        assert_eq!(
            fs::read(target.join("scripts").join("prepare-shadow.ps1")).unwrap(),
            b"ps1"
        );
        assert_eq!(
            fs::read(target.join("config.yaml")).unwrap(),
            b"user-config"
        );
        assert_eq!(
            fs::read(target.join("vendor").join("keep.txt")).unwrap(),
            b"keep-me"
        );

        let _ = fs::remove_file(&zip_path);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_foundation_and_bridge_extraction_into_dev_checkouts() {
        let base =
            std::env::temp_dir().join(format!("foundation_setup_devguard_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let dev_foundation = base.join("Foundation");
        let dev_bridge = base.join("rmp-bridge");
        let cargo_only = base.join("cargo-only");
        let plain = base.join("plain");
        fs::create_dir_all(dev_foundation.join(".git")).unwrap();
        fs::create_dir_all(dev_bridge.join(".git")).unwrap();
        fs::create_dir_all(&cargo_only).unwrap();
        fs::write(cargo_only.join("Cargo.toml"), "[workspace]").unwrap();
        fs::create_dir_all(&plain).unwrap();

        let err = guard_dev_checkout_target("foundation", &dev_foundation, &plain).unwrap_err();
        assert!(err.starts_with("dev_checkout: "));
        assert!(err.contains("Foundation"));
        assert!(guard_dev_checkout_target("foundation", &cargo_only, &plain).is_err());
        let err = guard_dev_checkout_target("rmp_bridge", &plain, &dev_bridge).unwrap_err();
        assert!(err.starts_with("dev_checkout: "));
        // Cargo.toml alone does not mark a bridge checkout.
        assert!(guard_dev_checkout_target("rmp_bridge", &plain, &cargo_only).is_ok());
        // Oodle / UE4SS / config stay allowed in dev checkouts.
        for comp in ["oodle", "ue4ss", "config"] {
            assert!(guard_dev_checkout_target(comp, &dev_foundation, &dev_bridge).is_ok());
        }
        assert!(guard_dev_checkout_target("foundation", &plain, &plain).is_ok());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn foundation_update_preserves_databases_config_and_oodle() {
        let base =
            std::env::temp_dir().join(format!("foundation_setup_preserve_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("oodle")).unwrap();
        fs::write(base.join("config.yaml"), b"mine").unwrap();
        fs::write(base.join("world.db"), b"world").unwrap();
        fs::write(base.join("Custom.DB"), b"custom").unwrap();
        fs::write(base.join("oodle").join("oodle-network-shared.dll"), b"dll").unwrap();

        let zip_path = make_zip(&[
            ("kawari-run.exe", b"new"),
            ("config.yaml", b"theirs"),
            ("world.db", b"empty"),
            ("custom.db", b"empty"),
            ("fresh.db", b"fresh"),
            ("oodle/oodle-network-shared.dll", b"other"),
        ]);
        extract_zip_to_dir(&zip_path, &base, false, FOUNDATION_PRESERVED).unwrap();
        assert_eq!(fs::read(base.join("kawari-run.exe")).unwrap(), b"new");
        assert_eq!(fs::read(base.join("config.yaml")).unwrap(), b"mine");
        assert_eq!(fs::read(base.join("world.db")).unwrap(), b"world");
        assert_eq!(fs::read(base.join("Custom.DB")).unwrap(), b"custom");
        assert_eq!(fs::read(base.join("fresh.db")).unwrap(), b"fresh");
        assert_eq!(
            fs::read(base.join("oodle").join("oodle-network-shared.dll")).unwrap(),
            b"dll"
        );
        assert!(BRIDGE_PRESERVED.contains(&"vendor"));
        assert!(BRIDGE_PRESERVED.contains(&"runtime"));
        assert!(BRIDGE_PRESERVED.contains(&"dumps"));
        let _ = fs::remove_file(&zip_path);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verifies_sha256_helper_matches_and_detects_mismatch() {
        let sample = b"foundation-studio-sha256-test";
        let expected = format!("{:x}", Sha256::digest(sample));
        assert!(verify_bytes_sha256(sample, &expected.to_ascii_uppercase()).is_ok());
        assert!(verify_bytes_sha256(sample, &"0".repeat(64)).is_err());
    }

    #[test]
    fn generates_localhost_config_yaml_from_game_exe_and_respects_overwrite() {
        let base =
            std::env::temp_dir().join(format!("foundation_setup_cfg_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let game_dir = base.join("FFXIV").join("game");
        let foundation_dir = base.join("foundation");
        fs::create_dir_all(&game_dir).unwrap();
        let exe = game_dir.join("ffxiv_dx11.exe");
        fs::write(&exe, b"exe").unwrap();

        let cfg_path =
            generate_foundation_config(&foundation_dir, &exe.to_string_lossy(), false).unwrap();
        let first_text = fs::read_to_string(&cfg_path).unwrap();
        assert!(first_text.contains("game_path:"));
        assert!(first_text.contains("http://127.0.0.1:21062"));
        assert_eq!(
            first_text.matches("listen_address: \"127.0.0.1\"").count(),
            config_yaml::LISTEN_SECTIONS.len()
        );
        for section in config_yaml::LISTEN_SECTIONS {
            assert_eq!(
                config_yaml::yaml_section_value(&first_text, section, "listen_address").as_deref(),
                Some("127.0.0.1"),
                "section {section}"
            );
        }
        assert!(config_yaml::config_file_is_localhost_only(&cfg_path));

        fs::write(&cfg_path, "custom: keep\n").unwrap();
        generate_foundation_config(&foundation_dir, &exe.to_string_lossy(), false).unwrap();
        assert_eq!(fs::read_to_string(&cfg_path).unwrap(), "custom: keep\n");
        assert!(!config_yaml::config_file_is_localhost_only(&cfg_path));

        generate_foundation_config(&foundation_dir, &exe.to_string_lossy(), true).unwrap();
        assert!(
            fs::read_to_string(&cfg_path)
                .unwrap()
                .contains("game_path:")
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn bridge_extraction_filters_out_dev_and_relay_files() {
        let base = std::env::temp_dir().join(format!(
            "foundation_setup_bridge_filter_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        let target = base.join("bridge");

        let zip_path = make_zip(&[
            ("rmp-bridge-main/scripts/prepare-shadow.ps1", b"ps1"),
            (
                "rmp-bridge-main/mods/FoundationBridge/Scripts/main.lua",
                b"lua",
            ),
            ("rmp-bridge-main/mods/FoundationBridge/enabled.txt", b""),
            ("rmp-bridge-main/LICENSE", b"gpl"),
            ("rmp-bridge-main/README.md", b"readme"),
            ("rmp-bridge-main/relay/Cargo.toml", b"[package]"),
            ("rmp-bridge-main/relay/src/main.rs", b"fn main() {}"),
            (
                "rmp-bridge-main/mods/FoundationDumper/Scripts/main.lua",
                b"dumper",
            ),
            ("rmp-bridge-main/scripts/run-dumper.ps1", b"dumper-ps1"),
            (
                "rmp-bridge-main/scripts/launch-rmp-bridge.ps1",
                b"launch-ps1",
            ),
            ("rmp-bridge-main/.gitignore", b"target/"),
            ("rmp-bridge-main/.github/workflows/release.yml", b"name: r"),
        ]);

        extract_zip_to_dir_filtered(
            &zip_path,
            &target,
            true,
            BRIDGE_PRESERVED,
            is_rmp_bridge_runtime_path,
        )
        .unwrap();

        assert!(target.join("scripts").join("prepare-shadow.ps1").is_file());
        assert!(
            target
                .join("mods")
                .join("FoundationBridge")
                .join("Scripts")
                .join("main.lua")
                .is_file()
        );
        assert!(
            target
                .join("mods")
                .join("FoundationBridge")
                .join("enabled.txt")
                .is_file()
        );
        assert!(target.join("LICENSE").is_file());
        assert!(target.join("README.md").is_file());

        assert!(!target.join("relay").exists());
        assert!(!target.join("mods").join("FoundationDumper").exists());
        assert!(!target.join("scripts").join("run-dumper.ps1").exists());
        assert!(
            !target
                .join("scripts")
                .join("launch-rmp-bridge.ps1")
                .exists()
        );
        assert!(!target.join(".gitignore").exists());
        assert!(!target.join(".github").exists());

        let _ = fs::remove_file(&zip_path);
        let _ = fs::remove_dir_all(&base);
    }
}
