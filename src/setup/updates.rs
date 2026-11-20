//! Runtime update check against the official GitHub endpoints, plus the per-component
//! "update in place" action.

use std::path::PathBuf;

use serde::Serialize;
use tauri::AppHandle;

use super::locations::{
    self, CheckoutKind, NOT_INSTALLED_PREFIX, SOURCE_CUSTOM, SOURCE_DEV, SOURCE_MANAGED,
    SOURCE_MISSING,
};
use super::{
    BusyGuard, FOUNDATION_LATEST_RELEASE_API, OODLE_LATEST_RELEASE_API,
    RMP_BRIDGE_LATEST_RELEASE_API, RMP_BRIDGE_MAIN_COMMIT_API, SetupManager, SetupStatus,
    UE4SS_VERSION, read_oodle_version, read_version_marker,
};
use crate::rmp::locator::try_resolve_remakeplace_source_dir;
use crate::server::resolve_binary_dir;
use crate::settings::StudioSettings;
use crate::setup::remakeplace::read_installed_version;

/// Components accepted by `update_component`.
pub const UPDATABLE_COMPONENTS: [&str; 4] = ["foundation", "oodle", "rmp_bridge", "remakeplace"];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateItem {
    pub installed: Option<String>,
    pub latest: Option<String>,
    pub update_available: bool,
    pub error: Option<String>,
    /// `managed` | `custom` | `dev` | `missing`.
    pub source: String,
    /// Update available and the active copy is Studio-writable (managed or custom).
    pub can_update: bool,
    /// Version is pinned by Studio (never offered as an update).
    pub pinned: bool,
}

impl Default for UpdateItem {
    fn default() -> Self {
        Self {
            installed: None,
            latest: None,
            update_available: false,
            error: None,
            source: SOURCE_MISSING.to_string(),
            can_update: false,
            pinned: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpdates {
    pub foundation: UpdateItem,
    pub remakeplace: UpdateItem,
    pub oodle: UpdateItem,
    pub rmp_bridge: UpdateItem,
    pub ue4ss: UpdateItem,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUpdateReport {
    pub component: String,
    pub previous: Option<String>,
    pub current: Option<String>,
    pub status: SetupStatus,
}

fn version_parts(v: &str) -> Option<Vec<u64>> {
    let trimmed = v.trim().trim_start_matches(['v', 'V']);
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .split(['.', '-', '_'])
        .map(|p| p.parse::<u64>().ok())
        .collect()
}

/// True when `latest` is newer than `installed` (numeric compare, falling back to inequality).
pub fn is_newer(latest: &str, installed: &str) -> bool {
    match (version_parts(latest), version_parts(installed)) {
        (Some(mut a), Some(mut b)) => {
            let len = a.len().max(b.len());
            a.resize(len, 0);
            b.resize(len, 0);
            a > b
        }
        _ => {
            latest.trim().trim_start_matches(['v', 'V'])
                != installed.trim().trim_start_matches(['v', 'V'])
        }
    }
}

/// Commit refs differ (short/long sha prefixes compare equal).
pub fn commit_differs(latest: &str, installed: &str) -> bool {
    let a = latest.trim().to_ascii_lowercase();
    let b = installed.trim().to_ascii_lowercase();
    if a.is_empty() || b.is_empty() {
        return a != b;
    }
    !(a.starts_with(&b) || b.starts_with(&a))
}

pub fn can_update_for(update_available: bool, source: &str) -> bool {
    update_available && (source == SOURCE_MANAGED || source == SOURCE_CUSTOM)
}

pub fn build_item(installed: Option<String>, latest: Result<Option<String>, String>) -> UpdateItem {
    build_item_with(installed, latest, SOURCE_MISSING, is_newer)
}

pub fn build_item_with(
    installed: Option<String>,
    latest: Result<Option<String>, String>,
    source: &str,
    newer: fn(&str, &str) -> bool,
) -> UpdateItem {
    let (latest, error) = match latest {
        Ok(v) => (v, None),
        Err(e) => (None, Some(e)),
    };
    let update_available = match (&installed, &latest) {
        (Some(i), Some(l)) => newer(l, i),
        _ => false,
    };
    UpdateItem {
        installed,
        latest,
        update_available,
        error,
        source: source.to_string(),
        can_update: can_update_for(update_available, source),
        pinned: false,
    }
}

/// RE-UE4SS is pinned to `UE4SS_VERSION`: never offered as an update.
pub fn build_pinned_ue4ss_item(present: bool, source: &str) -> UpdateItem {
    UpdateItem {
        installed: present.then(|| UE4SS_VERSION.to_string()),
        latest: Some(UE4SS_VERSION.to_string()),
        update_available: false,
        error: None,
        source: source.to_string(),
        can_update: false,
        pinned: true,
    }
}

/// Current location, source classification and installed version of one component.
pub struct ComponentLocation {
    pub dir: Option<PathBuf>,
    pub source: &'static str,
    pub installed: Option<String>,
}

pub fn locate_component(
    settings: &StudioSettings,
    profile: &str,
    component: &str,
) -> Option<ComponentLocation> {
    match component {
        "foundation" | "oodle" => {
            let root = locations::active_foundation_root(&settings.foundation_root);
            let (bin_dir, _) = resolve_binary_dir(&root, profile);
            if component == "foundation" {
                let present = ["kawari-run.exe", "kawari-world.exe", "kawari-lobby.exe"]
                    .iter()
                    .all(|b| bin_dir.join(b).is_file());
                Some(ComponentLocation {
                    source: locations::classify_source(
                        Some(&root),
                        CheckoutKind::Foundation,
                        present,
                    ),
                    installed: read_version_marker(&root),
                    dir: Some(root),
                })
            } else {
                let present = root
                    .join("oodle")
                    .join("oodle-network-shared.dll")
                    .is_file()
                    || bin_dir.join("oodle-network-shared.dll").is_file();
                Some(ComponentLocation {
                    source: locations::classify_source(
                        Some(&root),
                        CheckoutKind::Foundation,
                        present,
                    ),
                    installed: read_oodle_version(&root, &bin_dir),
                    dir: Some(root),
                })
            }
        }
        "rmp_bridge" => {
            let dir = locations::active_bridge_root();
            let present = dir.as_deref().is_some_and(locations::bridge_files_present);
            Some(ComponentLocation {
                source: locations::classify_source(dir.as_deref(), CheckoutKind::Bridge, present),
                installed: dir.as_deref().and_then(locations::read_bridge_version),
                dir,
            })
        }
        "ue4ss" => {
            let dir = locations::active_bridge_root();
            let present = dir.as_deref().is_some_and(|d| {
                let ue = d.join("vendor").join("ue4ss-v4.0.0");
                ue.join("dwmapi.dll").is_file() && ue.join("ue4ss").join("UE4SS.dll").is_file()
            });
            Some(ComponentLocation {
                source: locations::classify_source(dir.as_deref(), CheckoutKind::Bridge, present),
                installed: present.then(|| UE4SS_VERSION.to_string()),
                dir,
            })
        }
        "remakeplace" => {
            let dir = try_resolve_remakeplace_source_dir(&settings.remakeplace_path);
            let present = dir.is_some();
            Some(ComponentLocation {
                source: locations::classify_source(dir.as_deref(), CheckoutKind::Other, present),
                installed: read_installed_version(&settings.remakeplace_path),
                dir,
            })
        }
        _ => None,
    }
}

/// Validates that `component` can be updated in place; returns the target directory.
pub fn check_updatable(loc: &ComponentLocation, component: &str) -> Result<PathBuf, String> {
    match (loc.source, loc.dir.as_ref()) {
        (SOURCE_DEV, Some(dir)) => Err(locations::dev_checkout_error(dir)),
        (SOURCE_MANAGED | SOURCE_CUSTOM, Some(dir)) => Ok(dir.clone()),
        _ => Err(format!("{NOT_INSTALLED_PREFIX} {component}")),
    }
}

impl SetupManager {
    async fn latest_tag_allow_missing(&self, api: &str) -> Result<Option<String>, String> {
        match self.fetch_latest_github_release(api).await {
            Ok(rel) => Ok(rel.tag_name.filter(|t| !t.trim().is_empty())),
            // No published releases yet -> GitHub answers 404 for /releases/latest.
            Err(err) if err.contains("404") => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Latest rmp-bridge revision: release tag if any, else the short `main` commit sha.
    /// Returns `(ref, is_commit)`.
    async fn latest_bridge_ref(&self) -> Result<Option<(String, bool)>, String> {
        if let Some(tag) = self
            .latest_tag_allow_missing(RMP_BRIDGE_LATEST_RELEASE_API)
            .await?
        {
            return Ok(Some((tag, false)));
        }
        let sha = self
            .fetch_main_commit_sha(RMP_BRIDGE_MAIN_COMMIT_API)
            .await?;
        Ok(Some((locations::short_sha(&sha), true)))
    }

    pub async fn check_runtime_updates(
        &self,
        settings: &StudioSettings,
        profile: &str,
    ) -> RuntimeUpdates {
        let loc = |c| locate_component(settings, profile, c).expect("known component");
        let foundation = loc("foundation");
        let oodle = loc("oodle");
        let bridge = loc("rmp_bridge");
        let ue4ss = loc("ue4ss");
        let rmp = loc("remakeplace");

        let (foundation_latest, rmp_latest, oodle_latest, bridge_latest) = tokio::join!(
            self.latest_tag_allow_missing(FOUNDATION_LATEST_RELEASE_API),
            self.latest_remakeplace_tag(),
            self.latest_tag_allow_missing(OODLE_LATEST_RELEASE_API),
            self.latest_bridge_ref(),
        );

        let bridge_item = match bridge_latest {
            Ok(Some((latest, true))) => build_item_with(
                bridge.installed,
                Ok(Some(latest)),
                bridge.source,
                commit_differs,
            ),
            Ok(Some((latest, false))) => {
                build_item_with(bridge.installed, Ok(Some(latest)), bridge.source, is_newer)
            }
            Ok(None) => build_item_with(bridge.installed, Ok(None), bridge.source, is_newer),
            Err(e) => build_item_with(bridge.installed, Err(e), bridge.source, is_newer),
        };

        RuntimeUpdates {
            foundation: build_item_with(
                foundation.installed,
                foundation_latest,
                foundation.source,
                is_newer,
            ),
            remakeplace: build_item_with(rmp.installed, rmp_latest, rmp.source, is_newer),
            oodle: build_item_with(oodle.installed, oodle_latest, oodle.source, is_newer),
            rmp_bridge: bridge_item,
            ue4ss: build_pinned_ue4ss_item(ue4ss.installed.is_some(), ue4ss.source),
        }
    }

    /// Reinstalls `component` into its current (managed or custom) location.
    pub async fn update_component(
        &self,
        app: &AppHandle,
        settings: &StudioSettings,
        component: &str,
        profile: &str,
    ) -> Result<ComponentUpdateReport, String> {
        let component = component.trim().to_ascii_lowercase();
        if !UPDATABLE_COMPONENTS.contains(&component.as_str()) {
            return Err(format!("Unknown update component: {component}"));
        }
        let loc = locate_component(settings, profile, &component)
            .ok_or_else(|| format!("Unknown update component: {component}"))?;
        let target = check_updatable(&loc, &component)?;
        let previous = loc.installed.clone();

        if component == "remakeplace" {
            self.install_remakeplace(Some(app), &target, false).await?;
        } else {
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
            let res = match component.as_str() {
                "foundation" => self.install_foundation(Some(app), &target).await,
                "oodle" => self.install_oodle(Some(app), &target, profile).await,
                _ => self.install_rmp_bridge(Some(app), &target).await,
            };
            match &res {
                Ok(()) => super::emit_progress(
                    Some(app),
                    &component,
                    "done",
                    Some(100),
                    0,
                    None,
                    format!("Updated {component}"),
                ),
                Err(err) => {
                    super::emit_progress(Some(app), &component, "error", None, 0, None, err.clone())
                }
            }
            res?;
        }

        let current = locate_component(settings, profile, &component).and_then(|l| l.installed);
        Ok(ComponentUpdateReport {
            component,
            previous,
            current,
            status: self.inspect_status(settings, profile),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_and_builds_items() {
        assert!(is_newer("v7.57.2", "v7.57.1"));
        assert!(!is_newer("v7.57.1", "7.57.1"));
        assert!(!is_newer("v7.57", "v7.57.0"));
        assert!(!is_newer("v7.56.9", "v7.57.1"));
        assert!(is_newer("2026-06-04-1357", "2025-01-01-0000"));
        assert!(is_newer("nightly-b", "nightly-a"));

        let item = build_item(Some("v1.0.0".into()), Ok(Some("v1.1.0".into())));
        assert!(item.update_available);
        let none = build_item(None, Ok(None));
        assert!(!none.update_available && none.error.is_none());
        let err = build_item(Some("v1".into()), Err("boom".into()));
        assert!(!err.update_available);
        assert_eq!(err.error.as_deref(), Some("boom"));
    }

    #[test]
    fn can_update_only_for_managed_or_custom() {
        let newer = || Ok(Some("v2".to_string()));
        for (source, expect) in [
            (SOURCE_MANAGED, true),
            (SOURCE_CUSTOM, true),
            (SOURCE_DEV, false),
            (SOURCE_MISSING, false),
        ] {
            let item = build_item_with(Some("v1".into()), newer(), source, is_newer);
            assert!(item.update_available);
            assert_eq!(item.can_update, expect, "source {source}");
            assert_eq!(item.source, source);
            assert!(!item.pinned);
        }
        let same = build_item_with(Some("v2".into()), newer(), SOURCE_MANAGED, is_newer);
        assert!(!same.update_available && !same.can_update);
    }

    #[test]
    fn commit_refs_compare_by_inequality() {
        assert!(!commit_differs("abc1234", "abc1234"));
        assert!(!commit_differs("abc1234", "abc1234def5678"));
        assert!(commit_differs("abc1234", "def5678"));
        let item = build_item_with(
            Some("def5678".into()),
            Ok(Some("abc1234".into())),
            SOURCE_CUSTOM,
            commit_differs,
        );
        assert!(item.update_available && item.can_update);
    }

    #[test]
    fn ue4ss_item_is_pinned() {
        let item = build_pinned_ue4ss_item(true, SOURCE_MANAGED);
        assert!(item.pinned && !item.update_available && !item.can_update);
        assert_eq!(item.installed.as_deref(), Some(UE4SS_VERSION));
        assert_eq!(item.latest.as_deref(), Some(UE4SS_VERSION));
        let missing = build_pinned_ue4ss_item(false, SOURCE_MISSING);
        assert!(missing.installed.is_none() && missing.pinned);

        let json = serde_json::to_value(&item).unwrap();
        for key in ["canUpdate", "pinned", "source", "updateAvailable"] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn check_updatable_refuses_dev_and_missing() {
        let dir = PathBuf::from(r"C:\x\Foundation");
        let mk = |source| ComponentLocation {
            dir: Some(dir.clone()),
            source,
            installed: None,
        };
        assert!(
            check_updatable(&mk(SOURCE_DEV), "foundation")
                .unwrap_err()
                .starts_with(locations::DEV_CHECKOUT_PREFIX)
        );
        assert!(
            check_updatable(&mk(SOURCE_MISSING), "foundation")
                .unwrap_err()
                .starts_with(NOT_INSTALLED_PREFIX)
        );
        assert_eq!(check_updatable(&mk(SOURCE_MANAGED), "oodle").unwrap(), dir);
        assert_eq!(check_updatable(&mk(SOURCE_CUSTOM), "oodle").unwrap(), dir);
    }
}
