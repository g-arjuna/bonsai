//! BN-007: operator entry point for B16 routing/config migration and rollback.
//!
//! Usage:
//!   bonsai-migrate backup <file>
//!   bonsai-migrate migrate-jsonl <routing-events.jsonl>
//!   bonsai-migrate migrate-toml <bonsai.toml>
//!   bonsai-migrate rollback <manifest.json>
//!
//! `backup` and the `migrate-*` commands print their report (including the
//! `BackupManifest`) as JSON on stdout. Save the manifest JSON and pass it to
//! `rollback` to deterministically restore the pre-migration file with
//! SHA-256 verification.

use std::path::Path;
use std::process::ExitCode;

use bonsai::routing_service::migration::{
    BackupManifest, create_backup, migrate_bonsai_toml_file, migrate_routing_jsonl_file,
    rollback_from_backup,
};

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        return Err(format!(
            "usage: {} <backup|migrate-jsonl|migrate-toml|rollback> <path>",
            args[0]
        ));
    }
    let command = args[1].as_str();
    let path = Path::new(&args[2]);

    match command {
        "backup" => {
            let manifest = create_backup(path).map_err(|e| e.to_string())?;
            print_json(&manifest)
        }
        "migrate-jsonl" => {
            let report = migrate_routing_jsonl_file(path).map_err(|e| e.to_string())?;
            print_json(&report)
        }
        "migrate-toml" => {
            let report = migrate_bonsai_toml_file(path).map_err(|e| e.to_string())?;
            print_json(&report)
        }
        "rollback" => {
            let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let manifest: BackupManifest =
                serde_json::from_str(&raw).map_err(|e| format!("invalid manifest JSON: {e}"))?;
            let restored_sha = rollback_from_backup(&manifest).map_err(|e| e.to_string())?;
            println!("{{\"restored_sha256\":\"{restored_sha}\"}}");
            Ok(())
        }
        other => Err(format!("unknown command: {other}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bonsai-migrate: {e}");
            ExitCode::FAILURE
        }
    }
}
