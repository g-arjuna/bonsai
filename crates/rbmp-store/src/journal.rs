use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{Context, Result, anyhow};
use rbmp_core::bmp::types::{BmpMessage, BmpPayload};
use rbmp_rib::RibManager;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Summary returned after replaying an append-only BMP journal into a `RibManager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayReport {
    pub applied_messages: usize,
    pub skipped_truncated_tail: usize,
}

/// Durable append-only JSONL journal of raw `BmpMessage` records for crash recovery.
pub struct BmpJournal {
    path: Option<PathBuf>,
    file: Option<Arc<Mutex<tokio::fs::File>>>,
}

impl BmpJournal {
    pub async fn open(path: Option<impl AsRef<Path>>) -> Result<Self> {
        match path {
            None => Ok(Self {
                path: None,
                file: None,
            }),
            Some(p) => {
                let pb = p.as_ref().to_path_buf();
                if let Some(parent) = pb.parent() {
                    if !parent.as_os_str().is_empty() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                }
                let f = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&pb)
                    .await?;
                Ok(Self {
                    path: Some(pb),
                    file: Some(Arc::new(Mutex::new(f))),
                })
            }
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Serialize `msg` as a single JSONL record, flush, and fsync data before returning.
    pub async fn append(&self, msg: &BmpMessage) -> Result<()> {
        if let Some(file) = &self.file {
            let mut line = serde_json::to_vec(msg)?;
            line.push(b'\n');
            let mut guard = file.lock().await;
            guard.write_all(&line).await?;
            guard.flush().await?;
            guard.sync_data().await?;
        }
        Ok(())
    }

    /// Replay a journal file deterministically into `rib`.
    /// If the final line is truncated (no trailing newline or partial JSON from an unclean crash),
    /// it is skipped and counted in `ReplayReport::skipped_truncated_tail`.
    /// Any corruption in a non-final line returns an `Err`.
    pub fn replay_into(path: impl AsRef<Path>, rib: &mut RibManager) -> Result<ReplayReport> {
        let raw = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read BMP journal {}", path.as_ref().display()))?;
        if raw.is_empty() {
            return Ok(ReplayReport {
                applied_messages: 0,
                skipped_truncated_tail: 0,
            });
        }

        let ends_with_newline = raw.ends_with('\n');
        let lines: Vec<&str> = raw.split('\n').collect();
        // If `raw` ended with '\n', the last element of `split('\n')` is an empty string "".
        let non_empty_count = if ends_with_newline {
            lines.len().saturating_sub(1)
        } else {
            lines.len()
        };

        let mut active_speakers: HashSet<IpAddr> = HashSet::new();
        let mut applied_messages = 0usize;
        let mut skipped_truncated_tail = 0usize;

        for (idx, line) in lines.into_iter().take(non_empty_count).enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let is_last = idx + 1 == non_empty_count;
            match serde_json::from_str::<BmpMessage>(trimmed) {
                Ok(msg) => {
                    let speaker = msg.speaker_addr;
                    let is_term = matches!(msg.payload, BmpPayload::Termination { .. });
                    if active_speakers.insert(speaker) {
                        let _ = rib.open_speaker_session(speaker, msg.received_at);
                    }
                    rib.process(msg);
                    if is_term {
                        active_speakers.remove(&speaker);
                    }
                    applied_messages += 1;
                }
                Err(e) if is_last && !ends_with_newline => {
                    skipped_truncated_tail += 1;
                    let _ = e;
                }
                Err(e) => {
                    return Err(anyhow!(
                        "corrupted BMP journal record at line {}: {}",
                        idx + 1,
                        e
                    ));
                }
            }
        }

        Ok(ReplayReport {
            applied_messages,
            skipped_truncated_tail,
        })
    }
}

/// Before/after prefix transition entry reconstructed from durable `RibEvent` records.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PrefixHistoryItem {
    pub sequence: u64,
    pub event_id: uuid::Uuid,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub speaker_addr: IpAddr,
    pub peer_addr: IpAddr,
    pub peer_as: u32,
    pub rib_type: String,
    pub action: String,
    pub prefix: String,
    pub path_id: Option<u32>,
    pub before_attributes: Option<rbmp_core::bgp::types::PathAttributes>,
    pub after_attributes: Option<rbmp_core::bgp::types::PathAttributes>,
}

/// Durable append-only JSONL journal of `RibEvent` records for prefix-history queries
/// and downstream checkpointed projection (such as the Bonsai graph summary projector).
pub struct RibEventJournal {
    path: Option<PathBuf>,
    file: Option<Arc<Mutex<tokio::fs::File>>>,
    events: Arc<Mutex<Vec<rbmp_rib::event::RibEvent>>>,
}

impl RibEventJournal {
    pub async fn open(path: Option<impl AsRef<Path>>) -> Result<Self> {
        let mut initial_events = Vec::new();
        let (pb_opt, file_opt) = match path {
            None => (None, None),
            Some(p) => {
                let pb = p.as_ref().to_path_buf();
                if let Some(parent) = pb.parent() {
                    if !parent.as_os_str().is_empty() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                }
                if pb.exists() {
                    let raw = tokio::fs::read_to_string(&pb).await.unwrap_or_default();
                    let ends_with_newline = raw.ends_with('\n');
                    let lines: Vec<&str> = raw.split('\n').collect();
                    let non_empty_count = if ends_with_newline {
                        lines.len().saturating_sub(1)
                    } else {
                        lines.len()
                    };
                    for (idx, line) in lines.into_iter().take(non_empty_count).enumerate() {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let is_last = idx + 1 == non_empty_count;
                        match serde_json::from_str::<rbmp_rib::event::RibEvent>(trimmed) {
                            Ok(ev) => initial_events.push(ev),
                            Err(_) if is_last && !ends_with_newline => {
                                // Ignore truncated crash tail
                            }
                            Err(e) => {
                                return Err(anyhow!(
                                    "corrupted RibEventJournal record at line {}: {}",
                                    idx + 1,
                                    e
                                ));
                            }
                        }
                    }
                }
                let f = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&pb)
                    .await?;
                (Some(pb), Some(Arc::new(Mutex::new(f))))
            }
        };
        Ok(Self {
            path: pb_opt,
            file: file_opt,
            events: Arc::new(Mutex::new(initial_events)),
        })
    }

    pub async fn append(&self, ev: &rbmp_rib::event::RibEvent) -> Result<()> {
        if let Some(file) = &self.file {
            let mut line = serde_json::to_vec(ev)?;
            line.push(b'\n');
            let mut guard = file.lock().await;
            guard.write_all(&line).await?;
            guard.flush().await?;
            guard.sync_data().await?;
        }
        let mut guard = self.events.lock().await;
        guard.push(ev.clone());
        metrics::counter!("rib_events_written_total").increment(1);
        Ok(())
    }

    pub async fn latest_seq(&self) -> u64 {
        let guard = self.events.lock().await;
        guard.last().map(|e| e.event_key.sequence).unwrap_or(0)
    }

    pub async fn total_events(&self) -> usize {
        let guard = self.events.lock().await;
        guard.len()
    }

    pub async fn events_after_seq(
        &self,
        after_seq: u64,
        limit: usize,
    ) -> Vec<rbmp_rib::event::RibEvent> {
        let guard = self.events.lock().await;
        guard
            .iter()
            .filter(|e| e.event_key.sequence > after_seq)
            .take(limit)
            .cloned()
            .collect()
    }

    pub async fn prefix_history(&self, prefix: &str, limit: usize) -> Vec<PrefixHistoryItem> {
        use rbmp_rib::event::{RibEventPayload, RouteAction};
        use std::collections::HashMap;

        let guard = self.events.lock().await;
        let mut last_attrs_by_route: HashMap<
            rbmp_core::bmp::types::RouteKey,
            Option<rbmp_core::bgp::types::PathAttributes>,
        > = HashMap::new();
        let mut items = Vec::new();

        for ev in guard.iter() {
            if let RibEventPayload::RouteChange(rc) = &ev.payload {
                let pfx_str = rc.prefix.to_string();
                let prev = last_attrs_by_route
                    .get(&rc.route_key)
                    .cloned()
                    .flatten();
                let next = match rc.action {
                    RouteAction::Announce => rc.attributes.clone(),
                    RouteAction::Withdraw => None,
                };
                last_attrs_by_route.insert(rc.route_key.clone(), next.clone());

                if pfx_str == prefix {
                    items.push(PrefixHistoryItem {
                        sequence: ev.event_key.sequence,
                        event_id: ev.id,
                        occurred_at: ev.occurred_at,
                        speaker_addr: ev.speaker,
                        peer_addr: rc.peer_header.peer_address,
                        peer_as: rc.peer_header.peer_as,
                        rib_type: format!("{:?}", rc.rib_type),
                        action: match rc.action {
                            RouteAction::Announce => "announce".to_string(),
                            RouteAction::Withdraw => "withdraw".to_string(),
                        },
                        prefix: pfx_str,
                        path_id: rc.path_id,
                        before_attributes: prev,
                        after_attributes: next,
                    });
                }
            }
        }
        items.reverse();
        items.truncate(limit);
        items
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rbmp_core::bgp::types::{AfiSafi, AsPath, AsPathSegment, BgpUpdate, Origin, PathAttributes, Prefix};
    use rbmp_core::bmp::types::{
        BgpOpenInfo, BmpMessage, BmpPayload, PeerFlags, PeerHeader, PeerKey, PeerType,
        PeerUpMessage, RibType,
    };
    use std::io::Write;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;

    fn sample_peer_header() -> PeerHeader {
        PeerHeader {
            peer_type: PeerType::GlobalInstance,
            peer_flags: PeerFlags(0),
            peer_distinguisher: [0u8; 8],
            peer_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)),
            peer_as: 65002,
            peer_bgp_id: Ipv4Addr::new(10, 0, 0, 2),
            timestamp: Utc::now(),
            rib_type: RibType::AdjRibInPrePolicy,
        }
    }

    #[tokio::test]
    async fn b06_durable_journal_replay_reconstructs_identical_rib_state_after_truncated_crash_tail() {
        let tmp_path = std::env::temp_dir().join(format!("rbmp-b06-journal-{}.jsonl", Uuid::new_v4()));
        let journal = BmpJournal::open(Some(&tmp_path)).await.unwrap();

        let speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let hdr = sample_peer_header();
        let peer_key = PeerKey::from_header(speaker, &hdr);
        let now = Utc::now();

        let mut attrs = PathAttributes::default();
        attrs.origin = Some(Origin::Igp);
        attrs.as_path = Some(AsPath(vec![AsPathSegment::Sequence(vec![65002, 65100])]));
        attrs.next_hop = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));

        let pfx1 = Prefix::V4("10.10.1.0/24".parse().unwrap());
        let pfx2 = Prefix::V4("10.10.2.0/24".parse().unwrap());

        let messages = vec![
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::Initiation {
                    sys_name: Some("router-1".to_string()),
                    sys_descr: Some("FRR 9.1".to_string()),
                    labels: vec![],
                },
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::PeerUp(PeerUpMessage {
                    peer_header: hdr.clone(),
                    local_addr: speaker,
                    local_port: 179,
                    remote_port: 50123,
                    sent_open: BgpOpenInfo {
                        version: 4,
                        asn: 65001,
                        hold_time: 90,
                        bgp_id: Ipv4Addr::new(192, 0, 2, 10),
                        capabilities: vec![],
                    },
                    recv_open: BgpOpenInfo {
                        version: 4,
                        asn: 65002,
                        hold_time: 90,
                        bgp_id: Ipv4Addr::new(10, 0, 0, 2),
                        capabilities: vec![],
                    },
                }),
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::RouteMonitoring {
                    peer_header: hdr.clone(),
                    update: BgpUpdate {
                        withdrawn: vec![],
                        withdrawn_path_ids: vec![],
                        attributes: attrs.clone(),
                        announced: vec![pfx1.clone(), pfx2.clone()],
                        announced_path_ids: vec![None, None],
                    },
                },
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::RouteMonitoring {
                    peer_header: hdr.clone(),
                    update: BgpUpdate {
                        withdrawn: vec![pfx1.clone()],
                        withdrawn_path_ids: vec![None],
                        attributes: PathAttributes::default(),
                        announced: vec![],
                        announced_path_ids: vec![],
                    },
                },
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::RouteMonitoring {
                    peer_header: hdr.clone(),
                    update: BgpUpdate {
                        withdrawn: vec![],
                        withdrawn_path_ids: vec![],
                        attributes: PathAttributes::default(),
                        announced: vec![],
                        announced_path_ids: vec![],
                    },
                },
            },
        ];

        let (mut rib_live, _rx_live) = RibManager::new(64);
        rib_live.open_speaker_session(speaker, now);
        for msg in &messages {
            journal.append(msg).await.unwrap();
            rib_live.process(msg.clone());
        }
        drop(journal);

        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&tmp_path).unwrap();
            f.write_all(b"{\"id\":\"truncated-crash-record").unwrap();
            f.flush().unwrap();
        }

        let (mut rib_replayed, _rx_replayed) = RibManager::new(64);
        let report = BmpJournal::replay_into(&tmp_path, &mut rib_replayed).unwrap();
        let _ = std::fs::remove_file(&tmp_path);

        assert_eq!(report.applied_messages, 5);
        assert_eq!(report.skipped_truncated_tail, 1);
        assert_eq!(rib_replayed.total_routes(), rib_live.total_routes());
        assert_eq!(rib_replayed.total_routes(), 1);

        let replayed_rib = rib_replayed.rib_for_peer_key(&peer_key).expect("peer RIB present");
        assert!(replayed_rib.get(RibType::AdjRibInPrePolicy, &pfx1).is_none());
        assert!(replayed_rib.get(RibType::AdjRibInPrePolicy, &pfx2).is_some());

        let replayed_sess = rib_replayed.speaker(speaker).unwrap().peer_by_key(&peer_key).unwrap();
        assert!(replayed_sess.has_eor(RibType::AdjRibInPrePolicy, AfiSafi::ipv4_unicast()));
    }
}

