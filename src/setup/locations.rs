//! Where each runtime component lives (Studio-managed folder, a custom folder, or a developer
//! checkout) plus the rmp-bridge version marker.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rmp::locator::find_bridge_root_dir;
use crate::server::resolve_foundation_root;
use crate::settings::{managed_foundation_dir, managed_runtime_root};

pub const SOURCE_MANAGED: &str = "managed";
pub const SOURCE_CUSTOM: &str = "custom";
pub const SOURCE_DEV: &str = "dev";
pub const SOURCE_MISSING: &str = "missing";

/// Error prefix returned when an install/update would overwrite a developer checkout.
pub const DEV_CHECKOUT_PREFIX: &str = "dev_checkout:";
/// Error prefix returned when updating a component that is not installed anywhere.
pub const NOT_INSTALLED_PREFIX: &str = "not_installed:";

pub const BRIDGE_MARKER_FILE: &str = ".foundation-studio-bridge.json";
pub const BRIDGE_KIND_RELEASE: &str = "release";
pub const BRIDGE_KIND_COMMIT: &str = "commit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckoutKind {
    /// Foundation server checkout: `.git` or `Cargo.toml`.
    Foundation,
    /// rmp-bridge checkout: `.git`.
    Bridge,
    /// Never treated as a developer checkout (e.g. ReMakePlace).
    Other,
}

/// True when `dir` looks like a developer source checkout that installers must not overwrite.
pub fn is_dev_checkout(dir: &Path, kind: CheckoutKind) -> bool {
    match kind {
        CheckoutKind::Foundation => dir.join(".git").exists() || dir.join("Cargo.toml").is_file(),
        CheckoutKind::Bridge => dir.join(".git").exists(),
        CheckoutKind::Other => false,
    }
}

pub fn dev_checkout_error(dir: &Path) -> String {
    format!("{DEV_CHECKOUT_PREFIX} {}", dir.display())
}

fn normalize_for_compare(p: &Path) -> PathBuf {
    let resolved = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = resolved.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    PathBuf::from(s.replace('/', "\\").to_ascii_lowercase())
}

pub fn path_is_within(child: &Path, root: &Path) -> bool {
    normalize_for_compare(child).starts_with(normalize_for_compare(root))
}

/// Classifies a component location. `present` says whether the component's files were found.
pub fn classify_source_with_root(
    dir: Option<&Path>,
    kind: CheckoutKind,
    present: bool,
    managed_root: &Path,
) -> &'static str {
    let Some(dir) = dir.filter(|d| d.is_dir()) else {
        return SOURCE_MISSING;
    };
    if path_is_within(dir, managed_root) {
        return if present {
            SOURCE_MANAGED
        } else {
            SOURCE_MISSING
        };
    }
    if is_dev_checkout(dir, kind) {
        return SOURCE_DEV;
    }
    if present {
        SOURCE_CUSTOM
    } else {
        SOURCE_MISSING
    }
}

pub fn classify_source(dir: Option<&Path>, kind: CheckoutKind, present: bool) -> &'static str {
    classify_source_with_root(dir, kind, present, &managed_runtime_root())
}

/// Foundation root currently in use (resolved setting, raw setting, or the managed folder).
pub fn active_foundation_root(configured: &str) -> PathBuf {
    resolve_foundation_root(configured).unwrap_or_else(|| {
        let trimmed = configured.trim().trim_matches('"');
        if trimmed.is_empty() {
            managed_foundation_dir()
        } else {
            PathBuf::from(trimmed)
        }
    })
}

pub fn active_bridge_root() -> Option<PathBuf> {
    find_bridge_root_dir(None)
}

pub fn bridge_files_present(dir: &Path) -> bool {
    dir.join("scripts").join("prepare-shadow.ps1").is_file()
        && dir
            .join("mods")
            .join("FoundationBridge")
            .join("Scripts")
            .join("main.lua")
            .is_file()
}

// ---------------------------------------------------------------------------------------------
// rmp-bridge marker
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMarker {
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// `release` or `commit`.
    pub kind: String,
    /// Unix epoch seconds.
    #[serde(default)]
    pub installed_at: u64,
}

pub fn read_bridge_marker(dir: &Path) -> Option<BridgeMarker> {
    let raw = fs::read_to_string(dir.join(BRIDGE_MARKER_FILE)).ok()?;
    serde_json::from_str::<BridgeMarker>(&raw)
        .ok()
        .filter(|m| !m.git_ref.trim().is_empty())
}

pub fn write_bridge_marker(dir: &Path, marker: &BridgeMarker) -> Result<(), String> {
    let json = serde_json::to_string_pretty(marker)
        .map_err(|e| format!("Failed serializing rmp-bridge marker: {e}"))?;
    fs::write(dir.join(BRIDGE_MARKER_FILE), json)
        .map_err(|e| format!("Failed writing rmp-bridge version marker: {e}"))
}

pub fn short_sha(sha: &str) -> String {
    sha.trim().chars().take(7).collect()
}

fn is_hex_sha(s: &str) -> bool {
    s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Short HEAD sha of a git checkout, read from `.git` files without spawning git.
pub fn read_git_head_short(repo: &Path) -> Option<String> {
    let dot_git = repo.join(".git");
    let git_dir = if dot_git.is_file() {
        let content = fs::read_to_string(&dot_git).ok()?;
        let target = content.trim().strip_prefix("gitdir:")?.trim();
        let p = PathBuf::from(target);
        if p.is_absolute() { p } else { repo.join(p) }
    } else {
        dot_git
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(ref_name) = head.strip_prefix("ref:").map(str::trim) else {
        return is_hex_sha(head).then(|| short_sha(head));
    };

    let mut candidates = vec![git_dir.clone()];
    // Linked worktrees keep shared refs in the common dir.
    if let Ok(common) = fs::read_to_string(git_dir.join("commondir")) {
        let c = PathBuf::from(common.trim());
        candidates.push(if c.is_absolute() { c } else { git_dir.join(c) });
    }
    for dir in &candidates {
        if let Ok(sha) = fs::read_to_string(dir.join(ref_name)) {
            let sha = sha.trim();
            if is_hex_sha(sha) {
                return Some(short_sha(sha));
            }
        }
        if let Ok(packed) = fs::read_to_string(dir.join("packed-refs")) {
            for line in packed.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(sha), Some(name)) = (parts.next(), parts.next())
                    && name == ref_name
                    && is_hex_sha(sha)
                {
                    return Some(short_sha(sha));
                }
            }
        }
    }
    None
}

/// Installed rmp-bridge version: marker ref, or the checkout HEAD for dev checkouts.
pub fn read_bridge_version(dir: &Path) -> Option<String> {
    if let Some(m) = read_bridge_marker(dir) {
        return Some(m.git_ref);
    }
    if dir.join(".git").exists() {
        return read_git_head_short(dir);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "foundation_studio_loc_{tag}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn detects_dev_checkouts() {
        let base = temp_base("dev");
        let plain = base.join("plain");
        let git = base.join("git");
        let cargo = base.join("cargo");
        fs::create_dir_all(&plain).unwrap();
        fs::create_dir_all(git.join(".git")).unwrap();
        fs::create_dir_all(&cargo).unwrap();
        fs::write(cargo.join("Cargo.toml"), "[package]").unwrap();

        assert!(!is_dev_checkout(&plain, CheckoutKind::Foundation));
        assert!(is_dev_checkout(&git, CheckoutKind::Foundation));
        assert!(is_dev_checkout(&cargo, CheckoutKind::Foundation));
        assert!(is_dev_checkout(&git, CheckoutKind::Bridge));
        assert!(!is_dev_checkout(&cargo, CheckoutKind::Bridge));
        assert!(!is_dev_checkout(&git, CheckoutKind::Other));
        assert!(dev_checkout_error(&git).starts_with(DEV_CHECKOUT_PREFIX));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn classifies_sources() {
        let base = temp_base("classify");
        let managed_root = base.join("managed");
        let managed = managed_root.join("foundation");
        let dev = base.join("dev");
        let custom = base.join("custom");
        fs::create_dir_all(&managed).unwrap();
        fs::create_dir_all(dev.join(".git")).unwrap();
        fs::create_dir_all(&custom).unwrap();

        let f = CheckoutKind::Foundation;
        assert_eq!(
            classify_source_with_root(Some(&managed), f, true, &managed_root),
            SOURCE_MANAGED
        );
        assert_eq!(
            classify_source_with_root(Some(&managed), f, false, &managed_root),
            SOURCE_MISSING
        );
        assert_eq!(
            classify_source_with_root(Some(&dev), f, false, &managed_root),
            SOURCE_DEV
        );
        assert_eq!(
            classify_source_with_root(Some(&custom), f, true, &managed_root),
            SOURCE_CUSTOM
        );
        assert_eq!(
            classify_source_with_root(Some(&custom), f, false, &managed_root),
            SOURCE_MISSING
        );
        assert_eq!(
            classify_source_with_root(Some(&base.join("nope")), f, true, &managed_root),
            SOURCE_MISSING
        );
        assert_eq!(
            classify_source_with_root(None, f, true, &managed_root),
            SOURCE_MISSING
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn bridge_marker_roundtrip_and_git_head() {
        let base = temp_base("marker");
        assert!(read_bridge_marker(&base).is_none());
        let marker = BridgeMarker {
            git_ref: "abc1234".into(),
            kind: BRIDGE_KIND_COMMIT.into(),
            installed_at: 42,
        };
        write_bridge_marker(&base, &marker).unwrap();
        let raw = fs::read_to_string(base.join(BRIDGE_MARKER_FILE)).unwrap();
        assert!(raw.contains("\"ref\"") && raw.contains("\"installedAt\""));
        assert_eq!(read_bridge_marker(&base), Some(marker));
        assert_eq!(read_bridge_version(&base).as_deref(), Some("abc1234"));

        let repo = base.join("repo");
        fs::create_dir_all(repo.join(".git").join("refs").join("heads")).unwrap();
        fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            repo.join(".git").join("refs").join("heads").join("main"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(read_bridge_version(&repo).as_deref(), Some("0123456"));

        fs::remove_file(repo.join(".git").join("refs").join("heads").join("main")).unwrap();
        fs::write(
            repo.join(".git").join("packed-refs"),
            "# pack-refs\nfedcba9876543210fedcba9876543210fedcba98 refs/heads/main\n",
        )
        .unwrap();
        assert_eq!(read_git_head_short(&repo).as_deref(), Some("fedcba9"));

        fs::write(
            repo.join(".git").join("HEAD"),
            "1111111222222233333334444444555555566666\n",
        )
        .unwrap();
        assert_eq!(read_git_head_short(&repo).as_deref(), Some("1111111"));
        let _ = fs::remove_dir_all(&base);
    }
}
