use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Cursor, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;
use tar::{Builder, Header};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const AUDIT_DIR: &str = "audit";
const FILE_PREFIX: &str = "audit-";
const FILE_SUFFIX: &str = ".jsonl";
const RETENTION_DAYS_DEFAULT: i64 = 30;
const NS_PER_DAY: i64 = 86_400_000_000_000;

#[derive(Debug, Clone)]
pub struct AuditExportResult {
    pub output_path: PathBuf,
    pub entry_count: usize,
}

pub fn append_credential_resolve(
    root: &Path,
    timestamp_ns: i64,
    alias: &str,
    purpose: &str,
    outcome: &str,
    error: Option<&str>,
) -> Result<()> {
    let dir = audit_dir(root);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create audit directory '{}'", dir.display()))?;
    let file_path = dir.join(format!("{FILE_PREFIX}{}{FILE_SUFFIX}", epoch_day(timestamp_ns)));

    let mut event = json!({
        "timestamp_ns": timestamp_ns,
        "alias": alias,
        "purpose": purpose,
        "outcome": outcome,
    });
    if let Some(error) = error {
        event["error"] = serde_json::Value::String(error.to_string());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .with_context(|| format!("failed to open audit log '{}'", file_path.display()))?;
    writeln!(file, "{event}").with_context(|| {
        format!(
            "failed to append credential audit event to '{}'",
            file_path.display()
        )
    })?;

    enforce_retention(root, RETENTION_DAYS_DEFAULT)?;
    Ok(())
}

/// Append a structured enrichment run event to the audit log.
pub fn append_enrichment_run(
    root: &Path,
    timestamp_ns: i64,
    enricher_name: &str,
    outcome: &str,
    nodes_touched: usize,
    error: Option<&str>,
) -> Result<()> {
    let dir = audit_dir(root);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create audit directory '{}'", dir.display()))?;
    let file_path = dir.join(format!("{FILE_PREFIX}{}{FILE_SUFFIX}", epoch_day(timestamp_ns)));

    let mut event = json!({
        "timestamp_ns": timestamp_ns,
        "event": "enrichment_run",
        "enricher": enricher_name,
        "outcome": outcome,
        "nodes_touched": nodes_touched,
    });
    if let Some(error) = error {
        event["error"] = serde_json::Value::String(error.to_string());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .with_context(|| format!("failed to open audit log '{}'", file_path.display()))?;
    writeln!(file, "{event}").with_context(|| {
        format!("failed to append enrichment run audit event to '{}'", file_path.display())
    })?;

    enforce_retention(root, RETENTION_DAYS_DEFAULT)?;
    Ok(())
}

/// Append a trust-state-affecting operator decision to the audit log.
pub fn append_trust_operation(
    root: &Path,
    timestamp_ns: i64,
    trust_key: &str,
    operation: &str,   // "approve" | "reject" | "graduate" | "rollback" | "set_state"
    proposal_id: &str,
    operator_note: Option<&str>,
) -> Result<()> {
    let dir = audit_dir(root);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create audit directory '{}'", dir.display()))?;
    let file_path = dir.join(format!("{FILE_PREFIX}{}{FILE_SUFFIX}", epoch_day(timestamp_ns)));

    let mut event = json!({
        "timestamp_ns": timestamp_ns,
        "event": "trust_op",
        "trust_key": trust_key,
        "operation": operation,
        "proposal_id": proposal_id,
    });
    if let Some(note) = operator_note {
        event["operator_note"] = serde_json::Value::String(note.to_string());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .with_context(|| format!("failed to open audit log '{}'", file_path.display()))?;
    writeln!(file, "{event}").with_context(|| {
        format!("failed to append trust_op audit event to '{}'", file_path.display())
    })?;

    enforce_retention(root, RETENTION_DAYS_DEFAULT)?;
    Ok(())
}

/// Append an output-adapter push event to the audit log.
pub fn append_adapter_push(
    root: &Path,
    timestamp_ns: i64,
    adapter_name: &str,
    outcome: &str,
    events_pushed: usize,
    bytes_sent: u64,
    error: Option<&str>,
) -> Result<()> {
    let dir = audit_dir(root);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create audit directory '{}'", dir.display()))?;
    let file_path = dir.join(format!("{FILE_PREFIX}{}{FILE_SUFFIX}", epoch_day(timestamp_ns)));

    let mut event = json!({
        "timestamp_ns": timestamp_ns,
        "event": "adapter_push",
        "adapter": adapter_name,
        "outcome": outcome,
        "events_pushed": events_pushed,
        "bytes_sent": bytes_sent,
    });
    if let Some(error) = error {
        event["error"] = serde_json::Value::String(error.to_string());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .with_context(|| format!("failed to open audit log '{}'", file_path.display()))?;
    writeln!(file, "{event}").with_context(|| {
        format!("failed to append adapter push audit event to '{}'", file_path.display())
    })?;

    enforce_retention(root, RETENTION_DAYS_DEFAULT)?;
    Ok(())
}

pub fn enforce_retention(root: &Path, retention_days: i64) -> Result<usize> {
    let dir = audit_dir(root);
    if !dir.exists() {
        return Ok(0);
    }
    if retention_days <= 0 {
        bail!("retention_days must be positive");
    }
    let cutoff_day = epoch_day(now_ns()) - retention_days;
    let mut deleted = 0usize;
    for entry in
        fs::read_dir(&dir).with_context(|| format!("failed to read '{}'", dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in '{}'", dir.display()))?;
        let path = entry.path();
        let Some(day) = parse_epoch_day_from_filename(&path) else {
            continue;
        };
        if day < cutoff_day {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove old audit file '{}'", path.display()))?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

pub fn export_tarball(
    root: &Path,
    since_iso: &str,
    until_iso: &str,
    output_path: &Path,
) -> Result<AuditExportResult> {
    let since_ns = parse_iso_to_ns(since_iso)?;
    let until_ns = parse_iso_to_ns(until_iso)?;
    if until_ns < since_ns {
        bail!("--until must be greater than or equal to --since");
    }

    let dir = audit_dir(root);
    if !dir.exists() {
        bail!("audit log directory '{}' does not exist", dir.display());
    }

    let mut files = fs::read_dir(&dir)
        .with_context(|| format!("failed to read '{}'", dir.display()))?
        .filter_map(|entry| entry.ok().map(|value| value.path()))
        .filter(|path| parse_epoch_day_from_filename(path).is_some())
        .collect::<Vec<_>>();
    files.sort();

    let mut filtered_lines = Vec::new();
    for file in files {
        let fd = fs::File::open(&file)
            .with_context(|| format!("failed to open audit file '{}'", file.display()))?;
        let reader = BufReader::new(fd);
        for line in reader.lines() {
            let line = line.with_context(|| format!("failed to read line from '{}'", file.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(&line)
                .with_context(|| format!("failed to parse JSON line from '{}'", file.display()))?;
            let Some(ts) = value.get("timestamp_ns").and_then(|value| value.as_i64()) else {
                continue;
            };
            if ts >= since_ns && ts <= until_ns {
                filtered_lines.push(line);
            }
        }
    }

    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory for '{}'",
                output_path.display()
            )
        })?;
    }

    let output = fs::File::create(output_path)
        .with_context(|| format!("failed to create '{}'", output_path.display()))?;
    let mut builder = Builder::new(output);

    let events_payload = if filtered_lines.is_empty() {
        Vec::new()
    } else {
        format!("{}\n", filtered_lines.join("\n")).into_bytes()
    };
    append_bytes(&mut builder, "audit/events.jsonl", &events_payload)?;

    let manifest = json!({
        "since_iso": since_iso,
        "until_iso": until_iso,
        "since_ns": since_ns,
        "until_ns": until_ns,
        "exported_at_ns": now_ns(),
        "entry_count": filtered_lines.len(),
    });
    let manifest_payload = serde_json::to_vec_pretty(&manifest).context("serialize audit manifest")?;
    append_bytes(&mut builder, "audit/manifest.json", &manifest_payload)?;

    builder.finish().context("failed to finalize audit export tarball")?;

    Ok(AuditExportResult {
        output_path: output_path.to_path_buf(),
        entry_count: filtered_lines.len(),
    })
}

pub fn parse_iso_to_ns(value: &str) -> Result<i64> {
    let parsed = OffsetDateTime::parse(value.trim(), &Rfc3339)
        .with_context(|| format!("invalid RFC3339 timestamp '{value}'"))?;
    parsed
        .unix_timestamp_nanos()
        .try_into()
        .map_err(|_| anyhow::anyhow!("timestamp out of range for i64: '{value}'"))
}

fn append_bytes(builder: &mut Builder<fs::File>, path: &str, payload: &[u8]) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_size(payload.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(payload))
        .with_context(|| format!("failed to append '{path}' to tarball"))?;
    Ok(())
}

fn audit_dir(root: &Path) -> PathBuf {
    root.join(AUDIT_DIR)
}

fn parse_epoch_day_from_filename(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    if !name.starts_with(FILE_PREFIX) || !name.ends_with(FILE_SUFFIX) {
        return None;
    }
    let day = name
        .trim_start_matches(FILE_PREFIX)
        .trim_end_matches(FILE_SUFFIX);
    day.parse().ok()
}

fn epoch_day(timestamp_ns: i64) -> i64 {
    timestamp_ns.div_euclid(NS_PER_DAY)
}

fn now_ns() -> i64 {
    OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .try_into()
        .unwrap_or(i64::MAX)
}

