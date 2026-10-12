//! B16: Versioned configuration and routing event journal migration with
//! automatic pre-migration backup, SHA-256 checksum verification, explicit
//! `legacy_identity_incomplete` quality flags, and deterministic rollback.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::platform_registry::lookup_platform;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    pub source_path: PathBuf,
    pub backup_path: PathBuf,
    pub source_sha256: String,
    pub backup_sha256: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub total_records_read: usize,
    pub migrated_v1_to_v2: usize,
    pub already_v2_records: usize,
    pub legacy_identity_incomplete_count: usize,
    pub corrupt_lines_skipped: usize,
    pub backup: BackupManifest,
    pub output_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMigrationReport {
    pub migrated_vendor_aliases: Vec<(String, String)>,
    pub unsupported_vendors: Vec<String>,
    pub added_bmp_authority: bool,
    pub backup: BackupManifest,
    pub output_sha256: String,
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Create a verified `.pre-m3.bak` backup of `source_path` before any migration mutates it.
pub fn create_backup(source_path: &Path) -> anyhow::Result<BackupManifest> {
    if !source_path.exists() {
        anyhow::bail!("Cannot back up non-existent file: {}", source_path.display());
    }
    let source_sha256 = sha256_file(source_path)?;
    let file_name = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    let backup_path = source_path.with_file_name(format!("{file_name}.pre-m3.bak"));
    fs::copy(source_path, &backup_path)?;
    let backup_sha256 = sha256_file(&backup_path)?;
    if source_sha256 != backup_sha256 {
        anyhow::bail!(
            "Backup checksum mismatch for {}: {} != {}",
            source_path.display(),
            source_sha256,
            backup_sha256
        );
    }
    Ok(BackupManifest {
        source_path: source_path.to_path_buf(),
        backup_path,
        source_sha256,
        backup_sha256,
        created_at: Utc::now(),
    })
}

/// Deterministically restore `manifest.source_path` from `manifest.backup_path` and verify SHA-256 equality.
pub fn rollback_from_backup(manifest: &BackupManifest) -> anyhow::Result<String> {
    if !manifest.backup_path.exists() {
        anyhow::bail!(
            "Backup file missing during rollback: {}",
            manifest.backup_path.display()
        );
    }
    let current_bak_sha = sha256_file(&manifest.backup_path)?;
    if current_bak_sha != manifest.backup_sha256 {
        anyhow::bail!(
            "Backup file corrupted: expected {}, got {}",
            manifest.backup_sha256,
            current_bak_sha
        );
    }
    fs::copy(&manifest.backup_path, &manifest.source_path)?;
    let restored_sha = sha256_file(&manifest.source_path)?;
    if restored_sha != manifest.source_sha256 {
        anyhow::bail!(
            "Rollback verification failed: expected {}, got {}",
            manifest.source_sha256,
            restored_sha
        );
    }
    Ok(restored_sha)
}

/// Migrate a legacy routing event JSONL file (v1 -> v2), preserving a verified backup
/// and explicitly flagging records that lacked VRF distinguisher, view, or session epoch identity.
pub fn migrate_routing_jsonl_file(path: &Path) -> anyhow::Result<MigrationReport> {
    let backup = create_backup(path)?;
    let raw = fs::read_to_string(path)?;

    let mut total_records_read = 0usize;
    let mut migrated_v1_to_v2 = 0usize;
    let mut already_v2_records = 0usize;
    let mut legacy_identity_incomplete_count = 0usize;
    let mut corrupt_lines_skipped = 0usize;
    let mut out_lines = Vec::new();

    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        total_records_read += 1;
        let Ok(mut val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            corrupt_lines_skipped += 1;
            continue;
        };
        let Some(obj) = val.as_object_mut() else {
            corrupt_lines_skipped += 1;
            continue;
        };

        let schema_ver = obj
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);

        if schema_ver >= 2 {
            already_v2_records += 1;
            out_lines.push(serde_json::to_string(&val)?);
            continue;
        }

        // Migrate v1 -> v2
        migrated_v1_to_v2 += 1;
        obj.insert("schema_version".to_string(), serde_json::json!(2));

        let has_event_key = obj.get("event_key").and_then(|v| v.as_object()).is_some();
        let has_rd = obj
            .get("payload")
            .and_then(|p| p.get("route_distinguisher"))
            .is_some();
        let has_epoch = obj
            .get("event_key")
            .and_then(|ek| ek.get("source"))
            .and_then(|s| s.get("session_epoch"))
            .is_some();

        if !has_event_key || !has_rd || !has_epoch {
            legacy_identity_incomplete_count += 1;
            obj.insert(
                "legacy_identity_incomplete".to_string(),
                serde_json::json!(true),
            );
            obj.insert(
                "quality_flags".to_string(),
                serde_json::json!(["legacy_missing_identity"]),
            );

            let speaker_str = obj
                .get("speaker")
                .and_then(|v| v.as_str())
                .unwrap_or("0.0.0.0")
                .to_string();
            if !has_event_key {
                obj.insert(
                    "event_key".to_string(),
                    serde_json::json!({
                        "source": {
                            "collector_id": "legacy-migrated",
                            "exporter_id": speaker_str,
                            "exporter_addr": speaker_str,
                            "exporter_instance": "legacy-default",
                            "session_epoch": 0
                        },
                        "sequence": (idx as u64) + 1
                    }),
                );
            }
        } else {
            obj.insert(
                "legacy_identity_incomplete".to_string(),
                serde_json::json!(false),
            );
        }

        out_lines.push(serde_json::to_string(&val)?);
    }

    let mut final_content = out_lines.join("\n");
    if !final_content.is_empty() {
        final_content.push('\n');
    }
    fs::write(path, final_content.as_bytes())?;
    let output_sha256 = sha256_file(path)?;

    Ok(MigrationReport {
        total_records_read,
        migrated_v1_to_v2,
        already_v2_records,
        legacy_identity_incomplete_count,
        corrupt_lines_skipped,
        backup,
        output_sha256,
    })
}

/// Migrate a legacy `bonsai.toml` configuration file:
/// - Creates a verified backup first,
/// - Normalizes legacy vendor aliases (`srl` -> `nokia.srlinux`, `xrd` -> `cisco.iosxr`, etc.) via `platform_registry`,
/// - Records any unknown/unsupported vendor string without silent Cisco fallback,
/// - Adds `[bmp] authority = "routing_service"` if absent.
pub fn migrate_bonsai_toml_file(path: &Path) -> anyhow::Result<ConfigMigrationReport> {
    let backup = create_backup(path)?;
    let raw = fs::read_to_string(path)?;
    let mut migrated_vendor_aliases = Vec::new();
    let mut unsupported_vendors = Vec::new();
    let mut out_lines = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("vendor") && trimmed.contains('=') && !trimmed.starts_with('#') {
            if let Some((lhs, rhs)) = line.split_once('=') {
                let raw_val = rhs.trim().trim_matches('"').trim_matches('\'');
                let profile = lookup_platform(raw_val);
                if profile.platform_id != "unknown" {
                    if profile.platform_id != raw_val {
                        migrated_vendor_aliases
                            .push((raw_val.to_string(), profile.platform_id.to_string()));
                    }
                    out_lines.push(format!("{lhs}= \"{}\"", profile.platform_id));
                    continue;
                } else {
                    unsupported_vendors.push(raw_val.to_string());
                }
            }
        }
        out_lines.push(line.to_string());
    }

    let mut joined = out_lines.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }

    let mut added_bmp_authority = false;
    if !joined.contains("[bmp]") {
        joined.push_str(
            "\n[bmp]\nenabled = false\nauthority = \"routing_service\"\nrouting_service_url = \"http://127.0.0.1:8081\"\n",
        );
        added_bmp_authority = true;
    }

    fs::write(path, joined.as_bytes())?;
    let output_sha256 = sha256_file(path)?;

    Ok(ConfigMigrationReport {
        migrated_vendor_aliases,
        unsupported_vendors,
        added_bmp_authority,
        backup,
        output_sha256,
    })
}
