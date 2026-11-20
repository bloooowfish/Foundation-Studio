use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::presets::metadata::{LocalPresetMeta, decode_bytes_with_bom, parse_preset_metadata};
use crate::settings::dedup_nonempty_paths;

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);
const READ_RETRY_ATTEMPTS: usize = 8;
const READ_RETRY_DELAY: Duration = Duration::from_millis(80);
const SUPPRESS_DURATION: Duration = Duration::from_millis(900);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPresetEntry {
    pub path: String,
    pub file_name: String,
    pub title: String,
    pub dir: String,
    pub size_bytes: u64,
    pub modified_unix_ms: u64,
    pub meta: Option<LocalPresetMeta>,
    pub parse_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPresetChangePayload {
    pub kind: String,
    pub path: String,
    pub file_name: String,
    pub entry: Option<LocalPresetEntry>,
}

pub fn read_local_preset_entry(path: &Path) -> Option<LocalPresetEntry> {
    let meta_fs = fs::metadata(path).ok()?;
    if !meta_fs.is_file() {
        return None;
    }
    let file_name = path.file_name()?.to_string_lossy().into_owned();
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.clone());
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let modified_unix_ms = meta_fs
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let (parsed_meta, parse_error) =
        match read_shared_json_text_with_retry(path, 2, Duration::from_millis(25)) {
            Ok(raw) => match parse_preset_metadata(&raw) {
                Ok(m) => (Some(m), None),
                Err(err) => (None, Some(err)),
            },
            Err(err) => (None, Some(err)),
        };

    Some(LocalPresetEntry {
        path: path.to_string_lossy().into_owned(),
        file_name,
        title,
        dir,
        size_bytes: meta_fs.len(),
        modified_unix_ms,
        meta: parsed_meta,
        parse_error,
    })
}

pub fn scan_watched_dirs(dirs: &[String]) -> Vec<LocalPresetEntry> {
    let mut entries = Vec::new();
    let mut seen_files: Vec<String> = Vec::new();

    for dir_str in dedup_nonempty_paths(dirs) {
        let dir = Path::new(&dir_str);
        if !dir.is_dir() {
            continue;
        }
        let Ok(read_dir) = fs::read_dir(dir) else {
            continue;
        };
        for item in read_dir.flatten() {
            let path = item.path();
            if !is_json_preset_file(&path) {
                continue;
            }
            let key = normalize_path_key(&path);
            if seen_files.iter().any(|k| k == &key) {
                continue;
            }
            seen_files.push(key);
            if let Some(entry) = read_local_preset_entry(&path) {
                entries.push(entry);
            }
        }
    }

    entries.sort_by(|a, b| {
        b.modified_unix_ms
            .cmp(&a.modified_unix_ms)
            .then_with(|| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()))
    });
    entries
}

pub fn write_text_atomic(target: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }
    let tmp = target.with_extension("json.tmp");
    fs::write(&tmp, text.as_bytes()).map_err(|e| format!("Failed to write temporary file: {e}"))?;

    for attempt in 0..5 {
        match fs::rename(&tmp, target) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 4 => {
                let _ = fs::remove_file(target);
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => {
                let _ = fs::remove_file(&tmp);
                return Err(format!("Failed to save file ({}): {err}", target.display()));
            }
        }
    }
    Ok(())
}

pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("Path is empty.".to_string());
    }
    let target = fs::canonicalize(path)
        .map(|p| {
            let s = p.to_string_lossy();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
        })
        .unwrap_or_else(|_| path.to_path_buf());

    if !target.exists() {
        return Err(format!("Path does not exist: {}", target.display()));
    }

    if target.is_dir() {
        Command::new("explorer.exe")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("Failed to launch File Explorer: {e}"))?;
    } else {
        Command::new("explorer.exe")
            .arg(format!("/select,{}", target.display()))
            .spawn()
            .map_err(|e| format!("Failed to launch File Explorer: {e}"))?;
    }
    Ok(())
}

pub fn is_json_preset_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

fn read_shared_json_text_with_retry(
    path: &Path,
    attempts: usize,
    delay: Duration,
) -> Result<String, String> {
    let mut last_err = String::from("Failed to read file");
    for idx in 0..attempts {
        let attempt_res = (|| -> Result<String, String> {
            let mut file = fs::File::open(path).map_err(|e| format!("Failed to open file: {e}"))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|e| format!("Failed to read file: {e}"))?;
            let text = decode_bytes_with_bom(&bytes);
            if text.trim().is_empty() {
                return Err("Empty JSON file".to_string());
            }
            serde_json::from_str::<Value>(&text)
                .map_err(|e| format!("Failed to parse JSON: {e}"))?;
            Ok(text)
        })();

        match attempt_res {
            Ok(ok) => return Ok(ok),
            Err(err) => {
                last_err = err;
                if idx + 1 < attempts {
                    thread::sleep(delay);
                }
            }
        }
    }
    Err(last_err)
}

fn normalize_path_key(path: &Path) -> String {
    let abs = fs::canonicalize(path).unwrap_or_else(|_| {
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
            && let Ok(canon_parent) = fs::canonicalize(parent)
        {
            canon_parent.join(name)
        } else {
            path.to_path_buf()
        }
    });
    let s = abs.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_ascii_lowercase()
}

struct WatcherWorker {
    stop_flag: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
    _watcher: RecommendedWatcher,
}

impl WatcherWorker {
    fn stop(mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub struct PresetWatcherManager {
    worker: Mutex<Option<WatcherWorker>>,
    suppressed: Arc<Mutex<HashMap<String, Instant>>>,
    watched_dirs: Arc<RwLock<Vec<String>>>,
}

impl Default for PresetWatcherManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetWatcherManager {
    pub fn new() -> Self {
        Self {
            worker: Mutex::new(None),
            suppressed: Arc::new(Mutex::new(HashMap::new())),
            watched_dirs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn suppress_path(&self, path: &Path) {
        if let Ok(mut guard) = self.suppressed.lock() {
            guard.insert(normalize_path_key(path), Instant::now() + SUPPRESS_DURATION);
        }
    }

    pub fn update_dirs_with_app(&self, dirs: &[String], app: AppHandle) -> Result<(), String> {
        self.update_dirs_with_callback(dirs, move |payload| {
            let _ = app.emit("local-presets-changed", payload);
        })
    }

    pub fn update_dirs_with_callback<F>(
        &self,
        dirs: &[String],
        mut on_change: F,
    ) -> Result<(), String>
    where
        F: FnMut(LocalPresetChangePayload) + Send + 'static,
    {
        let clean_dirs = dedup_nonempty_paths(dirs);
        if let Ok(mut g) = self.watched_dirs.write() {
            *g = clean_dirs.clone();
        }

        // Stop existing watcher cleanly before replacing.
        if let Ok(mut guard) = self.worker.lock()
            && let Some(prev) = guard.take()
        {
            prev.stop();
        }

        let existing_dirs: Vec<PathBuf> = clean_dirs
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect();
        if existing_dirs.is_empty() {
            return Ok(());
        }

        let (raw_tx, raw_rx) = mpsc::channel::<(PathBuf, &'static str)>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(event) = res else {
                return;
            };
            let kind = match event.kind {
                EventKind::Remove(_) => "removed",
                EventKind::Create(_) | EventKind::Modify(_) => "updated",
                _ => return,
            };
            for path in event.paths {
                if is_json_preset_file(&path) {
                    let _ = raw_tx.send((path, kind));
                }
            }
        })
        .map_err(|e| format!("Failed to initialize file watcher: {e}"))?;

        for dir in &existing_dirs {
            if let Err(err) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                tracing::warn!(dir = %dir.display(), "Failed watching preset directory: {err}");
            }
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop_flag);
        let suppressed = Arc::clone(&self.suppressed);

        let join = thread::Builder::new()
            .name("preset-dir-watcher".to_string())
            .spawn(move || {
                let mut pending: HashMap<PathBuf, (Instant, &'static str)> = HashMap::new();

                while !worker_stop.load(Ordering::Relaxed) {
                    loop {
                        match raw_rx.try_recv() {
                            Ok((path, kind)) => {
                                pending.insert(path, (Instant::now() + DEBOUNCE_WINDOW, kind));
                            }
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => return,
                        }
                    }

                    let now = Instant::now();
                    let ready: Vec<(PathBuf, &'static str)> = pending
                        .iter()
                        .filter_map(|(p, (deadline, k))| {
                            if now >= *deadline {
                                Some((p.clone(), *k))
                            } else {
                                None
                            }
                        })
                        .collect();

                    for (path, kind) in ready {
                        pending.remove(&path);
                        let key = normalize_path_key(&path);
                        let is_suppressed = if let Ok(mut sup) = suppressed.lock() {
                            sup.retain(|_, until| *until > now);
                            sup.get(&key).is_some_and(|until| now < *until)
                        } else {
                            false
                        };
                        if is_suppressed {
                            continue;
                        }

                        if kind == "removed" && !path.exists() {
                            let file_name = path
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            on_change(LocalPresetChangePayload {
                                kind: "removed".to_string(),
                                path: path.to_string_lossy().into_owned(),
                                file_name,
                                entry: None,
                            });
                            continue;
                        }

                        if !path.is_file() {
                            continue;
                        }

                        if let Ok(_valid_raw) = read_shared_json_text_with_retry(
                            &path,
                            READ_RETRY_ATTEMPTS,
                            READ_RETRY_DELAY,
                        ) && let Some(entry) = read_local_preset_entry(&path)
                        {
                            on_change(LocalPresetChangePayload {
                                kind: "updated".to_string(),
                                path: entry.path.clone(),
                                file_name: entry.file_name.clone(),
                                entry: Some(entry),
                            });
                        }
                    }

                    thread::sleep(Duration::from_millis(30));
                }
            })
            .map_err(|e| format!("Failed to start watcher thread: {e}"))?;

        if let Ok(mut guard) = self.worker.lock() {
            *guard = Some(WatcherWorker {
                stop_flag,
                join: Some(join),
                _watcher: watcher,
            });
        }

        Ok(())
    }

    pub fn stop(&self) {
        if let Ok(mut guard) = self.worker.lock()
            && let Some(prev) = guard.take()
        {
            prev.stop();
        }
    }
}

impl Drop for PresetWatcherManager {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_debounces_rapid_writes_and_respects_suppression() {
        let dir =
            std::env::temp_dir().join(format!("foundation_studio_watch_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let (tx, rx) = mpsc::channel();
        let manager = PresetWatcherManager::new();
        manager
            .update_dirs_with_callback(&[dir.to_string_lossy().into_owned()], move |evt| {
                let _ = tx.send(evt);
            })
            .unwrap();

        let live_path = dir.join("live.json");
        let suppressed_path = dir.join("suppressed.json");

        // Rapid writes + tmp file -> debounced single valid event with final content.
        fs::write(
            &live_path,
            r#"{"houseSize":"Small","interiorFurniture":[]}"#,
        )
        .unwrap();
        thread::sleep(Duration::from_millis(30));
        fs::write(&live_path, "{partial_invalid").unwrap();
        thread::sleep(Duration::from_millis(30));
        let tmp = dir.join("live.json.tmp");
        fs::write(
            &tmp,
            r#"{"houseSize":"Large","interiorFurniture":[{"name":"Lamp"}]}"#,
        )
        .unwrap();
        let _ = fs::remove_file(&live_path);
        fs::rename(&tmp, &live_path).unwrap();

        // Suppressed self-write should not trigger a change event.
        manager.suppress_path(&suppressed_path);
        write_text_atomic(
            &suppressed_path,
            r#"{"houseSize":"Medium","interiorFurniture":[]}"#,
        )
        .unwrap();

        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(evt) = rx.recv_timeout(Duration::from_millis(120)) {
                received.push(evt);
            } else if !received.is_empty() {
                break;
            }
        }

        manager.stop();

        assert_eq!(received.len(), 1, "unexpected watcher events: {received:?}");
        assert_eq!(received[0].file_name, "live.json");
        let entry = received[0].entry.as_ref().unwrap();
        assert_eq!(entry.meta.as_ref().unwrap().size_key, "large");
        assert_eq!(entry.meta.as_ref().unwrap().indoor_count, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_path_key_matches_before_and_after_file_creation() {
        let dir =
            std::env::temp_dir().join(format!("foundation_studio_normkey_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let target = dir.join("NewFile.JSON");
        let before = normalize_path_key(&target);
        fs::write(&target, b"{}").unwrap();
        let after = normalize_path_key(&target);
        assert_eq!(before, after);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reveal_in_explorer_rejects_empty_and_missing_paths() {
        assert!(reveal_in_explorer(Path::new("")).is_err());
        let missing = std::env::temp_dir().join("foundation_studio_definitely_missing_12345");
        let _ = fs::remove_file(&missing);
        assert!(reveal_in_explorer(&missing).is_err());
    }
}
