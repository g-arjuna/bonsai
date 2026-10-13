use chrono::{DateTime, Utc};
use rbmp_core::bgp::types::{LlgrState, PathAttributes, Prefix};
use rbmp_core::bmp::types::{RibType, TableRouteKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;

/// A route entry in the RIB
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RibEntry {
    pub prefix: Prefix,
    /// Add-Path path ID (RFC 7911); None when Add-Path not active for this AFI-SAFI
    pub path_id: Option<u32>,
    pub attributes: PathAttributes,
    pub received_at: DateTime<Utc>,
    pub peer_addr: IpAddr,
    pub peer_as: u32,
    /// True when this is the best-path among multiple paths for the same prefix
    pub is_best: bool,
    /// LLGR stale state for this route (RFC 9494)
    pub llgr_state: LlgrState,
    /// Wall-clock time at which this route was marked stale (set by LLGR timer)
    pub stale_at: Option<DateTime<Utc>>,
}

/// Per-peer, per-RIB-type route table keyed by typed `TableRouteKey` (`CanonicalNlri` + `path_id`).
#[derive(Debug, Default)]
pub struct RibTable {
    /// rib_type → TableRouteKey → entry
    tables: HashMap<RibType, HashMap<TableRouteKey, RibEntry>>,
}

impl RibTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a route. Returns true if this is a new prefix/path, false if update.
    pub fn insert(&mut self, rib: RibType, entry: RibEntry) -> bool {
        let key = TableRouteKey::from_prefix(&entry.prefix, entry.path_id);
        let table = self.tables.entry(rib).or_default();
        let is_new = !table.contains_key(&key);
        table.insert(key, entry);
        is_new
    }

    /// Remove a route (uses prefix + path_id for compound key). Returns the removed entry.
    pub fn remove(&mut self, rib: RibType, prefix: &Prefix) -> Option<RibEntry> {
        self.remove_with_path_id(rib, prefix, None)
    }

    pub fn remove_with_path_id(
        &mut self,
        rib: RibType,
        prefix: &Prefix,
        path_id: Option<u32>,
    ) -> Option<RibEntry> {
        let key = TableRouteKey::from_prefix(prefix, path_id);
        self.tables.get_mut(&rib)?.remove(&key)
    }

    pub fn get(&self, rib: RibType, prefix: &Prefix) -> Option<&RibEntry> {
        self.get_with_path_id(rib, prefix, None)
    }

    pub fn get_with_path_id(
        &self,
        rib: RibType,
        prefix: &Prefix,
        path_id: Option<u32>,
    ) -> Option<&RibEntry> {
        let key = TableRouteKey::from_prefix(prefix, path_id);
        self.tables.get(&rib)?.get(&key)
    }

    /// Recompute best-path among multiple Add-Path entries for a prefix.
    /// Tie-breakers (in order): highest LOCAL_PREF, shortest AS_PATH, lowest MED.
    pub fn recompute_best_path(&mut self, rib: RibType, prefix: &Prefix) {
        let table = match self.tables.get_mut(&rib) {
            Some(t) => t,
            None => return,
        };
        let matching_keys: Vec<TableRouteKey> = table
            .keys()
            .filter(|k| k.matches_prefix(prefix))
            .cloned()
            .collect();
        if matching_keys.len() <= 1 {
            if let Some(key) = matching_keys.first() {
                if let Some(e) = table.get_mut(key) {
                    e.is_best = true;
                }
            }
            return;
        }
        let best_key = matching_keys
            .iter()
            .min_by_key(|k| {
                let e = &table[*k];
                let lp = e
                    .attributes
                    .local_pref
                    .map(|v| u32::MAX - v)
                    .unwrap_or(u32::MAX);
                let hop = e
                    .attributes
                    .as_path
                    .as_ref()
                    .map(|p| p.hop_count() as u32)
                    .unwrap_or(0);
                let med = e.attributes.multi_exit_disc.unwrap_or(u32::MAX);
                (lp, hop, med)
            })
            .cloned();
        for k in &matching_keys {
            if let Some(e) = table.get_mut(k) {
                e.is_best = best_key.as_ref() == Some(k);
            }
        }
    }

    pub fn count(&self, rib: RibType) -> usize {
        self.tables.get(&rib).map_or(0, |t| t.len())
    }

    pub fn iter_rib(&self, rib: RibType) -> impl Iterator<Item = &RibEntry> {
        self.tables.get(&rib).into_iter().flat_map(|t| t.values())
    }

    pub fn all_prefixes(&self) -> Vec<&RibEntry> {
        self.tables.values().flat_map(|t| t.values()).collect()
    }

    /// Clear all routes for a specific RIB type (e.g., on Peer Down)
    pub fn clear_rib(&mut self, rib: RibType) {
        self.tables.remove(&rib);
    }

    /// Clear all routes for this peer entirely
    pub fn clear_all(&mut self) {
        self.tables.clear();
    }

    pub fn rib_counts(&self) -> HashMap<RibType, usize> {
        self.tables.iter().map(|(k, v)| (*k, v.len())).collect()
    }

    /// Mark every route in this table as LLGR-stale (called on peer Down when LLGR active).
    /// Records the stale timestamp so callers can later expire entries.
    pub fn mark_stale_all(&mut self, at: DateTime<Utc>) {
        for table in self.tables.values_mut() {
            for entry in table.values_mut() {
                if entry.llgr_state == LlgrState::Normal {
                    entry.llgr_state = LlgrState::StaleMarked;
                    entry.stale_at = Some(at);
                }
            }
        }
    }

    /// Remove all entries whose stale timer has expired (stale_at + stale_secs <= now).
    /// Returns the number of routes deleted.
    pub fn drain_deleted_stale(&mut self, now: DateTime<Utc>, stale_secs: u32) -> usize {
        let mut removed = 0usize;
        for table in self.tables.values_mut() {
            table.retain(|_, entry| {
                if entry.llgr_state == LlgrState::StaleMarked {
                    if let Some(stale_at) = entry.stale_at {
                        if (now - stale_at).num_seconds() >= stale_secs as i64 {
                            entry.llgr_state = LlgrState::Deleted;
                            removed += 1;
                            return false;
                        }
                    }
                }
                true
            });
        }
        removed
    }
}
