use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

const REQUIRED_PORTS: std::ops::RangeInclusive<u16> = 21057..=21064;
const MAX_SERVER_LOGS: usize = 500;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreflightIssue {
    pub code: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundationServerStatus {
    pub mode: String,
    pub foundation_root: String,
    pub profile: String,
    pub pid: Option<u32>,
    pub protocol_patch: Option<String>,
    pub estates_count: Option<usize>,
    pub online_sessions: Option<usize>,
    pub preflight_issues: Vec<PreflightIssue>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLogEntry {
    pub ts_ms: u64,
    pub stream: String,
    pub line: String,
}

#[cfg(windows)]
struct JobObjectHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for JobObjectHandle {}
#[cfg(windows)]
unsafe impl Sync for JobObjectHandle {}

#[cfg(windows)]
impl Drop for JobObjectHandle {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                let _ = windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 0);
                let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
                self.0 = std::ptr::null_mut();
            }
        }
    }
}

struct ManagedServerState {
    run_id: u64,
    pid: u32,
    profile: String,
    foundation_root: String,
    #[cfg(windows)]
    _job: JobObjectHandle,
}

struct ServerInner {
    next_run_id: u64,
    managed: Option<ManagedServerState>,
    logs: VecDeque<ServerLogEntry>,
    last_error: Option<String>,
}

pub struct FoundationServerController {
    inner: Arc<Mutex<ServerInner>>,
    probe_http: Client,
}

impl Default for FoundationServerController {
    fn default() -> Self {
        Self::new()
    }
}

impl FoundationServerController {
    pub fn new() -> Self {
        let probe_http = Client::builder()
            .connect_timeout(Duration::from_millis(450))
            .timeout(Duration::from_millis(800))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            inner: Arc::new(Mutex::new(ServerInner {
                next_run_id: 1,
                managed: None,
                logs: VecDeque::with_capacity(MAX_SERVER_LOGS),
                last_error: None,
            })),
            probe_http,
        }
    }

    pub fn is_managed_running(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.managed.is_some())
            .unwrap_or(false)
    }

    pub fn get_logs(&self) -> Vec<ServerLogEntry> {
        self.inner
            .lock()
            .map(|g| g.logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear_logs(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.logs.clear();
        }
    }

    pub async fn get_status(
        &self,
        foundation_root: &str,
        profile: &str,
        sim_base_url: &str,
    ) -> FoundationServerStatus {
        let resolved_root = resolve_foundation_root(foundation_root);
        let root_str = resolved_root
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| foundation_root.trim().to_string());

        let (managed_info, last_error) = self
            .inner
            .lock()
            .map(|g| {
                (
                    g.managed
                        .as_ref()
                        .map(|m| (m.pid, m.profile.clone(), m.foundation_root.clone())),
                    g.last_error.clone(),
                )
            })
            .unwrap_or((None, None));

        let probe_url = format!(
            "{}/api/v1/housing/status",
            sim_base_url.trim_end_matches('/')
        );
        let probed = self.probe_housing_status(&probe_url).await;

        let (mode, pid, active_profile, active_root) = match (managed_info, probed.as_ref()) {
            (Some((mpid, mprof, mroot)), _) => ("managed", Some(mpid), mprof, mroot),
            (None, Some(_)) => ("external", None, profile.to_string(), root_str.clone()),
            (None, None) => ("stopped", None, profile.to_string(), root_str.clone()),
        };

        let preflight_issues = if mode == "stopped" {
            match resolved_root.as_deref() {
                Some(p) => check_preflight_issues(p, &active_profile),
                None => vec![PreflightIssue {
                    code: "no_foundation_root".to_string(),
                    detail: if root_str.trim().is_empty() {
                        None
                    } else {
                        Some(root_str.clone())
                    },
                }],
            }
        } else {
            Vec::new()
        };

        FoundationServerStatus {
            mode: mode.to_string(),
            foundation_root: active_root,
            profile: active_profile,
            pid,
            protocol_patch: probed.as_ref().map(|p| p.protocol_patch.clone()),
            estates_count: probed.as_ref().map(|p| p.estates_count),
            online_sessions: probed.as_ref().map(|p| p.online_sessions),
            preflight_issues,
            last_error,
        }
    }

    pub async fn start_server(
        &self,
        app: AppHandle,
        foundation_root: &str,
        profile: &str,
        sim_base_url: &str,
    ) -> Result<FoundationServerStatus, String> {
        let profile_norm = normalize_profile(profile);
        let current = self
            .get_status(foundation_root, &profile_norm, sim_base_url)
            .await;
        if current.mode == "managed" {
            return Err("A Foundation server started by this app is already running.".to_string());
        }
        if current.mode == "external" {
            return Err("An external Foundation server is already running.".to_string());
        }

        let root_path = resolve_foundation_root(foundation_root).ok_or_else(|| {
            "Could not find a Foundation root folder containing config.yaml.".to_string()
        })?;

        ensure_oodle_dll_in_profile(&root_path, &profile_norm)?;
        let issues = check_preflight_issues(&root_path, &profile_norm);
        if !issues.is_empty() {
            let msg = issues
                .iter()
                .map(|i| match &i.detail {
                    Some(d) => format!("{} ({d})", i.code),
                    None => i.code.clone(),
                })
                .collect::<Vec<_>>()
                .join(" / ");
            self.set_last_error(Some(msg.clone()));
            return Err(msg);
        }

        let occupied = find_occupied_ports();
        if !occupied.is_empty() {
            let msg = format!("Ports already in use: {occupied:?}");
            self.set_last_error(Some(msg.clone()));
            return Err(msg);
        }

        let run_exe = root_path
            .join("target")
            .join(&profile_norm)
            .join("kawari-run.exe");

        let (child, pid, run_id) = self.spawn_with_job(&root_path, &profile_norm, &run_exe)?;
        self.set_last_error(None);
        self.push_log(
            &app,
            "system",
            format!(
                "Foundation server started (profile={}, PID={}, CWD={})",
                profile_norm,
                pid,
                root_path.display()
            ),
        );

        self.spawn_io_and_wait_threads(app.clone(), child, run_id, sim_base_url.to_string());
        Ok(self
            .get_status(foundation_root, &profile_norm, sim_base_url)
            .await)
    }

    pub fn stop_server(&self, app: Option<&AppHandle>) -> Result<(), String> {
        let stopped_pid = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "Failed to lock server state".to_string())?;
            let Some(managed) = guard.managed.take() else {
                return Ok(());
            };
            let pid = managed.pid;
            // Dropping `_job` calls TerminateJobObject + CloseHandle, reaping all kawari-* children.
            drop(managed);
            pid
        };

        if let Some(handle) = app {
            self.push_log(
                handle,
                "system",
                format!("Foundation server stopped (PID {stopped_pid} Job Object cleaned up)"),
            );
            let _ = handle.emit("foundation-server-changed", ());
        }
        Ok(())
    }

    fn spawn_with_job(
        &self,
        root_path: &Path,
        profile_norm: &str,
        run_exe: &Path,
    ) -> Result<(Child, u32, u64), String> {
        let mut cmd = Command::new(run_exe);
        cmd.current_dir(root_path)
            .env("RUST_BACKTRACE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        #[cfg(windows)]
        let job = create_kill_on_close_job()?;

        let child = cmd.spawn().map_err(|e| {
            format!(
                "Failed to launch kawari-run.exe ({}): {e}",
                run_exe.display()
            )
        })?;
        let pid = child.id();

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let assigned = unsafe {
                windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                    job.0,
                    child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
                )
            };
            if assigned == 0 {
                let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
                // Drop `job` first or kill child manually since assignment failed.
                let mut owned_child = child;
                let _ = owned_child.kill();
                drop(job);
                return Err(format!(
                    "Failed to assign Windows Job Object (GetLastError={err})"
                ));
            }
        }

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| "Failed to lock server state".to_string())?;
        let run_id = guard.next_run_id;
        guard.next_run_id += 1;
        guard.managed = Some(ManagedServerState {
            run_id,
            pid,
            profile: profile_norm.to_string(),
            foundation_root: root_path.to_string_lossy().into_owned(),
            #[cfg(windows)]
            _job: job,
        });
        Ok((child, pid, run_id))
    }

    fn spawn_io_and_wait_threads(
        &self,
        app: AppHandle,
        mut child: Child,
        run_id: u64,
        _sim_base_url: String,
    ) {
        if let Some(stdout) = child.stdout.take() {
            let inner = Arc::clone(&self.inner);
            let app_out = app.clone();
            thread::Builder::new()
                .name("foundation-stdout".to_string())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        push_log_to_inner(&inner, &app_out, "stdout", line);
                    }
                })
                .ok();
        }
        if let Some(stderr) = child.stderr.take() {
            let inner = Arc::clone(&self.inner);
            let app_err = app.clone();
            thread::Builder::new()
                .name("foundation-stderr".to_string())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        push_log_to_inner(&inner, &app_err, "stderr", line);
                    }
                })
                .ok();
        }

        let inner = Arc::clone(&self.inner);
        thread::Builder::new()
            .name("foundation-wait".to_string())
            .spawn(move || {
                let exit = child.wait();
                let was_active = if let Ok(mut guard) = inner.lock() {
                    if guard.managed.as_ref().is_some_and(|m| m.run_id == run_id) {
                        guard.managed.take();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if was_active {
                    push_log_to_inner(
                        &inner,
                        &app,
                        "system",
                        format!("Foundation server process exited ({exit:?})"),
                    );
                    let _ = app.emit("foundation-server-changed", ());
                }
            })
            .ok();
    }

    fn set_last_error(&self, err: Option<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.last_error = err;
        }
    }

    fn push_log(&self, app: &AppHandle, stream: &str, line: String) {
        push_log_to_inner(&self.inner, app, stream, line);
    }

    async fn probe_housing_status(&self, url: &str) -> Option<ProbedStatus> {
        let resp = self.probe_http.get(url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let val: serde_json::Value = resp.json().await.ok()?;
        let protocol_patch = val
            .get("protocol_patch")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let estates_count = val
            .get("estates_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let online_sessions = val
            .get("online_sessions")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        Some(ProbedStatus {
            protocol_patch,
            estates_count,
            online_sessions,
        })
    }
}

struct ProbedStatus {
    protocol_patch: String,
    estates_count: usize,
    online_sessions: usize,
}

fn push_log_to_inner(inner: &Arc<Mutex<ServerInner>>, app: &AppHandle, stream: &str, line: String) {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let entry = ServerLogEntry {
        ts_ms,
        stream: stream.to_string(),
        line,
    };
    if let Ok(mut guard) = inner.lock() {
        if guard.logs.len() >= MAX_SERVER_LOGS {
            guard.logs.pop_front();
        }
        guard.logs.push_back(entry.clone());
    }
    let _ = app.emit("foundation-server-log", entry);
}

#[cfg(windows)]
fn create_kill_on_close_job() -> Result<JobObjectHandle, String> {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    unsafe {
        let handle = CreateJobObjectW(ptr::null(), ptr::null());
        if handle.is_null() {
            return Err(format!("CreateJobObjectW failed (err={})", GetLastError()));
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            let err = GetLastError();
            let _ = CloseHandle(handle);
            return Err(format!("SetInformationJobObject failed (err={err})"));
        }

        Ok(JobObjectHandle(handle))
    }
}

pub fn normalize_profile(profile: &str) -> String {
    if profile.trim().eq_ignore_ascii_case("release") {
        "release".to_string()
    } else {
        "debug".to_string()
    }
}

pub fn resolve_foundation_root(configured: &str) -> Option<PathBuf> {
    let trimmed = configured.trim().trim_matches('"');
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.join("config.yaml").is_file() {
            return Some(p);
        }
    }
    crate::settings::detect_default_foundation_root()
}

pub fn check_preflight_issues(root: &Path, profile: &str) -> Vec<PreflightIssue> {
    let mut issues = Vec::new();
    let profile_dir = root.join("target").join(profile);

    if !root.join("config.yaml").is_file() {
        issues.push(PreflightIssue {
            code: "missing_config".to_string(),
            detail: Some(root.display().to_string()),
        });
    }
    for bin in ["kawari-run.exe", "kawari-world.exe", "kawari-lobby.exe"] {
        if !profile_dir.join(bin).is_file() {
            issues.push(PreflightIssue {
                code: "missing_binary".to_string(),
                detail: Some(format!("{profile}/{bin}")),
            });
        }
    }
    let world_exe = profile_dir.join("kawari-world.exe");
    if world_exe.is_file() && !world_binary_has_oodle(&world_exe) {
        issues.push(PreflightIssue {
            code: "missing_oodle_feature".to_string(),
            detail: None,
        });
    }
    let dll = profile_dir.join("oodle-network-shared.dll");
    let repo_dll = root.join("oodle").join("oodle-network-shared.dll");
    if !dll.is_file() && !repo_dll.is_file() {
        issues.push(PreflightIssue {
            code: "missing_oodle_dll".to_string(),
            detail: None,
        });
    }
    issues
}

fn ensure_oodle_dll_in_profile(root: &Path, profile: &str) -> Result<(), String> {
    let profile_dir = root.join("target").join(profile);
    let target_dll = profile_dir.join("oodle-network-shared.dll");
    if target_dll.is_file() {
        return Ok(());
    }
    let repo_dll = root.join("oodle").join("oodle-network-shared.dll");
    if repo_dll.is_file() {
        fs::create_dir_all(&profile_dir)
            .map_err(|e| format!("Failed to create target directory: {e}"))?;
        fs::copy(&repo_dll, &target_dll)
            .map_err(|e| format!("Failed to copy oodle-network-shared.dll: {e}"))?;
    }
    Ok(())
}

fn world_binary_has_oodle(exe_path: &Path) -> bool {
    let Ok(bytes) = fs::read(exe_path) else {
        return false;
    };
    contains_bytes(&bytes, b"oodle-network-shared")
        && !contains_bytes(&bytes, b"Something is trying to use Oodle")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn find_occupied_ports() -> Vec<u16> {
    let mut occupied = Vec::new();
    for port in REQUIRED_PORTS {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        if TcpStream::connect_timeout(&addr, Duration::from_millis(60)).is_ok() {
            occupied.push(port);
        }
    }
    occupied
}
