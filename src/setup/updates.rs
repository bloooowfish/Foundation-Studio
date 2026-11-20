//! Runtime update check against the official GitHub "latest release" endpoints.

use serde::Serialize;

use super::{
    FOUNDATION_LATEST_RELEASE_API, OODLE_LATEST_RELEASE_API, SetupManager, read_oodle_version,
    read_version_marker,
};
use crate::server::{resolve_binary_dir, resolve_foundation_root};
use crate::settings::{StudioSettings, managed_foundation_dir};
use crate::setup::remakeplace::read_installed_version;

#[derive(Clone, Debug, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateItem {
    pub installed: Option<String>,
    pub latest: Option<String>,
    pub update_available: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpdates {
    pub foundation: UpdateItem,
    pub remakeplace: UpdateItem,
    pub oodle: UpdateItem,
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

pub fn build_item(installed: Option<String>, latest: Result<Option<String>, String>) -> UpdateItem {
    let (latest, error) = match latest {
        Ok(v) => (v, None),
        Err(e) => (None, Some(e)),
    };
    let update_available = match (&installed, &latest) {
        (Some(i), Some(l)) => is_newer(l, i),
        _ => false,
    };
    UpdateItem {
        installed,
        latest,
        update_available,
        error,
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

    pub async fn check_runtime_updates(
        &self,
        settings: &StudioSettings,
        profile: &str,
    ) -> RuntimeUpdates {
        let root = resolve_foundation_root(&settings.foundation_root).unwrap_or_else(|| {
            let trimmed = settings.foundation_root.trim().trim_matches('"');
            if trimmed.is_empty() {
                managed_foundation_dir()
            } else {
                std::path::PathBuf::from(trimmed)
            }
        });
        let (bin_dir, _) = resolve_binary_dir(&root, profile);
        let foundation_installed = read_version_marker(&root);
        let oodle_installed = read_oodle_version(&root, &bin_dir);
        let rmp_installed = read_installed_version(&settings.remakeplace_path);

        let (foundation_latest, rmp_latest, oodle_latest) = tokio::join!(
            self.latest_tag_allow_missing(FOUNDATION_LATEST_RELEASE_API),
            self.latest_remakeplace_tag(),
            self.latest_tag_allow_missing(OODLE_LATEST_RELEASE_API),
        );

        RuntimeUpdates {
            foundation: build_item(foundation_installed, foundation_latest),
            remakeplace: build_item(rmp_installed, rmp_latest),
            oodle: build_item(oodle_installed, oodle_latest),
        }
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
}
