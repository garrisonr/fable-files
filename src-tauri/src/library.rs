//! Library Memory settings + retention (Indexer v2).
//!
//! Dickles' ruling (2026-10-14, from his 2019 wishlist): "Fable
//! should remember past drives until you tell it to forget."
//! Remembered volumes live in the catalog (`volumes` table); the
//! retention POLICY lives here, in a JSON sidecar beside the other
//! app settings files (the rommedia.rs pattern: atomic tmp+rename,
//! defaults on any read failure — a corrupt settings file must
//! never cost anyone their library).
//!
//! Retention law:
//! - The global default is Forever (None). Windows are 30/60/90/
//!   180/365 days; a per-volume override replaces the global.
//! - Pruning touches ONLY offline volumes past their window: their
//!   catalog rows are purged and the volume record is dropped.
//! - A connected volume is NEVER pruned, and the root filesystem
//!   (the volume mounted at "/") is never pruned or forgotten —
//!   it holds Home, and it cannot be "away" while the app runs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ufm_catalog::{Catalog, VolumeRecord};

/// Retention windows the Preferences UI offers, in days.
pub const RETENTION_DAYS: [u32; 5] = [30, 60, 90, 180, 365];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibrarySettings {
    /// Global default retention for offline volumes, in days.
    /// None = Forever (Dickles' default).
    #[serde(default)]
    pub retention_days: Option<u32>,
    /// Per-volume overrides keyed by filesystem UUID. A present key
    /// with None means "Forever for this volume, whatever the
    /// global says"; an absent key inherits the global.
    #[serde(default)]
    pub overrides: HashMap<String, Option<u32>>,
}

impl LibrarySettings {
    /// The retention that applies to one volume: its override when
    /// set, else the global default. None = keep forever.
    pub fn effective_retention(&self, fs_uuid: &str) -> Option<u32> {
        match self.overrides.get(fs_uuid) {
            Some(days) => *days,
            None => self.retention_days,
        }
    }

    /// Reject windows the UI never offers (a hand-edited file or a
    /// future version's values must not silently prune at 7 days).
    pub fn validate(&self) -> Result<(), String> {
        let check = |days: Option<u32>| -> Result<(), String> {
            match days {
                None => Ok(()),
                Some(d) if RETENTION_DAYS.contains(&d) => Ok(()),
                Some(d) => Err(format!(
                    "retention of {d} days is not one of 30/60/90/180/365"
                )),
            }
        };
        check(self.retention_days)?;
        for days in self.overrides.values() {
            check(*days)?;
        }
        Ok(())
    }
}

pub fn settings_path(base: &Path) -> PathBuf {
    base.join("library-settings.json")
}

pub fn load_settings(base: &Path) -> LibrarySettings {
    let path = settings_path(base);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => LibrarySettings::default(),
    }
}

pub fn save_settings(base: &Path, settings: &LibrarySettings) -> Result<(), String> {
    settings.validate()?;
    let path = settings_path(base);
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, data).map_err(|e| format!("write library settings: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("save library settings: {e}"))?;
    Ok(())
}

/// The system volume: the one whose mount history contains "/".
/// It is never pruned and never forgotten.
pub fn is_system_volume(record: &VolumeRecord) -> bool {
    record.mount_history.iter().any(|m| m.path == "/")
}

/// Every mount path this volume has ever been seen at — the purge
/// prefixes for Forget / prune / rebuild.
pub fn volume_prefixes(record: &VolumeRecord) -> Vec<String> {
    record.mount_history.iter().map(|m| m.path.clone()).collect()
}

/// Prune offline volumes whose retention window has closed.
///
/// Safety rails, in order: enumeration is the caller's job and an
/// EMPTY mounted set must never reach this function (the caller
/// treats empty as "enumeration failed" and skips pruning);
/// mounted volumes are untouchable; the system volume is
/// untouchable; Forever (None) is untouchable. A volume prunes when
/// now - last_seen exceeds its effective window. Returns the UUIDs
/// pruned (their items were purged and records deleted).
pub fn prune_expired_volumes(
    catalog: &Catalog,
    settings: &LibrarySettings,
    mounted_uuids: &HashSet<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let mut pruned = Vec::new();
    let volumes = match catalog.volumes() {
        Ok(v) => v,
        Err(_) => return pruned,
    };
    for record in &volumes {
        if mounted_uuids.contains(&record.fs_uuid) || is_system_volume(record) {
            continue;
        }
        let Some(days) = settings.effective_retention(&record.fs_uuid) else {
            continue; // Forever
        };
        let last_seen = match chrono::DateTime::parse_from_rfc3339(&record.last_seen) {
            Ok(dt) => dt.with_timezone(&chrono::Utc),
            Err(_) => continue, // unparseable history: keep, never guess
        };
        if now - last_seen <= chrono::Duration::days(days as i64) {
            continue;
        }
        let prefixes = volume_prefixes(record);
        if catalog.purge_items_under_prefixes(&prefixes).is_ok()
            && catalog.delete_volume(&record.fs_uuid).is_ok()
        {
            pruned.push(record.fs_uuid.clone());
        }
    }
    pruned
}

/// One volume as Preferences shows it: identity, live state, and the
/// retention that currently applies.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeSummary {
    pub fs_uuid: String,
    pub label: String,
    pub online: bool,
    /// Where the volume is mounted now, or was last seen mounted.
    pub mount_point: Option<String>,
    pub last_seen: String,
    pub last_indexed: Option<String>,
    /// Catalog items remembered on this volume.
    pub item_count: u64,
    /// True for the root-filesystem volume (never pruned/forgotten).
    pub is_system: bool,
    /// The retention currently in force (None = Forever).
    pub retention_days: Option<u32>,
    /// Whether a per-volume override exists, and its value.
    pub has_override: bool,
    pub override_days: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ufm_core::{ItemId, LocationId};

    fn settings(global: Option<u32>) -> LibrarySettings {
        LibrarySettings {
            retention_days: global,
            overrides: HashMap::new(),
        }
    }

    #[test]
    fn retention_resolution_and_validation() {
        let mut s = settings(Some(90));
        assert_eq!(s.effective_retention("a"), Some(90));
        s.overrides.insert("a".into(), None);
        assert_eq!(s.effective_retention("a"), None, "override Forever wins");
        s.overrides.insert("b".into(), Some(30));
        assert_eq!(s.effective_retention("b"), Some(30));
        assert!(s.validate().is_ok());
        s.retention_days = Some(7);
        assert!(s.validate().is_err(), "unlisted windows are rejected");
    }

    fn seed_volume(cat: &Catalog, uuid: &str, mount: &str, last_seen: &str, files: usize) {
        cat.record_volume_sighting(uuid, Some("Drive"), mount, last_seen)
            .unwrap();
        let loc_id = LocationId::new();
        cat.insert_location(&ufm_vfs::Location {
            id: loc_id,
            provider: "local".into(),
            display_name: "Drive".into(),
            capabilities: ufm_vfs::Capabilities::default(),
            protected: ufm_vfs::ProtectionTier::None,
        })
        .unwrap();
        for i in 0..files {
            cat.insert_item(&ufm_vfs::Item {
                id: ItemId::new(),
                location: loc_id,
                name: format!("f{i}.txt"),
                kind: ufm_vfs::ItemKind::File,
                size_bytes: Some(1),
                modified: None,
                provider_key: Some(format!("{mount}/f{i}.txt")),
                labels: vec![],
                starred: false,
            })
            .unwrap();
        }
    }

    #[test]
    fn prune_touches_only_expired_offline_volumes() {
        let cat = Catalog::open_in_memory().unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // Expired: last seen 100 days ago, window 30.
        seed_volume(&cat, "old", "/media/old", "2026-07-07T00:00:00Z", 3);
        // Within window: seen 10 days ago.
        seed_volume(&cat, "recent", "/media/recent", "2026-10-05T00:00:00Z", 2);
        // System volume: ancient, but holds "/".
        seed_volume(&cat, "sys", "/", "2020-01-01T00:00:00Z", 2);
        // Expired but MOUNTED right now.
        seed_volume(&cat, "mounted", "/media/mounted", "2026-07-07T00:00:00Z", 2);

        let mounted: HashSet<String> = ["mounted".to_string()].into_iter().collect();
        let pruned = prune_expired_volumes(&cat, &settings(Some(30)), &mounted, now);
        assert_eq!(pruned, vec!["old".to_string()]);
        assert!(cat.volume("old").unwrap().is_none());
        assert_eq!(
            cat.item_count_under_prefixes(&["/media/old".to_string()])
                .unwrap(),
            0,
            "the expired volume's rows are gone"
        );
        for kept in ["recent", "sys", "mounted"] {
            assert!(cat.volume(kept).unwrap().is_some(), "{kept} must survive");
        }
        assert_eq!(
            cat.item_count_under_prefixes(&["/media/recent".to_string()])
                .unwrap(),
            2
        );

        // Forever prunes nothing, however old.
        let pruned = prune_expired_volumes(&cat, &settings(None), &mounted, now);
        assert!(pruned.is_empty());
    }
}

pub fn summarize(
    catalog: &Catalog,
    record: &VolumeRecord,
    online_mount: Option<&str>,
    settings: &LibrarySettings,
) -> VolumeSummary {
    let prefixes = volume_prefixes(record);
    let item_count = catalog.item_count_under_prefixes(&prefixes).unwrap_or(0);
    let has_override = settings.overrides.contains_key(&record.fs_uuid);
    VolumeSummary {
        fs_uuid: record.fs_uuid.clone(),
        label: record
            .label
            .clone()
            .unwrap_or_else(|| "Unnamed drive".to_string()),
        online: online_mount.is_some(),
        mount_point: online_mount
            .map(|s| s.to_string())
            .or_else(|| record.mount_history.last().map(|m| m.path.clone())),
        last_seen: record.last_seen.clone(),
        last_indexed: record.last_indexed.clone(),
        item_count,
        is_system: is_system_volume(record),
        retention_days: settings.effective_retention(&record.fs_uuid),
        has_override,
        override_days: settings.overrides.get(&record.fs_uuid).copied().flatten(),
    }
}
