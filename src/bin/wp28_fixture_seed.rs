//! WP2.8 disposable-recovery fixture seeder (NOT for production use).
//!
//! Opens a graph database at a caller-supplied path and writes legacy
//! credential-bearing `ConfigItem` records through
//! `GraphStore::upsert_config_item_unchecked` — the same fixture-only
//! path used by the BN-022 migration tests — so a subsequent server
//! start exercises the real `AuthStore::ensure_migrated` import /
//! quarantine / tombstone-scrub pipeline end to end.
//!
//! Usage: wp28_fixture_seed <graph.db path>
//! The server MUST be stopped while this runs (single-writer DB).

use anyhow::Result;
use bonsai::graph::{ConfigItemRecord, GraphStore};
use sha2::{Digest, Sha256};

fn item(id: &str, class: &str, name: &str, content: &str, enabled: bool) -> ConfigItemRecord {
    ConfigItemRecord {
        id: id.into(),
        config_class: class.into(),
        vendor: String::new(),
        name: name.into(),
        version: "1".into(),
        content_json: content.into(),
        enabled,
        created_by: "wp28-fixture".into(),
    }
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: wp28_fixture_seed <graph.db path>"))?;
    let store = GraphStore::open(&path, 512 * 1024 * 1024)?;

    // Legacy user (SHA-256 verifier — the pre-Argon2 format the migration
    // is required to accept and upgrade on next login).
    store
        .upsert_config_item_unchecked(item(
            "cfg-user-legacy-viewer",
            "user",
            "legacy-viewer",
            &format!(
                "{{\"password_hash\":\"{}\",\"role\":\"viewer\"}}",
                sha256_hex("LegacyViewerPassw0rd!")
            ),
            true,
        ))
        .await?;

    // Malformed user record → must be sealed into quarantine, never imported.
    store
        .upsert_config_item_unchecked(item(
            "cfg-user-malformed",
            "user",
            "",
            "{\"password_hash\":\"\"}",
            true,
        ))
        .await?;

    // Legacy API key. Raw key chosen so sha256(raw) == key_hash.
    let raw_key = "bsk_legacy_fixture_key_0123456789abcdef";
    store
        .upsert_config_item_unchecked(item(
            "cfg-key-legacy-read",
            "api_key",
            "legacy-read-key",
            &format!(
                "{{\"key_hash\":\"{}\",\"scope\":\"read\"}}",
                sha256_hex(raw_key)
            ),
            true,
        ))
        .await?;
    println!("LEGACY_API_KEY={raw_key}");

    // Canonical legacy JWT secret (id + enabled + >=32 chars → imported).
    store
        .upsert_config_item_unchecked(item(
            "jwt-secret-v1",
            "jwt_secret",
            "jwt-secret-v1",
            "\"wp28-legacy-jwt-secret-0123456789abcdef\"",
            true,
        ))
        .await?;

    // Non-canonical jwt_secret row → quarantined.
    store
        .upsert_config_item_unchecked(item(
            "jwt-secret-extra",
            "jwt_secret",
            "jwt-secret-extra",
            "\"wp28-extra-jwt-material-not-importable\"",
            true,
        ))
        .await?;

    // Legacy JWT revocation.
    store
        .upsert_config_item_unchecked(item(
            "jwt-revoked-legacyjti0001",
            "jwt_revoked",
            "jwt-revoked-legacyjti0001",
            "{\"exp\": 2000000000}",
            true,
        ))
        .await?;

    // Legacy session row → counted, retired (sessions are JWT now).
    store
        .upsert_config_item_unchecked(item(
            "sess-legacy-1",
            "session",
            "legacy-session",
            "{\"token\":\"bst_legacy\"}",
            true,
        ))
        .await?;

    // Protected service-token names in app_config.
    // enabled routing_service_token → imported into service_tokens.
    store
        .upsert_config_item_unchecked(item(
            "cfg-svc-routing",
            "app_config",
            "routing_service_token",
            "\"rt_legacy_routing_token_sealed_001\"",
            true,
        ))
        .await?;
    // DISABLED internal_service_token → quarantined, NOT activated.
    store
        .upsert_config_item_unchecked(item(
            "cfg-svc-internal-disabled",
            "app_config",
            "internal_service_token",
            "\"it_legacy_disabled_must_not_activate\"",
            false,
        ))
        .await?;
    // Names with no consumer contract → quarantined.
    for (id, name, content) in [
        ("cfg-svc-st", "service_token", "\"svc_legacy_orphan\""),
        ("cfg-svc-adminpw", "admin_password", "\"admin_legacy_pw\""),
        ("cfg-svc-jwtsec", "jwt_secret", "\"appcfg_jwt_secret_material\""),
    ] {
        store
            .upsert_config_item_unchecked(item(id, "app_config", name, content, true))
            .await?;
    }

    println!("fixture seed complete");
    Ok(())
}
