use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::settings::normalize_expansion_level;

pub const EXECUTABLE_FILE_NAME: &str = "ffxiv_dx11.exe";
pub const VERSION_FILE_NAME: &str = "ffxivgame.ver";
pub const LOBBY_HOST_SLOT_COUNT: usize = 12;
pub const DEFAULT_REGION: i32 = 3;

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub session_id: String,
    pub lobby_host: String,
    pub lobby_port: u16,
    pub frontier_host_argument: String,
    pub language: i32,
    pub expansion_level: i32,
    pub game_version: String,
    pub extra_arguments: String,
    pub include_frontier_host: bool,
}

pub fn resolve_game_executable(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("Game path is required.".to_string());
    }

    let p = Path::new(trimmed);
    let mut candidates = Vec::new();
    if p.is_file() {
        candidates.push(p.to_path_buf());
    } else if p.is_dir() {
        candidates.push(p.join(EXECUTABLE_FILE_NAME));
        candidates.push(p.join("game").join(EXECUTABLE_FILE_NAME));
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(fs::canonicalize(&candidate)
                .map(strip_unc_prefix)
                .unwrap_or(candidate));
        }
    }

    Err(format!(
        "Could not find {EXECUTABLE_FILE_NAME} from '{path}'."
    ))
}

pub fn read_game_version(game_path: &str) -> Result<String, String> {
    let trimmed = game_path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("Game path is required.".to_string());
    }

    let p = Path::new(trimmed);
    let mut candidates = Vec::new();
    let looks_like_exe = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if (p.is_file() || looks_like_exe)
        && let Some(dir) = p.parent()
    {
        candidates.push(dir.join(VERSION_FILE_NAME));
    }
    if p.is_dir() {
        candidates.push(p.join(VERSION_FILE_NAME));
        candidates.push(p.join("game").join(VERSION_FILE_NAME));
    }

    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        if let Ok(file) = File::open(&candidate) {
            let mut reader = BufReader::new(file);
            let mut first_line = String::new();
            if reader.read_line(&mut first_line).is_ok() {
                let ver = first_line.trim().to_string();
                if !ver.is_empty() {
                    return Ok(ver);
                }
            }
        }
    }

    Err(format!(
        "Could not find {VERSION_FILE_NAME} from '{game_path}'."
    ))
}

pub fn build_launch_arguments(options: &LaunchOptions) -> Result<String, String> {
    let session_id = require_value(&options.session_id, "SessionId")?;
    let lobby_host = require_value(&options.lobby_host, "LobbyHost")?;
    let game_version = require_value(&options.game_version, "GameVersion")?;
    let language = options.language.clamp(0, 3);
    let expansion = normalize_expansion_level(options.expansion_level);

    let mut args = vec![
        format!("DEV.TestSID={session_id}"),
        "DEV.UseSqPack=1".to_string(),
        "DEV.DataPathType=1".to_string(),
    ];

    for i in 1..=LOBBY_HOST_SLOT_COUNT {
        args.push(format!("DEV.LobbyHost{i:02}={lobby_host}"));
        args.push(format!("DEV.LobbyPort{i:02}={}", options.lobby_port));
    }

    args.push(format!("SYS.region={DEFAULT_REGION}"));
    args.push(format!("language={language}"));
    args.push(format!("version={game_version}"));
    args.push(format!("DEV.MaxEntitledExpansionID={expansion}"));

    if options.include_frontier_host {
        let frontier = require_value(&options.frontier_host_argument, "FrontierHostArgument")?;
        args.push(format!("DEV.GMServerHost={frontier}"));
    }

    if !options.extra_arguments.trim().is_empty() {
        args.push(options.extra_arguments.trim().to_string());
    }

    Ok(args.join(" "))
}

pub fn mask_session_id(session_id: &str) -> String {
    let chars: Vec<char> = session_id.chars().collect();
    if chars.len() <= 8 {
        return "***".to_string();
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

pub fn spawn_game_client(exe_path: &Path, raw_args: &str) -> Result<Child, String> {
    let working_dir = exe_path
        .parent()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| "Failed to determine game working directory".to_string())?;

    let mut cmd = Command::new(exe_path);
    cmd.current_dir(working_dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(raw_args);
    }
    #[cfg(not(windows))]
    {
        cmd.args(raw_args.split_whitespace());
    }
    cmd.spawn()
        .map_err(|e| format!("Failed launching {}: {e}", exe_path.display()))
}

#[cfg(windows)]
pub fn find_running_ffxiv_pids() -> Vec<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut pids = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return pids;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if name.eq_ignore_ascii_case(EXECUTABLE_FILE_NAME) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    pids
}

#[cfg(not(windows))]
pub fn find_running_ffxiv_pids() -> Vec<u32> {
    Vec::new()
}

fn require_value(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} is required."))
    } else {
        Ok(trimmed.to_string())
    }
}

fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_game_exe_and_version_from_install_root() {
        let root =
            std::env::temp_dir().join(format!("foundation_studio_game_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let game_dir = root.join("game");
        fs::create_dir_all(&game_dir).unwrap();
        let exe = game_dir.join(EXECUTABLE_FILE_NAME);
        fs::write(&exe, b"stub").unwrap();
        fs::write(game_dir.join(VERSION_FILE_NAME), "2026.11.03.0000.0000\r\n").unwrap();

        let resolved = resolve_game_executable(&root.to_string_lossy()).unwrap();
        assert!(resolved.ends_with(Path::new("game").join(EXECUTABLE_FILE_NAME)));
        assert_eq!(
            read_game_version(&root.to_string_lossy()).unwrap(),
            "2026.11.03.0000.0000"
        );
        assert_eq!(
            read_game_version(&exe.to_string_lossy()).unwrap(),
            "2026.11.03.0000.0000"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn builds_direct_client_args_for_all_12_lobby_slots_and_masks_sid() {
        let args = build_launch_arguments(&LaunchOptions {
            session_id: "SID1234567890".to_string(),
            lobby_host: "127.0.0.1".to_string(),
            lobby_port: 21059,
            frontier_host_argument: "127.0.0.1:21058".to_string(),
            language: 0,
            expansion_level: 99,
            game_version: "2026.11.03.0000.0000".to_string(),
            extra_arguments: "-customFlag=1".to_string(),
            include_frontier_host: true,
        })
        .unwrap();

        assert!(args.starts_with("DEV.TestSID=SID1234567890 DEV.UseSqPack=1 DEV.DataPathType=1 "));
        assert!(args.contains("DEV.LobbyHost01=127.0.0.1 DEV.LobbyPort01=21059"));
        assert!(args.contains("DEV.LobbyHost12=127.0.0.1 DEV.LobbyPort12=21059"));
        assert!(args.contains("SYS.region=3 language=0 version=2026.11.03.0000.0000"));
        assert!(args.contains(
            "DEV.MaxEntitledExpansionID=5 DEV.GMServerHost=127.0.0.1:21058 -customFlag=1"
        ));
        assert_eq!(mask_session_id("short"), "***");
        assert_eq!(mask_session_id("12345678"), "***");
        assert_eq!(mask_session_id("SID1234567890"), "SID1...7890");
    }
}
