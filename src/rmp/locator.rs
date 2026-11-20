use std::path::{Path, PathBuf};

pub const MAKEPLACE_EXE_NAME: &str = "MakePlace-Win64-Shipping.exe";
pub const RELAY_EXE_NAME: &str = "rmp-bridge-relay.exe";
const DEFAULT_WORKSPACE_BRIDGE: &str = r"E:\FF14\Repos\PrivateTest\rmp-bridge";

/// Resolves a configured path (executable, inner `MakePlace` folder, or root folder) into the
/// root ReMakePlace directory containing `MakePlace/Binaries/Win64/MakePlace-Win64-Shipping.exe`.
pub fn try_resolve_remakeplace_source_dir(configured_path: &str) -> Option<PathBuf> {
    let trimmed = configured_path.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = Path::new(trimmed);
    if !path.is_absolute() && !path.exists() {
        return None;
    }

    let candidate_root = if path.is_file() {
        let is_makeplace_exe = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(MAKEPLACE_EXE_NAME));
        if is_makeplace_exe {
            // Support ...\MakePlace\Binaries\Win64\MakePlace-Win64-Shipping.exe
            let win64 = path.parent();
            let binaries = win64.and_then(Path::parent);
            let makeplace = binaries.and_then(Path::parent);
            let root = makeplace.and_then(Path::parent);
            root.or(win64).unwrap_or(path).to_path_buf()
        } else {
            path.parent().unwrap_or(path).to_path_buf()
        }
    } else {
        path.to_path_buf()
    };

    let candidate_root = normalize_clean_path(&candidate_root);
    if has_makeplace_exe(&candidate_root) {
        return Some(candidate_root);
    }

    // Also allow selecting the existing inner `MakePlace` directory directly.
    if candidate_root.is_dir()
        && let Some(parent) = candidate_root.parent()
        && has_makeplace_exe(parent)
    {
        return Some(parent.to_path_buf());
    }

    None
}

/// Locates the `rmp-bridge` directory containing `scripts/prepare-shadow.ps1`.
pub fn find_bridge_root_dir(start_dir: Option<&Path>) -> Option<PathBuf> {
    let mut probe_roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = start_dir {
        probe_roots.push(dir.to_path_buf());
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        probe_roots.push(parent.to_path_buf());
    }
    if let Ok(cwd) = std::env::current_dir() {
        probe_roots.push(cwd);
    }

    for start in probe_roots {
        let mut cur: Option<&Path> = Some(start.as_path());
        for _ in 0..10 {
            let Some(dir) = cur else {
                break;
            };
            if is_valid_bridge_root(dir) {
                return Some(dir.to_path_buf());
            }
            let sibling = dir.join("rmp-bridge");
            if is_valid_bridge_root(&sibling) {
                return Some(sibling);
            }
            cur = dir.parent();
        }
    }

    let managed = crate::settings::managed_rmp_bridge_dir();
    if is_valid_bridge_root(&managed) {
        return Some(managed);
    }

    let fallback = Path::new(DEFAULT_WORKSPACE_BRIDGE);
    if is_valid_bridge_root(fallback) {
        Some(fallback.to_path_buf())
    } else {
        None
    }
}

pub fn get_prepare_script_path(bridge_root: &Path) -> PathBuf {
    bridge_root.join("scripts").join("prepare-shadow.ps1")
}

pub fn get_shadow_executable_path(bridge_root: &Path) -> PathBuf {
    bridge_root
        .join("runtime")
        .join("MakePlace")
        .join("Binaries")
        .join("Win64")
        .join(MAKEPLACE_EXE_NAME)
}

pub fn get_bridge_ipc_directory(bridge_root: &Path) -> PathBuf {
    bridge_root.join("runtime").join("bridge_ipc")
}

fn has_makeplace_exe(root: &Path) -> bool {
    root.join("MakePlace")
        .join("Binaries")
        .join("Win64")
        .join(MAKEPLACE_EXE_NAME)
        .is_file()
}

fn is_valid_bridge_root(path: &Path) -> bool {
    path.is_dir() && get_prepare_script_path(path).is_file()
}

fn normalize_clean_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let trimmed = s.trim_end_matches(['\\', '/']);
    if trimmed.ends_with(':') {
        PathBuf::from(format!("{trimmed}\\"))
    } else {
        PathBuf::from(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolves_remakeplace_from_root_inner_dir_and_exe() {
        let root =
            std::env::temp_dir().join(format!("foundation_studio_rmp_loc_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let win64 = root.join("MakePlace").join("Binaries").join("Win64");
        fs::create_dir_all(&win64).unwrap();
        let exe = win64.join(MAKEPLACE_EXE_NAME);
        fs::write(&exe, b"stub").unwrap();

        assert_eq!(
            try_resolve_remakeplace_source_dir(&root.to_string_lossy()),
            Some(root.clone())
        );
        assert_eq!(
            try_resolve_remakeplace_source_dir(&root.join("MakePlace").to_string_lossy()),
            Some(root.clone())
        );
        assert_eq!(
            try_resolve_remakeplace_source_dir(&format!("\"{}\"", exe.display())),
            Some(root.clone())
        );
        assert_eq!(
            try_resolve_remakeplace_source_dir(&root.join("missing").to_string_lossy()),
            None
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_bridge_root_from_nested_dir() {
        let root = std::env::temp_dir().join(format!(
            "foundation_studio_bridge_root_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let bridge = root.join("rmp-bridge");
        fs::create_dir_all(bridge.join("scripts")).unwrap();
        fs::write(bridge.join("scripts").join("prepare-shadow.ps1"), "# stub").unwrap();
        let nested = root.join("Foundation-Studio").join("target").join("debug");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_bridge_root_dir(Some(&nested)), Some(bridge));

        let _ = fs::remove_dir_all(&root);
    }
}
