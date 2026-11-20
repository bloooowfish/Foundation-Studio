use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use rmp_bridge_relay::client::HousingSseEventMsg;
use rmp_bridge_relay::ipc::IpcMailbox;
use rmp_bridge_relay::{RelayConfig, RelayHandle, RelayStatusSnapshot};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::housing::HousingClientState;
use crate::rmp::locator::{
    find_bridge_root_dir, get_bridge_ipc_directory, get_prepare_script_path,
    get_shadow_executable_path, try_resolve_remakeplace_source_dir,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RmpSyncStatus {
    pub phase: String,
    pub running: bool,
    pub shadow_pid: Option<u32>,
    pub relay: RelayStatusSnapshot,
    pub message: Option<String>,
}

impl Default for RmpSyncStatus {
    fn default() -> Self {
        Self {
            phase: "idle".to_string(),
            running: false,
            shadow_pid: None,
            relay: RelayStatusSnapshot::default(),
            message: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RmpLogEvent {
    pub ts_ms: u64,
    pub level: String,
    pub message: String,
}

struct ActiveRmpSession {
    session_seq: u64,
    shadow_pid: u32,
    relay_handle: Option<RelayHandle>,
    monitor_cancel: Arc<AtomicBool>,
}

struct RmpInner {
    next_seq: u64,
    active: Option<ActiveRmpSession>,
    latest_status: RmpSyncStatus,
}

pub struct RmpController {
    inner: Arc<Mutex<RmpInner>>,
}

impl Default for RmpController {
    fn default() -> Self {
        Self::new()
    }
}

impl RmpController {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RmpInner {
                next_seq: 1,
                active: None,
                latest_status: RmpSyncStatus::default(),
            })),
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.active.is_some() || g.latest_status.phase == "preparing")
            .unwrap_or(false)
    }

    pub fn current_status(&self) -> RmpSyncStatus {
        self.inner
            .lock()
            .map(|g| g.latest_status.clone())
            .unwrap_or_default()
    }

    pub async fn start_sync(
        &self,
        app: AppHandle,
        remakeplace_path: String,
        housing: Arc<HousingClientState>,
    ) -> Result<RmpSyncStatus, String> {
        let session_seq = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "Failed to lock RMP state".to_string())?;
            if guard.active.is_some() {
                return Err("ReMakePlace Live Sync is already running.".to_string());
            }
            if guard.latest_status.phase == "preparing" {
                return Err("Preparing ReMakePlace shadow runtime.".to_string());
            }
            let seq = guard.next_seq;
            guard.next_seq += 1;
            guard.latest_status = RmpSyncStatus {
                phase: "preparing".to_string(),
                running: false,
                shadow_pid: None,
                relay: RelayStatusSnapshot::default(),
                message: Some("Preparing shadow runtime...".to_string()),
            };
            seq
        };
        emit_status(&app, &self.inner);

        let source_dir =
            try_resolve_remakeplace_source_dir(&remakeplace_path).ok_or_else(|| {
                self.reset_idle_with_error(
                    &app,
                    "Please configure a valid ReMakePlace folder or executable path.",
                );
                "Could not find a valid ReMakePlace folder or MakePlace-Win64-Shipping.exe path."
                    .to_string()
            })?;

        let bridge_root = find_bridge_root_dir(None).ok_or_else(|| {
            self.reset_idle_with_error(
                &app,
                "Could not locate rmp-bridge/scripts/prepare-shadow.ps1.",
            );
            "Could not locate rmp-bridge root directory.".to_string()
        })?;

        let prepare_script = get_prepare_script_path(&bridge_root);
        if !prepare_script.is_file() {
            let msg = format!("prepare-shadow.ps1 not found: {}", prepare_script.display());
            self.reset_idle_with_error(&app, &msg);
            return Err(msg);
        }

        emit_log(
            &app,
            "info",
            &format!(
                "Preparing shadow runtime (source untouched: {})",
                source_dir.display()
            ),
        );

        let prep_script_clone = prepare_script.clone();
        let source_clone = source_dir.clone();
        let prep_res = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut cmd = Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&prep_script_clone)
                .arg("-SourceDir")
                .arg(&source_clone)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let output = cmd
                .output()
                .map_err(|e| format!("Failed to execute prepare-shadow.ps1: {e}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                return Err(format!(
                    "prepare-shadow.ps1 failed (code {:?}): {}{}",
                    output.status.code(),
                    stderr.trim(),
                    stdout.trim()
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
        .await
        .map_err(|e| format!("Failed to join shadow prepare task: {e}"))?;

        if let Err(err) = prep_res {
            self.reset_idle_with_error(&app, &err);
            return Err(err);
        }

        let shadow_exe = get_shadow_executable_path(&bridge_root);
        if !shadow_exe.is_file() {
            let msg = format!(
                "Shadow ReMakePlace executable not found: {}",
                shadow_exe.display()
            );
            self.reset_idle_with_error(&app, &msg);
            return Err(msg);
        }

        let ipc_dir = get_bridge_ipc_directory(&bridge_root);
        if let Err(err) = fs::create_dir_all(&ipc_dir) {
            let msg = format!("Failed to create IPC directory: {err}");
            self.reset_idle_with_error(&app, &msg);
            return Err(msg);
        }
        if let Ok(mailbox) = IpcMailbox::new(&ipc_dir) {
            mailbox.clear_stale_outbound_commands();
            mailbox.clear_stale_rmp_state();
        }

        let shadow_child = spawn_shadow_rmp(&shadow_exe, &bridge_root, &ipc_dir)
            .inspect_err(|e| self.reset_idle_with_error(&app, e))?;
        let shadow_pid = shadow_child.id();

        let (sse_tx, sse_rx) = std::sync::mpsc::channel::<HousingSseEventMsg>();
        housing.set_rmp_sse_sender(Some(sse_tx));

        let sim_url = housing.simulator_base_url();
        let inner_for_cb = Arc::clone(&self.inner);
        let app_for_cb = app.clone();

        let relay_handle = match RelayHandle::start(
            RelayConfig {
                api_url: sim_url.clone(),
                ipc_dir: ipc_dir.clone(),
                land_ident: None,
                parent_pid: Some(shadow_pid),
                clear_rmp_state_files: false,
            },
            Some(sse_rx),
            move |snapshot| {
                let mut should_emit = false;
                if let Ok(mut guard) = inner_for_cb.lock()
                    && guard
                        .active
                        .as_ref()
                        .is_some_and(|a| a.session_seq == session_seq)
                {
                    guard.latest_status.phase = if snapshot.running {
                        "running".to_string()
                    } else {
                        "idle".to_string()
                    };
                    guard.latest_status.running = snapshot.running;
                    guard.latest_status.relay = snapshot;
                    guard.latest_status.message = None;
                    should_emit = true;
                }
                if should_emit {
                    emit_status(&app_for_cb, &inner_for_cb);
                }
            },
        ) {
            Ok(h) => h,
            Err(err) => {
                housing.set_rmp_sse_sender(None);
                kill_process_tree(shadow_pid);
                let msg = format!("Failed to start in-process relay: {err}");
                self.reset_idle_with_error(&app, &msg);
                return Err(msg);
            }
        };

        let monitor_cancel = Arc::new(AtomicBool::new(false));
        {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "Failed to lock RMP state".to_string())?;
            guard.active = Some(ActiveRmpSession {
                session_seq,
                shadow_pid,
                relay_handle: Some(relay_handle),
                monitor_cancel: Arc::clone(&monitor_cancel),
            });
            guard.latest_status = RmpSyncStatus {
                phase: "running".to_string(),
                running: true,
                shadow_pid: Some(shadow_pid),
                relay: RelayStatusSnapshot {
                    running: true,
                    ..Default::default()
                },
                message: Some(format!("Running (Shadow PID {shadow_pid})")),
            };
        }

        emit_status(&app, &self.inner);
        emit_log(
            &app,
            "info",
            &format!("ReMakePlace Live Sync started (Shadow PID {shadow_pid}, API {sim_url})"),
        );

        let inner_for_wait = Arc::clone(&self.inner);
        let app_for_wait = app.clone();
        let housing_for_wait = Arc::clone(&housing);
        thread::Builder::new()
            .name("rmp-shadow-monitor".to_string())
            .spawn(move || {
                let mut child = shadow_child;
                let status = child.wait();
                if monitor_cancel.load(Ordering::Relaxed) {
                    return;
                }
                let taken_relay = {
                    let Ok(mut guard) = inner_for_wait.lock() else {
                        return;
                    };
                    if guard
                        .active
                        .as_ref()
                        .is_some_and(|a| a.session_seq == session_seq)
                    {
                        let active = guard.active.take();
                        guard.latest_status = RmpSyncStatus {
                            phase: "idle".to_string(),
                            running: false,
                            shadow_pid: None,
                            relay: RelayStatusSnapshot::default(),
                            message: Some("ReMakePlace exited; sync stopped.".to_string()),
                        };
                        active.and_then(|mut a| a.relay_handle.take())
                    } else {
                        None
                    }
                };
                housing_for_wait.set_rmp_sse_sender(None);
                if let Some(relay) = taken_relay {
                    relay.stop();
                }
                emit_status(&app_for_wait, &inner_for_wait);
                emit_log(
                    &app_for_wait,
                    "info",
                    &format!("Shadow ReMakePlace exited ({status:?})"),
                );
            })
            .ok();

        Ok(self.current_status())
    }

    pub fn stop_sync(
        &self,
        app: Option<&AppHandle>,
        housing: &HousingClientState,
        kill_shadow: bool,
    ) -> RmpSyncStatus {
        let (taken_relay, shadow_pid) = {
            let Ok(mut guard) = self.inner.lock() else {
                return RmpSyncStatus::default();
            };
            let Some(mut active) = guard.active.take() else {
                return guard.latest_status.clone();
            };
            active.monitor_cancel.store(true, Ordering::Relaxed);
            guard.latest_status = RmpSyncStatus {
                phase: "idle".to_string(),
                running: false,
                shadow_pid: None,
                relay: RelayStatusSnapshot::default(),
                message: Some("Stopped".to_string()),
            };
            (active.relay_handle.take(), active.shadow_pid)
        };

        housing.set_rmp_sse_sender(None);
        if let Some(relay) = taken_relay {
            relay.stop();
        }
        if kill_shadow {
            kill_process_tree(shadow_pid);
        }

        if let Some(handle) = app {
            emit_status(handle, &self.inner);
            emit_log(
                handle,
                "info",
                &format!("ReMakePlace Live Sync stopped (PID {shadow_pid})"),
            );
        }

        self.current_status()
    }

    fn reset_idle_with_error(&self, app: &AppHandle, msg: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.latest_status = RmpSyncStatus {
                phase: "error".to_string(),
                running: false,
                shadow_pid: None,
                relay: RelayStatusSnapshot::default(),
                message: Some(msg.to_string()),
            };
        }
        emit_status(app, &self.inner);
        emit_log(app, "error", msg);
    }
}

fn spawn_shadow_rmp(
    shadow_exe: &Path,
    bridge_root: &Path,
    ipc_dir: &Path,
) -> Result<Child, String> {
    let working_dir = shadow_exe.parent().unwrap_or(bridge_root);
    Command::new(shadow_exe)
        .current_dir(working_dir)
        .env("RMP_BRIDGE_IPC_DIR", ipc_dir)
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to launch Shadow ReMakePlace ({}): {e}",
                shadow_exe.display()
            )
        })
}

fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

fn emit_status(app: &AppHandle, inner: &Arc<Mutex<RmpInner>>) {
    if let Ok(guard) = inner.lock() {
        let _ = app.emit("rmp-sync-status", guard.latest_status.clone());
    }
}

fn emit_log(app: &AppHandle, level: &str, message: &str) {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let _ = app.emit(
        "rmp-sync-log",
        RmpLogEvent {
            ts_ms,
            level: level.to_string(),
            message: message.to_string(),
        },
    );
}
