//! ReMakePlace installer / updater.
//!
//! Behaviour (independently implemented, modelled after the public RemakePlaceAutoupdater flow):
//! 1. Query the latest `RemakePlace/app` GitHub release and pick its `.7z` (or `.zip`) asset.
//! 2. Download into `%LocalAppData%\FoundationStudio\cache\` with HTTP Range resume (`.part`),
//!    verifying the final size and the GitHub SHA-256 `digest` when present.
//! 3. Extract into a staging folder next to the install target (path traversal rejected) and
//!    strip a single wrapper folder if present.
//! 4. Move user data (`Makeplace/Custom`, `Makeplace/Save`, `Makeplace/config.json`) aside,
//!    replace the program files, merge the user data back, then write a version marker.
//!
//! The cached archive is kept on failure (so a retry can resume / reuse it) and deleted after a
//! successful install.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::AppHandle;

use super::{
    BusyGuard, GithubAsset, SetupManager, SetupStatus, emit_progress, extract_zip_to_dir,
    parse_sha256_digest,
};
use crate::rmp::locator::try_resolve_remakeplace_source_dir;
use crate::settings::managed_cache_dir;

pub const REMAKEPLACE_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/RemakePlace/app/releases/latest";
pub const REMAKEPLACE_MARKER_FILE: &str = ".foundation-studio-remakeplace.json";
pub const COMPONENT: &str = "remakeplace";

/// User data inside the inner `Makeplace` folder that survives updates.
const PRESERVED_USER_ENTRIES: [&str; 3] = ["Custom", "Save", "config.json"];
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const MAX_DOWNLOAD_ATTEMPTS: u32 = 5;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemakePlaceMarker {
    pub tag: String,
    #[serde(default)]
    pub asset_name: String,
    /// Unix epoch seconds.
    #[serde(default)]
    pub installed_at: u64,
    #[serde(default)]
    pub source: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemakePlaceInstallReport {
    pub installed: bool,
    pub up_to_date: bool,
    pub version: Option<String>,
    pub previous_version: Option<String>,
    pub install_dir: String,
    pub asset_name: Option<String>,
    pub status: Option<SetupStatus>,
}

// ---------------------------------------------------------------------------------------------
// Marker helpers
// ---------------------------------------------------------------------------------------------

pub fn read_marker(dir: &Path) -> Option<RemakePlaceMarker> {
    let raw = fs::read_to_string(dir.join(REMAKEPLACE_MARKER_FILE)).ok()?;
    serde_json::from_str::<RemakePlaceMarker>(&raw)
        .ok()
        .filter(|m| !m.tag.trim().is_empty())
}

pub fn write_marker(dir: &Path, marker: &RemakePlaceMarker) -> Result<(), String> {
    let json = serde_json::to_string_pretty(marker)
        .map_err(|e| format!("Failed serializing ReMakePlace marker: {e}"))?;
    fs::write(dir.join(REMAKEPLACE_MARKER_FILE), json)
        .map_err(|e| format!("Failed writing ReMakePlace version marker: {e}"))
}

/// Installed ReMakePlace version (marker tag) for a configured path (root, inner dir, or exe).
pub fn read_installed_version(configured_path: &str) -> Option<String> {
    if let Some(root) = try_resolve_remakeplace_source_dir(configured_path)
        && let Some(m) = read_marker(&root)
    {
        return Some(m.tag);
    }
    let trimmed = configured_path.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return None;
    }
    read_marker(Path::new(trimmed)).map(|m| m.tag)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------------------------

/// Case-insensitive lookup of a direct child.
pub fn find_child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.exists() {
        // On case-insensitive filesystems prefer the on-disk spelling.
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(name)
                {
                    return Some(entry.path());
                }
            }
        }
        return Some(exact);
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            return Some(entry.path());
        }
    }
    None
}

/// A valid payload has `Makeplace.exe` and `Makeplace/Content` (case-insensitive).
pub fn validate_payload(dir: &Path) -> Result<(), String> {
    let exe_ok = find_child_ci(dir, "Makeplace.exe").is_some_and(|p| p.is_file());
    let content_ok = find_child_ci(dir, "Makeplace")
        .filter(|p| p.is_dir())
        .and_then(|inner| find_child_ci(&inner, "Content"))
        .is_some_and(|p| p.is_dir());
    if exe_ok && content_ok {
        Ok(())
    } else {
        Err(
            "The ReMakePlace archive does not contain Makeplace.exe and Makeplace/Content."
                .to_string(),
        )
    }
}

/// Returns the payload root inside `staging`, stripping a single top-level wrapper folder.
pub fn locate_payload_root(staging: &Path) -> Result<PathBuf, String> {
    if validate_payload(staging).is_ok() {
        return Ok(staging.to_path_buf());
    }
    let entries: Vec<PathBuf> = fs::read_dir(staging)
        .map_err(|e| format!("Failed reading staging folder: {e}"))?
        .flatten()
        .map(|e| e.path())
        .collect();
    if entries.len() == 1 && entries[0].is_dir() {
        validate_payload(&entries[0])?;
        return Ok(entries[0].clone());
    }
    validate_payload(staging).map(|_| staging.to_path_buf())
}

/// Sanitizes an archive entry name into a safe relative path, or `None` if it is unsafe.
pub fn sanitize_entry_name(name: &str) -> Option<PathBuf> {
    let normalized = name.replace('\\', "/");
    if normalized.contains(':') {
        return None;
    }
    let mut out = PathBuf::new();
    for comp in Path::new(&normalized).components() {
        match comp {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Extracts a `.7z` archive into `dest`, rejecting traversal entries before writing anything.
pub fn extract_7z_archive(
    archive_path: &Path,
    dest: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    use sevenz_rust2::{ArchiveReader, Password};

    fs::create_dir_all(dest)
        .map_err(|e| format!("Failed creating staging folder {}: {e}", dest.display()))?;
    let mut reader = ArchiveReader::open(archive_path, Password::empty())
        .map_err(|e| format!("Invalid 7z archive: {e}"))?;

    let mut total: u64 = 0;
    for entry in &reader.archive().files {
        if sanitize_entry_name(entry.name()).is_none() {
            return Err("Refusing archive with unsafe path traversal entry.".to_string());
        }
        total = total.saturating_add(entry.size());
    }

    let mut done: u64 = 0;
    let mut buf = vec![0u8; 256 * 1024];
    reader
        .for_each_entries(|entry, rd| {
            let Some(rel) = sanitize_entry_name(entry.name()) else {
                return Err(sevenz_rust2::Error::Other(
                    "unsafe path traversal entry".into(),
                ));
            };
            let out_path = dest.join(rel);
            if entry.is_directory() {
                fs::create_dir_all(&out_path)?;
                return Ok(true);
            }
            if entry.is_anti_item() {
                return Ok(true);
            }
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = BufWriter::new(File::create(&out_path)?);
            loop {
                let n = rd.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                done = done.saturating_add(n as u64);
                progress(done, total);
            }
            out.flush()?;
            Ok(true)
        })
        .map_err(|e| format!("Failed extracting 7z archive: {e}"))
}

pub fn extract_archive(
    archive_path: &Path,
    dest: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    let lower = archive_path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".zip") {
        extract_zip_to_dir(archive_path, dest, false, &[])
    } else {
        extract_7z_archive(archive_path, dest, progress)
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else if path.exists() {
        fs::remove_file(path)
    } else {
        Ok(())
    }
}

/// Moves `src` to `dst` (rename, falling back to copy + delete across volumes).
fn move_path(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    if src.is_dir() {
        copy_dir_recursive(src, dst)?;
        fs::remove_dir_all(src)
    } else {
        fs::copy(src, dst)?;
        fs::remove_file(src)
    }
}

/// Merges `src` into `dst`; entries from `src` win. `src` is consumed.
fn merge_move(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        return move_path(src, dst);
    }
    if src.is_dir() && dst.is_dir() {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            let target =
                find_child_ci(dst, &name.to_string_lossy()).unwrap_or_else(|| dst.join(&name));
            merge_move(&entry.path(), &target)?;
        }
        let _ = fs::remove_dir_all(src);
        return Ok(());
    }
    remove_path(dst)?;
    move_path(src, dst)
}

/// Replaces program files in `target` with `payload` while preserving user data.
pub fn install_payload(payload: &Path, target: &Path, backup_root: &Path) -> Result<(), String> {
    fs::create_dir_all(target)
        .map_err(|e| format!("Failed creating install folder {}: {e}", target.display()))?;

    // 1. Move user data aside.
    let mut backed_up: Vec<(String, PathBuf)> = Vec::new();
    if let Some(inner) = find_child_ci(target, "Makeplace").filter(|p| p.is_dir()) {
        for name in PRESERVED_USER_ENTRIES {
            if let Some(src) = find_child_ci(&inner, name) {
                let dst = backup_root.join(name);
                move_path(&src, &dst)
                    .map_err(|e| format!("Failed backing up Makeplace/{name}: {e}"))?;
                backed_up.push((name.to_string(), dst));
            }
        }
    }

    // 2. Replace every top-level entry shipped by the payload.
    let copy_result: Result<(), String> = (|| {
        let entries = fs::read_dir(payload).map_err(|e| format!("Failed reading payload: {e}"))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy().into_owned();
            if let Some(existing) = find_child_ci(target, &name_str) {
                remove_path(&existing)
                    .map_err(|e| format!("Failed removing old {}: {e}", existing.display()))?;
            }
            move_path(&entry.path(), &target.join(&name))
                .map_err(|e| format!("Failed installing {name_str}: {e}"))?;
        }
        Ok(())
    })();

    // 3. Always merge user data back, even if the copy step failed.
    let inner = find_child_ci(target, "Makeplace")
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| target.join("MakePlace"));
    let mut restore_errors = Vec::new();
    for (name, backup) in &backed_up {
        let dst = find_child_ci(&inner, name).unwrap_or_else(|| inner.join(name));
        if let Err(e) = merge_move(backup, &dst) {
            restore_errors.push(format!(
                "Failed restoring Makeplace/{name} (backup kept at {}): {e}",
                backup.display()
            ));
        }
    }
    if restore_errors.is_empty() {
        let _ = fs::remove_dir_all(backup_root);
    }

    copy_result?;
    if !restore_errors.is_empty() {
        return Err(restore_errors.join(" / "));
    }
    Ok(())
}

/// Refuses obviously wrong targets such as drive roots or unrelated non-empty folders.
pub fn check_install_target(target: &Path) -> Result<(), String> {
    if !target.is_absolute() || target.parent().is_none() {
        return Err("Please choose a dedicated absolute folder for ReMakePlace.".to_string());
    }
    if target.exists() && !target.is_dir() {
        return Err(format!("{} is not a folder.", target.display()));
    }
    if target.is_dir() {
        let non_empty = fs::read_dir(target)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        let looks_like_rmp = read_marker(target).is_some()
            || find_child_ci(target, "Makeplace.exe").is_some()
            || find_child_ci(target, "Makeplace").is_some_and(|p| p.is_dir());
        if non_empty && !looks_like_rmp {
            return Err(
                "The selected folder is not empty and does not look like a ReMakePlace installation. Choose an empty folder or an existing ReMakePlace folder."
                    .to_string(),
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Resumable download helpers
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumePlan {
    Fresh,
    Resume(u64),
    Complete,
}

/// Decides how to continue given the existing `.part` length and the expected total size.
pub fn plan_resume(existing: u64, expected_total: Option<u64>) -> ResumePlan {
    match expected_total {
        _ if existing == 0 => ResumePlan::Fresh,
        Some(total) if existing == total => ResumePlan::Complete,
        Some(total) if existing > total => ResumePlan::Fresh,
        _ => ResumePlan::Resume(existing),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RangeOutcome {
    /// Server honoured the range; append to the `.part` file.
    Append,
    /// Server sent the whole body; truncate and start from zero.
    Restart,
    /// Nothing left to fetch.
    AlreadyComplete,
    /// The `.part` file is unusable (e.g. 416 with a size mismatch); delete it and retry.
    Discard,
    Fail(String),
}

pub fn parse_content_range_total(header: &str) -> Option<u64> {
    // e.g. "bytes 100-199/2000" or "bytes */2000"
    header.rsplit('/').next()?.trim().parse::<u64>().ok()
}

pub fn parse_content_range_start(header: &str) -> Option<u64> {
    let spec = header.trim().strip_prefix("bytes")?.trim();
    let (range, _) = spec.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    start.trim().parse::<u64>().ok()
}

pub fn classify_range_response(
    status: u16,
    requested_from: u64,
    content_range: Option<&str>,
    expected_total: Option<u64>,
) -> RangeOutcome {
    match status {
        206 if requested_from > 0 => match content_range.and_then(parse_content_range_start) {
            Some(start) if start == requested_from => RangeOutcome::Append,
            _ => RangeOutcome::Discard,
        },
        200 | 206 => RangeOutcome::Restart,
        416 => {
            let server_total = content_range
                .and_then(parse_content_range_total)
                .or(expected_total);
            if server_total == Some(requested_from) {
                RangeOutcome::AlreadyComplete
            } else {
                RangeOutcome::Discard
            }
        }
        other => RangeOutcome::Fail(format!("Download failed with HTTP {other}")),
    }
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("Failed opening download: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Failed reading download: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn percent(done: u64, total: Option<u64>, lo: u8, hi: u8) -> Option<u8> {
    let total = total.filter(|t| *t > 0)?;
    let span = (hi - lo) as u64;
    Some((lo as u64 + done.min(total).saturating_mul(span) / total) as u8)
}

fn pick_asset(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase().ends_with(".7z"))
        .or_else(|| {
            assets
                .iter()
                .find(|a| a.name.to_ascii_lowercase().ends_with(".zip"))
        })
}

fn safe_file_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

impl SetupManager {
    /// Latest ReMakePlace release tag (used by the update checker).
    pub async fn latest_remakeplace_tag(&self) -> Result<Option<String>, String> {
        let rel = self
            .fetch_latest_github_release(REMAKEPLACE_LATEST_RELEASE_API)
            .await?;
        Ok(rel.tag_name.filter(|t| !t.trim().is_empty()))
    }

    pub async fn install_remakeplace(
        &self,
        app: Option<&AppHandle>,
        target_dir: &Path,
        repair: bool,
    ) -> Result<RemakePlaceInstallReport, String> {
        if self
            .busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return Err("Runtime setup is already in progress.".to_string());
        }
        let _guard = BusyGuard(&self.busy);

        let res = self
            .install_remakeplace_inner(app, target_dir, repair)
            .await;
        match &res {
            Ok(report) => emit_progress(
                app,
                COMPONENT,
                if report.up_to_date {
                    "up_to_date"
                } else {
                    "done"
                },
                Some(100),
                0,
                None,
                report.version.clone().unwrap_or_default(),
            ),
            Err(err) => emit_progress(app, COMPONENT, "error", None, 0, None, err.clone()),
        }
        res
    }

    async fn install_remakeplace_inner(
        &self,
        app: Option<&AppHandle>,
        target_dir: &Path,
        repair: bool,
    ) -> Result<RemakePlaceInstallReport, String> {
        check_install_target(target_dir)?;
        let previous = read_marker(target_dir).map(|m| m.tag);

        emit_progress(
            app,
            COMPONENT,
            "checking",
            None,
            0,
            None,
            "Checking latest RemakePlace/app release...".to_string(),
        );
        let release = self
            .fetch_latest_github_release(REMAKEPLACE_LATEST_RELEASE_API)
            .await?;
        let tag = release
            .tag_name
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let asset = pick_asset(&release.assets).ok_or_else(|| {
            "Latest ReMakePlace release does not contain a .7z or .zip asset.".to_string()
        })?;

        let installed_ok = validate_payload(target_dir).is_ok()
            && try_resolve_remakeplace_source_dir(&target_dir.to_string_lossy()).is_some();
        if !repair && installed_ok && previous.as_deref() == Some(tag.as_str()) {
            return Ok(RemakePlaceInstallReport {
                installed: false,
                up_to_date: true,
                version: Some(tag),
                previous_version: previous,
                install_dir: target_dir.to_string_lossy().into_owned(),
                asset_name: Some(asset.name.clone()),
                status: None,
            });
        }

        let cache_dir = managed_cache_dir();
        fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("Failed creating download cache folder: {e}"))?;
        let archive_path = cache_dir.join(safe_file_name(&asset.name));
        let expected_sha = parse_sha256_digest(asset.digest.as_deref());
        if repair {
            let _ = fs::remove_file(&archive_path);
        }
        self.download_resumable(
            app,
            COMPONENT,
            &asset.browser_download_url,
            &archive_path,
            asset.size.filter(|s| *s > 0),
            expected_sha.as_deref(),
        )
        .await?;

        // Staging + backup live next to the install folder so moves stay on one volume.
        let parent = target_dir
            .parent()
            .ok_or_else(|| "Install folder has no parent directory.".to_string())?
            .to_path_buf();
        let stamp = format!("{}-{}", std::process::id(), now_unix());
        let staging = parent.join(format!(".fs-rmp-staging-{stamp}"));
        let backup = parent.join(format!(".fs-rmp-backup-{stamp}"));
        let _ = fs::remove_dir_all(&staging);

        emit_progress(
            app,
            COMPONENT,
            "extracting",
            Some(0),
            0,
            None,
            "Extracting ReMakePlace archive...".to_string(),
        );
        let app_owned = app.cloned();
        let archive_clone = archive_path.clone();
        let staging_clone = staging.clone();
        let extract_res = tokio::task::spawn_blocking(move || {
            let mut last = Instant::now();
            extract_archive(&archive_clone, &staging_clone, |done, total| {
                if last.elapsed() >= PROGRESS_INTERVAL {
                    last = Instant::now();
                    emit_progress(
                        app_owned.as_ref(),
                        COMPONENT,
                        "extracting",
                        percent(done, Some(total), 0, 99),
                        done,
                        Some(total),
                        "Extracting ReMakePlace archive...".to_string(),
                    );
                }
            })
        })
        .await
        .map_err(|e| format!("Extraction task failed: {e}"))
        .and_then(|r| r);
        if let Err(err) = extract_res {
            let _ = fs::remove_dir_all(&staging);
            return Err(err);
        }

        let payload = match locate_payload_root(&staging) {
            Ok(p) => p,
            Err(err) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(err);
            }
        };

        emit_progress(
            app,
            COMPONENT,
            "installing",
            None,
            0,
            None,
            "Installing ReMakePlace (preserving Custom/Save)...".to_string(),
        );
        let target_owned = target_dir.to_path_buf();
        let backup_clone = backup.clone();
        let install_res = tokio::task::spawn_blocking(move || {
            install_payload(&payload, &target_owned, &backup_clone)
        })
        .await
        .map_err(|e| format!("Install task failed: {e}"))
        .and_then(|r| r);
        let _ = fs::remove_dir_all(&staging);
        install_res?;

        validate_payload(target_dir)?;
        if try_resolve_remakeplace_source_dir(&target_dir.to_string_lossy()).is_none() {
            return Err(
                "Installed ReMakePlace is missing MakePlace/Binaries/Win64/MakePlace-Win64-Shipping.exe."
                    .to_string(),
            );
        }

        write_marker(
            target_dir,
            &RemakePlaceMarker {
                tag: tag.clone(),
                asset_name: asset.name.clone(),
                installed_at: now_unix(),
                source: "github:RemakePlace/app".to_string(),
            },
        )?;
        let _ = fs::remove_file(&archive_path);

        Ok(RemakePlaceInstallReport {
            installed: true,
            up_to_date: false,
            version: Some(tag),
            previous_version: previous,
            install_dir: target_dir.to_string_lossy().into_owned(),
            asset_name: Some(asset.name.clone()),
            status: None,
        })
    }

    /// Downloads `url` into `dest` via `<dest>.part` with HTTP Range resume and verification.
    pub async fn download_resumable(
        &self,
        app: Option<&AppHandle>,
        component: &str,
        url: &str,
        dest: &Path,
        expected_size: Option<u64>,
        expected_sha256: Option<&str>,
    ) -> Result<(), String> {
        let file_name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".to_string());
        let part = dest.with_file_name(format!("{file_name}.part"));

        // Reuse a previously completed archive (e.g. when a later install step failed).
        if dest.is_file() {
            let len = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            if expected_size.is_none_or(|s| s == len)
                && self
                    .verify_file_sha(app, component, dest, len, expected_sha256)
                    .await
                    .is_ok()
            {
                return Ok(());
            }
            let _ = fs::remove_file(dest);
        }

        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let existing = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            let from = match plan_resume(existing, expected_size) {
                ResumePlan::Complete => break,
                ResumePlan::Fresh => {
                    let _ = fs::remove_file(&part);
                    0
                }
                ResumePlan::Resume(n) => n,
            };

            let mut req = self.download_http.get(url);
            if from > 0 {
                req = req.header(reqwest::header::RANGE, format!("bytes={from}-"));
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) if attempt < MAX_DOWNLOAD_ATTEMPTS => {
                    tracing::warn!("Download request failed (attempt {attempt}): {e}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                Err(e) => return Err(format!("Download request failed ({url}): {e}")),
            };

            let status = resp.status().as_u16();
            let content_range = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let outcome =
                classify_range_response(status, from, content_range.as_deref(), expected_size);
            let (mut file, mut downloaded) = match outcome {
                RangeOutcome::Append => (
                    OpenOptions::new()
                        .append(true)
                        .open(&part)
                        .map_err(|e| format!("Failed opening partial download: {e}"))?,
                    from,
                ),
                RangeOutcome::Restart => (
                    File::create(&part)
                        .map_err(|e| format!("Failed creating download file: {e}"))?,
                    0,
                ),
                RangeOutcome::AlreadyComplete => break,
                RangeOutcome::Discard => {
                    let _ = fs::remove_file(&part);
                    if attempt < MAX_DOWNLOAD_ATTEMPTS {
                        continue;
                    }
                    return Err("Server rejected the resume request.".to_string());
                }
                RangeOutcome::Fail(msg) => {
                    if (500..600).contains(&status) && attempt < MAX_DOWNLOAD_ATTEMPTS {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    return Err(format!("{msg} ({url})"));
                }
            };

            let total = expected_size
                .or_else(|| content_range.as_deref().and_then(parse_content_range_total))
                .or_else(|| resp.content_length().map(|l| l + downloaded));
            let mut stream = resp.bytes_stream();
            let mut last_emit = Instant::now() - PROGRESS_INTERVAL;
            let mut stream_error: Option<String> = None;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        stream_error = Some(format!("Download stream error: {e}"));
                        break;
                    }
                };
                file.write_all(&chunk)
                    .map_err(|e| format!("Failed writing download file: {e}"))?;
                downloaded += chunk.len() as u64;
                if last_emit.elapsed() >= PROGRESS_INTERVAL {
                    last_emit = Instant::now();
                    emit_progress(
                        app,
                        component,
                        "downloading",
                        percent(downloaded, total, 0, 99),
                        downloaded,
                        total,
                        file_name.clone(),
                    );
                }
            }
            file.flush()
                .map_err(|e| format!("Failed flushing download file: {e}"))?;
            drop(file);

            let incomplete = expected_size.is_some_and(|s| downloaded < s);
            if stream_error.is_some() || incomplete {
                if attempt < MAX_DOWNLOAD_ATTEMPTS {
                    tracing::warn!(
                        "Download interrupted at {downloaded} bytes (attempt {attempt}); resuming"
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                return Err(stream_error.unwrap_or_else(|| {
                    "Download ended before the expected size was reached.".to_string()
                }));
            }
            break;
        }

        let final_len = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if let Some(size) = expected_size
            && final_len != size
        {
            let _ = fs::remove_file(&part);
            return Err(format!(
                "Downloaded size mismatch (expected {size} bytes, got {final_len})."
            ));
        }
        if let Err(err) = self
            .verify_file_sha(app, component, &part, final_len, expected_sha256)
            .await
        {
            let _ = fs::remove_file(&part);
            return Err(err);
        }
        fs::rename(&part, dest).map_err(|e| format!("Failed finalizing download: {e}"))?;
        Ok(())
    }

    async fn verify_file_sha(
        &self,
        app: Option<&AppHandle>,
        component: &str,
        path: &Path,
        len: u64,
        expected_sha256: Option<&str>,
    ) -> Result<(), String> {
        let Some(expected) = expected_sha256 else {
            return Ok(());
        };
        emit_progress(
            app,
            component,
            "verifying",
            Some(99),
            len,
            Some(len),
            "Verifying SHA-256 checksum...".to_string(),
        );
        let owned = path.to_path_buf();
        let actual = tokio::task::spawn_blocking(move || sha256_file(&owned))
            .await
            .map_err(|e| format!("Checksum task failed: {e}"))??;
        let expected_norm = expected.trim().to_ascii_lowercase();
        if actual == expected_norm {
            Ok(())
        } else {
            Err(format!(
                "SHA-256 verification failed (expected {expected_norm}, got {actual})"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};

    fn temp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "foundation_rmp_{tag}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn make_7z(path: &Path, files: &[(&str, &[u8])]) {
        let mut writer = ArchiveWriter::create(path).unwrap();
        for (name, data) in files {
            writer
                .push_archive_entry(ArchiveEntry::new_file(name), Some(*data))
                .unwrap();
        }
        writer.finish().unwrap();
    }

    const SHIPPING: &str = "MakePlace/Binaries/Win64/MakePlace-Win64-Shipping.exe";

    #[test]
    fn extracts_wrapped_7z_strips_wrapper_and_preserves_user_data() {
        let base = temp_base("install");
        let archive = base.join("rmp.7z");
        let wrapped_shipping = format!("ReMakeplace-V7/{SHIPPING}");
        make_7z(
            &archive,
            &[
                ("ReMakeplace-V7/Makeplace.exe", b"new-exe"),
                ("ReMakeplace-V7/MakePlace/Content/Paks/a.pak", b"new-pak"),
                (wrapped_shipping.as_str(), b"shipping"),
                (
                    "ReMakeplace-V7/MakePlace/Custom/default.json",
                    b"default-custom",
                ),
                (
                    "ReMakeplace-V7/MakePlace/Save/shipped.json",
                    b"shipped-save",
                ),
            ],
        );

        // Existing install with user data and a stale pak.
        let target = base.join("ReMakePlace");
        fs::create_dir_all(target.join("MakePlace").join("Save")).unwrap();
        fs::create_dir_all(target.join("MakePlace").join("Custom")).unwrap();
        fs::create_dir_all(target.join("MakePlace").join("Content").join("Paks")).unwrap();
        fs::write(target.join("MakePlace.exe"), b"old-exe").unwrap();
        fs::write(
            target.join("MakePlace").join("Save").join("mine.json"),
            b"user-save",
        )
        .unwrap();
        fs::write(
            target.join("MakePlace").join("Save").join("shipped.json"),
            b"user-override",
        )
        .unwrap();
        fs::write(
            target.join("MakePlace").join("Custom").join("item.json"),
            b"user-custom",
        )
        .unwrap();
        fs::write(target.join("MakePlace").join("config.json"), b"user-config").unwrap();
        fs::write(
            target
                .join("MakePlace")
                .join("Content")
                .join("Paks")
                .join("stale.pak"),
            b"stale",
        )
        .unwrap();

        let staging = base.join(".staging");
        let mut last = (0, 0);
        extract_7z_archive(&archive, &staging, |d, t| last = (d, t)).unwrap();
        assert_eq!(last.0, last.1);
        let payload = locate_payload_root(&staging).unwrap();
        assert!(payload.ends_with("ReMakeplace-V7"));

        check_install_target(&target).unwrap();
        install_payload(&payload, &target, &base.join(".backup")).unwrap();

        let exe = find_child_ci(&target, "Makeplace.exe").unwrap();
        assert_eq!(fs::read(exe).unwrap(), b"new-exe");
        let inner = find_child_ci(&target, "Makeplace").unwrap();
        assert_eq!(
            fs::read(inner.join("Save").join("mine.json")).unwrap(),
            b"user-save"
        );
        assert_eq!(
            fs::read(inner.join("Save").join("shipped.json")).unwrap(),
            b"user-override"
        );
        assert_eq!(
            fs::read(inner.join("Custom").join("item.json")).unwrap(),
            b"user-custom"
        );
        assert_eq!(
            fs::read(inner.join("Custom").join("default.json")).unwrap(),
            b"default-custom"
        );
        assert_eq!(fs::read(inner.join("config.json")).unwrap(), b"user-config");
        assert!(
            !inner
                .join("Content")
                .join("Paks")
                .join("stale.pak")
                .exists()
        );
        assert!(inner.join("Content").join("Paks").join("a.pak").is_file());
        assert!(!base.join(".backup").exists());
        assert!(try_resolve_remakeplace_source_dir(&target.to_string_lossy()).is_some());

        write_marker(
            &target,
            &RemakePlaceMarker {
                tag: "v7.57.1".to_string(),
                asset_name: "rmp.7z".to_string(),
                installed_at: 1,
                source: "test".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            read_installed_version(&target.to_string_lossy()).as_deref(),
            Some("v7.57.1")
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rejects_traversal_entries_and_invalid_payloads() {
        let base = temp_base("slip");
        let archive = base.join("evil.7z");
        make_7z(&archive, &[("ok.txt", b"ok"), ("../evil.txt", b"bad")]);
        let err = extract_7z_archive(&archive, &base.join("out"), |_, _| {}).unwrap_err();
        assert!(err.contains("unsafe path traversal"));
        assert!(!base.join("evil.txt").exists());
        assert!(!base.join("out").join("ok.txt").exists());

        assert!(sanitize_entry_name("C:/Windows/x").is_none());
        assert!(sanitize_entry_name("..\\x").is_none());
        assert!(sanitize_entry_name("/abs").is_none());
        assert_eq!(
            sanitize_entry_name("./a\\b.txt"),
            Some(PathBuf::from("a").join("b.txt"))
        );

        let bad = base.join("bad_payload");
        fs::create_dir_all(bad.join("Other")).unwrap();
        assert!(locate_payload_root(&bad).is_err());

        let unrelated = base.join("unrelated");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("notes.txt"), b"x").unwrap();
        assert!(check_install_target(&unrelated).is_err());
        assert!(check_install_target(&base.join("fresh")).is_ok());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn resume_planning_and_range_classification() {
        assert_eq!(plan_resume(0, Some(10)), ResumePlan::Fresh);
        assert_eq!(plan_resume(4, Some(10)), ResumePlan::Resume(4));
        assert_eq!(plan_resume(10, Some(10)), ResumePlan::Complete);
        assert_eq!(plan_resume(12, Some(10)), ResumePlan::Fresh);
        assert_eq!(plan_resume(4, None), ResumePlan::Resume(4));

        assert_eq!(
            classify_range_response(206, 4, Some("bytes 4-9/10"), Some(10)),
            RangeOutcome::Append
        );
        assert_eq!(
            classify_range_response(206, 4, Some("bytes 0-9/10"), Some(10)),
            RangeOutcome::Discard
        );
        assert_eq!(
            classify_range_response(200, 4, None, Some(10)),
            RangeOutcome::Restart
        );
        assert_eq!(
            classify_range_response(200, 0, None, None),
            RangeOutcome::Restart
        );
        assert_eq!(
            classify_range_response(416, 10, Some("bytes */10"), Some(10)),
            RangeOutcome::AlreadyComplete
        );
        assert_eq!(
            classify_range_response(416, 7, Some("bytes */10"), Some(10)),
            RangeOutcome::Discard
        );
        assert!(matches!(
            classify_range_response(404, 0, None, None),
            RangeOutcome::Fail(_)
        ));
        assert_eq!(parse_content_range_total("bytes 4-9/2000"), Some(2000));
        assert_eq!(parse_content_range_start("bytes 4-9/2000"), Some(4));
        assert_eq!(percent(50, Some(100), 0, 99), Some(49));
    }

    #[test]
    fn picks_7z_asset_before_zip() {
        let assets = vec![
            GithubAsset {
                name: "notes.zip".to_string(),
                browser_download_url: "u1".to_string(),
                digest: None,
                size: Some(1),
            },
            GithubAsset {
                name: "ReMakeplace-V7-57-1.7z".to_string(),
                browser_download_url: "u2".to_string(),
                digest: None,
                size: Some(2),
            },
        ];
        assert_eq!(pick_asset(&assets).unwrap().browser_download_url, "u2");
        assert_eq!(pick_asset(&assets[..1]).unwrap().browser_download_url, "u1");
    }
}
