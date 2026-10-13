use anyhow::{Context, Result, anyhow};
use rbmp_core::bgp::types::BgpDecodeContext;
use rbmp_core::bmp::parser::{
    DEFAULT_MAX_FRAME, parse_bmp_message_with_context, parse_peer_header,
};
use rbmp_core::bmp::types::{BmpMessage, BmpPayload, PeerKey};
use rbmp_rib::RibManager;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Summary returned after replaying an append-only BMP journal into a `RibManager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayReport {
    pub applied_messages: usize,
    pub skipped_truncated_tail: usize,
}

/// Durable transport-disconnect record persisted in `BmpJournal` across restarts (R04).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct JournalTransportGap {
    pub speaker_addr: IpAddr,
    #[serde(default)]
    pub session_epoch: Option<u64>,
    pub disconnected_at: chrono::DateTime<chrono::Utc>,
    pub reason: String,
}

/// Rich replay outcome returning the report, all deterministically emitted `RibEvent`s,
/// active `JournalTransportGap`s, and reconstructed `BgpDecodeContext`s (R02, R04).
#[derive(Debug)]
pub struct ReplayOutcome {
    pub report: ReplayReport,
    pub emitted_events: Vec<rbmp_rib::event::RibEvent>,
    pub transport_gaps: HashMap<IpAddr, JournalTransportGap>,
    pub decode_contexts: HashMap<PeerKey, BgpDecodeContext>,
}

/// Observation entry serialized to `BmpJournal` preserving `session_epoch` and optional raw wire bytes (R02).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BmpJournalObservation {
    #[serde(flatten)]
    pub message: BmpMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_frame_hex: Option<String>,
}

/// Control / lifecycle record serialized to `BmpJournal` for transport disconnects (R04).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "record_kind", rename_all = "snake_case")]
pub enum BmpJournalControlRecord {
    TransportDisconnect {
        speaker_addr: IpAddr,
        #[serde(default)]
        session_epoch: Option<u64>,
        disconnected_at: chrono::DateTime<chrono::Utc>,
        reason: String,
    },
}

enum ParsedBmpJournalLine {
    Observation(BmpJournalObservation),
    Control(BmpJournalControlRecord),
}

fn parse_bmp_journal_line(
    line: &str,
) -> std::result::Result<ParsedBmpJournalLine, serde_json::Error> {
    if line.contains("\"record_kind\"") {
        if let Ok(ctrl) = serde_json::from_str::<BmpJournalControlRecord>(line) {
            return Ok(ParsedBmpJournalLine::Control(ctrl));
        }
    }
    serde_json::from_str::<BmpJournalObservation>(line).map(ParsedBmpJournalLine::Observation)
}

/// Determine whether an incoming BMP message for a speaker clears an active transport gap (R04).
///
/// A transport gap is cleared only when:
/// 1. The speaker session is explicitly terminated (`BmpPayload::Termination`) in an epoch >= `gap_epoch`, or
/// 2. An explicit `EndOfRib` marker is received in an epoch >= `gap_epoch`, or
/// 3. A new session synchronization (`Initiation` or `PeerUp`) arrives on a newer session epoch (`incoming_epoch > gap_epoch`).
pub fn should_clear_transport_gap(
    gap_epoch: Option<u64>,
    incoming_epoch: Option<u64>,
    payload: &BmpPayload,
) -> bool {
    let is_not_stale_epoch = match (gap_epoch, incoming_epoch) {
        (Some(g), Some(inc)) => inc >= g,
        _ => true,
    };
    if !is_not_stale_epoch {
        return false;
    }
    match payload {
        BmpPayload::Termination { .. } => true,
        BmpPayload::RouteMonitoring { update, .. } if update.eor_afi_safi().is_some() => true,
        BmpPayload::Initiation { .. } | BmpPayload::PeerUp(_) => {
            match (gap_epoch, incoming_epoch) {
                (Some(g), Some(inc)) => inc > g,
                (None, Some(_)) => true,
                _ => false,
            }
        }
        _ => false,
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn hex_to_bytes(hex_str: &str) -> Option<Vec<u8>> {
    let s = hex_str.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks_exact(2) {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

fn truncate_file_to_len(path: &Path, valid_len: usize) -> Result<()> {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {} for tail truncation", path.display()))?;
    f.set_len(valid_len as u64)
        .with_context(|| format!("failed to truncate torn tail on {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("failed to sync truncated journal {}", path.display()))?;
    Ok(())
}

fn append_newline_and_sync(path: &Path) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {} to terminate final line", path.display()))?;
    f.write_all(b"\n")?;
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

/// Durable append-only JSONL journal of raw `BmpMessage` records and transport lifecycle events for crash recovery.
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
                if pb.exists() {
                    let (mut dummy_rib, _rx) = RibManager::new(16);
                    let _ = Self::replay_into_with_state(&pb, &mut dummy_rib)?;
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
        self.append_observation(msg, None, None).await
    }

    /// Serialize `msg` together with optional `session_epoch` and raw wire frame bytes, flush, and fsync.
    pub async fn append_observation(
        &self,
        msg: &BmpMessage,
        session_epoch: Option<u64>,
        raw_frame: Option<&[u8]>,
    ) -> Result<()> {
        if let Some(file) = &self.file {
            let record = BmpJournalObservation {
                message: msg.clone(),
                session_epoch,
                raw_frame_hex: raw_frame.map(bytes_to_hex),
            };
            let mut line = serde_json::to_vec(&record)?;
            line.push(b'\n');
            let mut guard = file.lock().await;
            guard.write_all(&line).await?;
            guard.flush().await?;
            guard.sync_data().await?;
        }
        Ok(())
    }

    /// Durably record a BMP transport disconnect so monitoring gaps survive restarts (R04).
    pub async fn append_transport_disconnect(&self, gap: &JournalTransportGap) -> Result<()> {
        if let Some(file) = &self.file {
            let record = BmpJournalControlRecord::TransportDisconnect {
                speaker_addr: gap.speaker_addr,
                session_epoch: gap.session_epoch,
                disconnected_at: gap.disconnected_at,
                reason: gap.reason.clone(),
            };
            let mut line = serde_json::to_vec(&record)?;
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
    /// it is skipped, truncated on disk (`set_len`), and counted in `ReplayReport::skipped_truncated_tail`.
    /// Any corruption in a non-final line returns an `Err`.
    pub fn replay_into(path: impl AsRef<Path>, rib: &mut RibManager) -> Result<ReplayReport> {
        Ok(Self::replay_into_with_state(path, rib)?.report)
    }

    /// Replay a journal file into `rib`, returning all emitted `RibEvent`s, active `JournalTransportGap`s,
    /// and `BgpDecodeContext`s while repairing any torn trailing bytes on disk (R02, R03, R04).
    pub fn replay_into_with_state(
        path: impl AsRef<Path>,
        rib: &mut RibManager,
    ) -> Result<ReplayOutcome> {
        let path_ref = path.as_ref();
        let raw_bytes = std::fs::read(path_ref)
            .with_context(|| format!("failed to read BMP journal {}", path_ref.display()))?;
        if raw_bytes.is_empty() {
            return Ok(ReplayOutcome {
                report: ReplayReport {
                    applied_messages: 0,
                    skipped_truncated_tail: 0,
                },
                emitted_events: Vec::new(),
                transport_gaps: HashMap::new(),
                decode_contexts: HashMap::new(),
            });
        }

        let mut applied_messages = 0usize;
        let mut skipped_truncated_tail = 0usize;
        let mut emitted_events = Vec::new();
        let mut transport_gaps: HashMap<IpAddr, JournalTransportGap> = HashMap::new();
        let mut decode_contexts: HashMap<PeerKey, BgpDecodeContext> = HashMap::new();

        let mut cursor = 0usize;
        let mut valid_len = 0usize;
        let mut line_idx = 0usize;

        while cursor < raw_bytes.len() {
            let next_nl = raw_bytes[cursor..].iter().position(|&b| b == b'\n');
            match next_nl {
                Some(rel_end) => {
                    let line_end = cursor + rel_end;
                    let line_bytes = &raw_bytes[cursor..line_end];
                    let line_str = std::str::from_utf8(line_bytes).map_err(|e| {
                        anyhow!(
                            "corrupted UTF-8 in BMP journal at line {}: {}",
                            line_idx + 1,
                            e
                        )
                    })?;
                    let trimmed = line_str.trim();
                    if !trimmed.is_empty() {
                        let parsed = parse_bmp_journal_line(trimmed).map_err(|e| {
                            anyhow!(
                                "corrupted BMP journal record at line {}: {}",
                                line_idx + 1,
                                e
                            )
                        })?;
                        Self::apply_parsed_line(
                            parsed,
                            rib,
                            &mut applied_messages,
                            &mut emitted_events,
                            &mut transport_gaps,
                            &mut decode_contexts,
                        );
                    }
                    valid_len = line_end + 1;
                    cursor = line_end + 1;
                    line_idx += 1;
                }
                None => {
                    let tail_bytes = &raw_bytes[cursor..];
                    match std::str::from_utf8(tail_bytes) {
                        Ok(tail_str) if tail_str.trim().is_empty() => {
                            truncate_file_to_len(path_ref, valid_len)?;
                        }
                        Ok(tail_str) => match parse_bmp_journal_line(tail_str.trim()) {
                            Ok(parsed) => {
                                append_newline_and_sync(path_ref)?;
                                Self::apply_parsed_line(
                                    parsed,
                                    rib,
                                    &mut applied_messages,
                                    &mut emitted_events,
                                    &mut transport_gaps,
                                    &mut decode_contexts,
                                );
                            }
                            Err(_) => {
                                skipped_truncated_tail += 1;
                                truncate_file_to_len(path_ref, valid_len)?;
                            }
                        },
                        Err(_) => {
                            skipped_truncated_tail += 1;
                            truncate_file_to_len(path_ref, valid_len)?;
                        }
                    }
                    break;
                }
            }
        }

        Ok(ReplayOutcome {
            report: ReplayReport {
                applied_messages,
                skipped_truncated_tail,
            },
            emitted_events,
            transport_gaps,
            decode_contexts,
        })
    }

    fn apply_parsed_line(
        parsed: ParsedBmpJournalLine,
        rib: &mut RibManager,
        applied_messages: &mut usize,
        emitted_events: &mut Vec<rbmp_rib::event::RibEvent>,
        transport_gaps: &mut HashMap<IpAddr, JournalTransportGap>,
        decode_contexts: &mut HashMap<PeerKey, BgpDecodeContext>,
    ) {
        match parsed {
            ParsedBmpJournalLine::Observation(obs) => {
                let speaker = obs.message.speaker_addr;
                let mut msg = obs.message;

                // If raw wire bytes were captured, re-verify/decode with session-aware context when possible
                if let Some(ref hex_str) = obs.raw_frame_hex {
                    if let Some(frame) = hex_to_bytes(hex_str) {
                        let ctx = if frame.len() >= 48 && frame[0] == 3 && frame[5] == 0 {
                            if let Ok((peer_hdr, _)) = parse_peer_header(&frame[6..]) {
                                let pk = PeerKey::from_header(speaker, &peer_hdr);
                                decode_contexts.get(&pk).cloned().unwrap_or_else(|| {
                                    BgpDecodeContext::new(peer_hdr.peer_flags.four_byte_asn())
                                })
                            } else {
                                BgpDecodeContext::default()
                            }
                        } else {
                            BgpDecodeContext::default()
                        };
                        if let Ok(redecoded_payload) =
                            parse_bmp_message_with_context(&frame, speaker, DEFAULT_MAX_FRAME, &ctx)
                        {
                            msg.payload = redecoded_payload;
                        }
                    }
                }

                if let BmpPayload::PeerUp(ref pu) = msg.payload {
                    let peer_key = PeerKey::from_header(speaker, &pu.peer_header);
                    let ctx = BgpDecodeContext::from_negotiated_open(
                        pu.peer_header.peer_flags.four_byte_asn(),
                        &pu.sent_open.capabilities,
                        &pu.recv_open.capabilities,
                    );
                    decode_contexts.insert(peer_key, ctx);
                }

                let effective_epoch = obs
                    .session_epoch
                    .or_else(|| rib.speaker(speaker).map(|s| s.source.session_epoch));

                if let Some(existing_gap) = transport_gaps.get(&speaker) {
                    if should_clear_transport_gap(
                        existing_gap.session_epoch,
                        effective_epoch,
                        &msg.payload,
                    ) {
                        transport_gaps.remove(&speaker);
                    }
                }

                let events = rib.process_for_epoch(msg, obs.session_epoch);
                emitted_events.extend(events);
                *applied_messages += 1;
            }
            ParsedBmpJournalLine::Control(BmpJournalControlRecord::TransportDisconnect {
                speaker_addr,
                session_epoch,
                disconnected_at,
                reason,
            }) => {
                let active_epoch = rib.speaker(speaker_addr).map(|s| s.source.session_epoch);
                if let (Some(disc_ep), Some(act_ep)) = (session_epoch, active_epoch) {
                    if disc_ep < act_ep {
                        return;
                    }
                }
                let effective_epoch = session_epoch.or(active_epoch);
                rib.clear_speaker_eor(speaker_addr);
                transport_gaps.insert(
                    speaker_addr,
                    JournalTransportGap {
                        speaker_addr,
                        session_epoch: effective_epoch,
                        disconnected_at,
                        reason,
                    },
                );
            }
        }
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
    #[serde(default)]
    pub peer_distinguisher: String,
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
                    let raw_bytes = tokio::fs::read(&pb).await.unwrap_or_default();
                    let mut cursor = 0usize;
                    let mut valid_len = 0usize;
                    let mut line_idx = 0usize;

                    while cursor < raw_bytes.len() {
                        let next_nl = raw_bytes[cursor..].iter().position(|&b| b == b'\n');
                        match next_nl {
                            Some(rel_end) => {
                                let line_end = cursor + rel_end;
                                let line_bytes = &raw_bytes[cursor..line_end];
                                let line_str = std::str::from_utf8(line_bytes).map_err(|e| {
                                    anyhow!(
                                        "corrupted UTF-8 in RibEventJournal at line {}: {}",
                                        line_idx + 1,
                                        e
                                    )
                                })?;
                                let trimmed = line_str.trim();
                                if !trimmed.is_empty() {
                                    let ev = serde_json::from_str::<rbmp_rib::event::RibEvent>(
                                        trimmed,
                                    )
                                    .map_err(|e| {
                                        anyhow!(
                                            "corrupted RibEventJournal record at line {}: {}",
                                            line_idx + 1,
                                            e
                                        )
                                    })?;
                                    initial_events.push(ev);
                                }
                                valid_len = line_end + 1;
                                cursor = line_end + 1;
                                line_idx += 1;
                            }
                            None => {
                                let tail_bytes = &raw_bytes[cursor..];
                                match std::str::from_utf8(tail_bytes) {
                                    Ok(tail_str) if tail_str.trim().is_empty() => {
                                        truncate_file_to_len(&pb, valid_len)?;
                                    }
                                    Ok(tail_str) => {
                                        match serde_json::from_str::<rbmp_rib::event::RibEvent>(
                                            tail_str.trim(),
                                        ) {
                                            Ok(ev) => {
                                                append_newline_and_sync(&pb)?;
                                                initial_events.push(ev);
                                            }
                                            Err(_) => {
                                                // Truncate incomplete torn crash tail on disk before opening in append mode (R03)
                                                truncate_file_to_len(&pb, valid_len)?;
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        truncate_file_to_len(&pb, valid_len)?;
                                    }
                                }
                                break;
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
        self.append_batch(std::slice::from_ref(ev)).await
    }

    /// Persist a batch of `RibEvent`s in a single atomic buffered write + `sync_data()` (R01).
    pub async fn append_batch(&self, events: &[rbmp_rib::event::RibEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        if let Some(file) = &self.file {
            let mut buf = Vec::with_capacity(events.len() * 256);
            for ev in events {
                serde_json::to_writer(&mut buf, ev)?;
                buf.push(b'\n');
            }
            let mut guard = file.lock().await;
            guard.write_all(&buf).await?;
            guard.flush().await?;
            guard.sync_data().await?;
        }
        let mut guard = self.events.lock().await;
        guard.extend_from_slice(events);
        metrics::counter!("rib_events_written_total").increment(events.len() as u64);
        Ok(())
    }

    pub async fn existing_sequences(&self) -> HashSet<u64> {
        let guard = self.events.lock().await;
        guard.iter().map(|e| e.event_key.sequence).collect()
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
        self.prefix_history_filtered(prefix, None, None, limit)
            .await
    }

    pub async fn prefix_history_filtered(
        &self,
        prefix: &str,
        speaker_filter: Option<&str>,
        vrf_rd_filter: Option<&str>,
        limit: usize,
    ) -> Vec<PrefixHistoryItem> {
        use rbmp_rib::event::{RibEventPayload, RouteAction};

        let guard = self.events.lock().await;
        let mut last_attrs_by_route: HashMap<
            rbmp_core::bmp::types::RouteKey,
            Option<rbmp_core::bgp::types::PathAttributes>,
        > = HashMap::new();
        let mut items = Vec::new();

        for ev in guard.iter() {
            if let RibEventPayload::RouteChange(rc) = &ev.payload {
                let pfx_str = rc.prefix.to_string();
                let prev = last_attrs_by_route.get(&rc.route_key).cloned().flatten();
                let next = match rc.action {
                    RouteAction::Announce => rc.attributes.clone(),
                    RouteAction::Withdraw => None,
                };
                last_attrs_by_route.insert(rc.route_key.clone(), next.clone());

                if pfx_str != prefix {
                    continue;
                }
                if let Some(spk) = speaker_filter {
                    if !spk.trim().is_empty() && ev.speaker.to_string() != spk.trim() {
                        continue;
                    }
                }
                let rd_str = rc.peer_key.distinguisher_string();
                if let Some(rd) = vrf_rd_filter {
                    if !rd.trim().is_empty() && rd_str != rd.trim() {
                        continue;
                    }
                }

                items.push(PrefixHistoryItem {
                    sequence: ev.event_key.sequence,
                    event_id: ev.id,
                    occurred_at: ev.occurred_at,
                    speaker_addr: ev.speaker,
                    peer_addr: rc.peer_header.peer_address,
                    peer_as: rc.peer_header.peer_as,
                    peer_distinguisher: rd_str,
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
    use rbmp_core::bgp::types::{
        AfiSafi, AsPath, AsPathSegment, BgpUpdate, Origin, PathAttributes, Prefix,
    };
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
    async fn b06_durable_journal_replay_reconstructs_identical_rib_state_after_truncated_crash_tail()
     {
        let tmp_path =
            std::env::temp_dir().join(format!("rbmp-b06-journal-{}.jsonl", Uuid::new_v4()));
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
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&tmp_path)
                .unwrap();
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

        let replayed_rib = rib_replayed
            .rib_for_peer_key(&peer_key)
            .expect("peer RIB present");
        assert!(
            replayed_rib
                .get(RibType::AdjRibInPrePolicy, &pfx1)
                .is_none()
        );
        assert!(
            replayed_rib
                .get(RibType::AdjRibInPrePolicy, &pfx2)
                .is_some()
        );

        let replayed_sess = rib_replayed
            .speaker(speaker)
            .unwrap()
            .peer_by_key(&peer_key)
            .unwrap();
        assert!(replayed_sess.has_eor(RibType::AdjRibInPrePolicy, AfiSafi::ipv4_unicast()));
    }
}
