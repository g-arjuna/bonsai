//! BN-022 — Protected authentication storage.
//!
//! `AuthStore` is the single authority for authentication material. It
//! replaces the previous practice of persisting JWT signing secrets,
//! password verifiers, API-key verifiers/scopes and JWT revocation records
//! as graph `ConfigItem` nodes (see `docs/auth_storage.md` for the full
//! inventory, design contract and migration stages).
//!
//! Storage layout:
//!   - `<parent-of-graph-path>/bonsai_auth.db` — SQLite, mode 0600, owned by
//!     the service identity. Holds users, api_keys, jwt_keys, jwt_revoked,
//!     auth_config and auth_audit tables.
//!   - The JWT signing secret is the only recoverable secret; it is stored
//!     age-encrypted under `BONSAI_VAULT_PASSPHRASE` (same key ownership as
//!     `CredentialVault`). Verifiers (Argon2id PHC, SHA-256 key hashes) are
//!     stored as hashes — never plaintext secrets.
//!
//! Failure contract: every fallible operation returns `Result`. Storage
//! errors propagate to callers which must fail closed — an unreadable
//! auth database must never degrade into "no users / no revocations /
//! default key".

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::graph::{ConfigItemRecord, GraphStore};

/// File name of the auth database, placed beside the graph database.
pub const AUTH_DB_FILE_NAME: &str = "bonsai_auth.db";

/// How long a retired JWT signing key remains valid for verification.
/// Must exceed the maximum token lifetime (24 h) plus clock skew margin.
const JWT_KEY_OVERLAP_NS: i64 = 26 * 3600 * 1_000_000_000;

/// Minimum interval between migration attempts after a failure.
const MIGRATION_RETRY_INTERVAL_NS: i64 = 60 * 1_000_000_000;

const JWT_SECRET_ENV: &str = "BONSAI_JWT_SECRET";
/// At-rest protection for the JWT signing secret. Resolution order:
///   1. `BONSAI_VAULT_PASSPHRASE` env (external key ownership — preferred).
///   2. `<auth.db>.key` — a random 0600 keyfile auto-generated beside the
///      database. Protects the secret when the DB file is copied without
///      the keyfile (e.g. partial backups); equivalent to file permissions
///      against a local attacker who already owns the runtime dir.
const VAULT_PASSPHRASE_ENV: &str = "BONSAI_VAULT_PASSPHRASE";

const SCHEMA_VERSION: i64 = 1;

// ── Record types (typed API — no generic row access) ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthUser {
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub enabled: bool,
    pub last_login_ns: i64,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthApiKey {
    pub id: String,
    pub alias: String,
    pub scope: String,
    pub expires_at_ns: i64,
    pub last_used_at_ns: i64,
    pub enabled: bool,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

/// Outcome of a completed (or already-complete) graph→auth migration.
#[derive(Debug, Default)]
pub struct MigrationReport {
    pub already_complete: bool,
    pub users_imported: usize,
    pub api_keys_imported: usize,
    pub jwt_secrets_imported: usize,
    pub jwt_revocations_imported: usize,
    pub legacy_sessions_seen: usize,
    pub malformed_records: usize,
    pub quarantined_rows: usize,
}

// ── AuthStore ────────────────────────────────────────────────────────────────

pub struct AuthStore {
    conn: Mutex<Connection>,
    path: PathBuf,
    /// Key protecting the JWT signing secret at rest (see
    /// `VAULT_PASSPHRASE_ENV` / keyfile fallback). Always present —
    /// `AuthStore::open` fails if neither source can be established.
    secret_key: age::secrecy::SecretString,
    /// Process-local migration gate. Migration is idempotent; this only
    /// avoids re-scanning the graph on every request. Failed attempts are
    /// retried at most once per `MIGRATION_RETRY_INTERVAL_NS` so a
    /// persistent failure cannot turn every request into a graph scan.
    migrated: AtomicBool,
    last_attempt_ns: std::sync::atomic::AtomicI64,
    migration_lock: tokio::sync::Mutex<()>,
}

impl AuthStore {
    /// Open (creating if needed) the auth database at `path`.
    ///
    /// Reads `BONSAI_VAULT_PASSPHRASE` for at-rest encryption of the JWT
    /// signing secret. File permissions are restricted to the service
    /// identity (0600) on Unix.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create auth store dir '{}'", parent.display())
                })?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open auth store '{}'", path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout=5000;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS schema_meta (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS users (
               username TEXT PRIMARY KEY,
               password_hash TEXT NOT NULL,
               role TEXT NOT NULL,
               enabled INTEGER NOT NULL DEFAULT 1,
               last_login_ns INTEGER NOT NULL DEFAULT 0,
               created_at_ns INTEGER NOT NULL,
               updated_at_ns INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS api_keys (
               id TEXT PRIMARY KEY,
               alias TEXT NOT NULL,
               key_hash TEXT NOT NULL,
               scope TEXT NOT NULL,
               expires_at_ns INTEGER NOT NULL DEFAULT 0,
               last_used_at_ns INTEGER NOT NULL DEFAULT 0,
               enabled INTEGER NOT NULL DEFAULT 1,
               created_at_ns INTEGER NOT NULL,
               updated_at_ns INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS jwt_keys (
               kid TEXT PRIMARY KEY,
               is_active INTEGER NOT NULL DEFAULT 0,
               algorithm TEXT NOT NULL DEFAULT 'HS256',
               secret_enc BLOB NOT NULL,
               source TEXT NOT NULL,
               created_at_ns INTEGER NOT NULL,
               retired_at_ns INTEGER,
               verify_until_ns INTEGER
             );
             -- At most one active signing key per database (SQLite treats
             -- NULLs as distinct, so the flag must be a real value).
             CREATE UNIQUE INDEX IF NOT EXISTS jwt_keys_one_active
               ON jwt_keys(is_active) WHERE is_active = 1;
             CREATE TABLE IF NOT EXISTS jwt_revoked (
               jti TEXT PRIMARY KEY,
               expires_at_ns INTEGER NOT NULL,
               revoked_at_ns INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS auth_config (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS auth_audit (
               ts_ns INTEGER NOT NULL,
               actor TEXT NOT NULL,
               action TEXT NOT NULL,
               subject TEXT NOT NULL,
               outcome TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS auth_audit_ts ON auth_audit(ts_ns);",
        )
        .with_context(|| {
            format!(
                "failed to initialize auth store schema at '{}'",
                path.display()
            )
        })?;

        // Durable schema marker.
        conn.execute(
            "INSERT OR IGNORE INTO schema_meta(key, value) VALUES('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;

        restrict_permissions(path)?;

        let secret_key = resolve_secret_key(path)?;

        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
            secret_key,
            migrated: AtomicBool::new(false),
            last_attempt_ns: std::sync::atomic::AtomicI64::new(0),
            migration_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Path of the auth database for a given graph database path: the
    /// `bonsai_auth.db` file in the graph path's parent directory.
    pub fn path_for_graph(graph_path: &str) -> PathBuf {
        let p = Path::new(graph_path);
        let parent = p.parent().unwrap_or_else(|| Path::new("."));
        if parent.as_os_str().is_empty() {
            PathBuf::from(AUTH_DB_FILE_NAME)
        } else {
            parent.join(AUTH_DB_FILE_NAME)
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("auth store lock poisoned"))
    }

    // ── JWT signing key lifecycle ──────────────────────────────────────────

    /// Return the JWT signing secret.
    ///
    /// Order: explicit `BONSAI_JWT_SECRET` env override → active stored key.
    /// A set-but-too-short env value is a hard configuration error. With no
    /// env override and no stored key, a CSPRNG key is generated, encrypted
    /// under the vault passphrase and persisted before being returned — a
    /// transient unpersisted key is never minted.
    pub fn jwt_secret(&self) -> Result<String> {
        if let Ok(env_secret) = std::env::var(JWT_SECRET_ENV) {
            if env_secret.len() >= 32 {
                return Ok(env_secret);
            }
            bail!(
                "{JWT_SECRET_ENV} is set but shorter than 32 bytes; refusing to \
                 mint or accept tokens with an invalid configured key"
            );
        }

        let mut conn = self.lock()?;
        // Active key (at most one — enforced by partial unique index).
        let row = conn.query_row(
            "SELECT kid, secret_enc, source FROM jwt_keys WHERE is_active = 1 \
             ORDER BY created_at_ns DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        );
        match row {
            Ok((kid, secret_enc, source)) => self.decrypt_jwt_secret(&kid, &secret_enc, &source),
            Err(rusqlite::Error::QueryReturnedNoRows) => self.generate_jwt_secret(&mut conn),
            Err(e) => Err(anyhow!("jwt_keys lookup failed: {e}")),
        }
    }

    /// Secrets to try when verifying a JWT, newest first: the active key
    /// plus retired keys still inside their verification overlap window.
    pub fn jwt_verify_candidates(&self) -> Result<Vec<String>> {
        if std::env::var(JWT_SECRET_ENV).is_ok() {
            // Env override is the sole authority while set.
            return self.jwt_secret().map(|s| vec![s]);
        }
        let conn = self.lock()?;
        let now = now_ns();
        let mut stmt = conn.prepare(
            "SELECT kid, secret_enc, source FROM jwt_keys \
             WHERE is_active = 1 \
                OR (verify_until_ns IS NOT NULL AND verify_until_ns > ?1) \
             ORDER BY is_active DESC, created_at_ns DESC",
        )?;
        let rows = stmt.query_map(params![now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kid, enc, source) = row?;
            out.push(self.decrypt_jwt_secret(&kid, &enc, &source)?);
        }
        Ok(out)
    }

    /// Rotate the JWT signing key: generate a new CSPRNG key, retire the
    /// current active key (still verifiable for the overlap window) and
    /// install the new key atomically.
    pub fn rotate_jwt_key(&self) -> Result<()> {
        if std::env::var(JWT_SECRET_ENV).is_ok() {
            bail!("{JWT_SECRET_ENV} override is active; unset it before rotating the stored key");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_ns();
        tx.execute(
            "UPDATE jwt_keys SET is_active = 0, retired_at_ns = ?1, verify_until_ns = ?2 \
             WHERE is_active = 1",
            params![now, now + JWT_KEY_OVERLAP_NS],
        )?;
        let secret = generate_secret_hex();
        let enc = self.encrypt_jwt_secret(&secret)?;
        tx.execute(
            "INSERT INTO jwt_keys(kid, is_active, algorithm, secret_enc, source, created_at_ns) \
             VALUES(?1, 1, 'HS256', ?2, 'generated', ?3)",
            params![format!("key-{}", now), enc, now],
        )?;
        tx.commit()?;
        drop(conn);
        self.audit("system", "jwt_key_rotate", "jwt_keys", "success");
        Ok(())
    }

    /// Import a legacy plaintext JWT secret (migration only): encrypt and
    /// store as the active key. Fails if a key already exists unless
    /// `replace` is set — used to remediate exposed secrets deliberately.
    pub fn import_jwt_secret(&self, plaintext_hex: &str, replace: bool) -> Result<()> {
        if plaintext_hex.len() < 32 {
            bail!("legacy jwt_secret record is too short to be a valid signing key");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_ns();
        if replace {
            tx.execute(
                "UPDATE jwt_keys SET is_active = 0, retired_at_ns = ?1, verify_until_ns = ?2 \
                 WHERE is_active = 1",
                params![now, now + JWT_KEY_OVERLAP_NS],
            )?;
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM jwt_keys WHERE is_active = 1)",
            [],
            |r| r.get(0),
        )?;
        if exists && !replace {
            bail!("an active jwt key already exists; pass replace=true to rotate");
        }
        let enc = self.encrypt_jwt_secret(plaintext_hex)?;
        tx.execute(
            "INSERT INTO jwt_keys(kid, is_active, algorithm, secret_enc, source, created_at_ns) \
             VALUES(?1, 1, 'HS256', ?2, 'imported', ?3)",
            params![format!("imported-{}", now), enc, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn generate_jwt_secret(&self, conn: &mut Connection) -> Result<String> {
        let secret = generate_secret_hex();
        let enc = self.encrypt_jwt_secret(&secret)?;
        let now = now_ns();
        // INSERT may race a concurrent first-start in another process; the
        // partial unique index guarantees a single active row. On conflict,
        // re-read the winner instead of minting a second key.
        match conn.execute(
            "INSERT INTO jwt_keys(kid, is_active, algorithm, secret_enc, source, created_at_ns) \
             VALUES(?1, 1, 'HS256', ?2, 'generated', ?3)",
            params![format!("key-{}", now), enc, now],
        ) {
            Ok(_) => Ok(secret),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                let row = conn.query_row(
                    "SELECT kid, secret_enc, source FROM jwt_keys \
                     WHERE is_active = 1 LIMIT 1",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Vec<u8>>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )?;
                self.decrypt_jwt_secret(&row.0, &row.1, &row.2)
            }
            Err(e) => Err(anyhow!("failed to persist generated jwt key: {e}")),
        }
    }

    fn encrypt_jwt_secret(&self, plaintext: &str) -> Result<Vec<u8>> {
        let encryptor = age::Encryptor::with_user_passphrase(self.secret_key.clone());
        let mut ciphertext = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut ciphertext)
            .context("failed to start jwt secret encryption")?;
        use std::io::Write;
        writer
            .write_all(plaintext.as_bytes())
            .context("failed to encrypt jwt secret")?;
        writer
            .finish()
            .context("failed to finish jwt secret encryption")?;
        Ok(ciphertext)
    }

    fn decrypt_jwt_secret(&self, _kid: &str, secret_enc: &[u8], source: &str) -> Result<String> {
        // 'env' source rows are never produced by this code path; every
        // stored row is age-encrypted. Decryption failure is fatal — never
        // fall back to a default or transient key.
        let _ = source;
        let decryptor = age::Decryptor::new_buffered(std::io::BufReader::new(secret_enc))
            .context("invalid stored jwt secret ciphertext")?;
        if !decryptor.is_scrypt() {
            bail!("stored jwt secret is not passphrase-encrypted");
        }
        let identity = age::scrypt::Identity::new(self.secret_key.clone());
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as _))
            .context("stored jwt secret passphrase rejected")?;
        let mut plaintext = String::new();
        use std::io::Read;
        reader
            .read_to_string(&mut plaintext)
            .context("failed to read decrypted jwt secret")?;
        Ok(plaintext)
    }

    // ── JWT revocation ─────────────────────────────────────────────────────

    /// Record a JWT `jti` as revoked until `expires_at_ns`.
    pub fn revoke_jwt(&self, jti: &str, expires_at_ns: i64) -> Result<()> {
        let conn = self.lock()?;
        let now = now_ns();
        conn.execute(
            "INSERT OR IGNORE INTO jwt_revoked(jti, expires_at_ns, revoked_at_ns) \
             VALUES(?1, ?2, ?3)",
            params![jti, expires_at_ns, now],
        )?;
        // Opportunistic prune of expired entries.
        conn.execute(
            "DELETE FROM jwt_revoked WHERE expires_at_ns < ?1",
            params![now],
        )?;
        Ok(())
    }

    /// `Ok(true)` revoked, `Ok(false)` not revoked, `Err` on storage failure.
    /// Callers MUST treat `Err` as deny — a failed lookup is not proof of
    /// validity.
    pub fn is_jwt_revoked(&self, jti: &str) -> Result<bool> {
        let conn = self.lock()?;
        let now = now_ns();
        let revoked: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM jwt_revoked WHERE jti = ?1 AND expires_at_ns > ?2)",
            params![jti, now],
            |r| r.get(0),
        )?;
        Ok(revoked)
    }

    // ── Users ──────────────────────────────────────────────────────────────

    pub fn list_users(&self) -> Result<Vec<AuthUser>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT username, password_hash, role, enabled, last_login_ns, \
                    created_at_ns, updated_at_ns FROM users ORDER BY username",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AuthUser {
                username: r.get(0)?,
                password_hash: r.get(1)?,
                role: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                last_login_ns: r.get(4)?,
                created_at_ns: r.get(5)?,
                updated_at_ns: r.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| anyhow!("users list failed: {e}"))
    }

    pub fn get_user(&self, username: &str) -> Result<Option<AuthUser>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT username, password_hash, role, enabled, last_login_ns, \
                    created_at_ns, updated_at_ns FROM users WHERE username = ?1",
        )?;
        let mut rows = stmt.query_map(params![username], |r| {
            Ok(AuthUser {
                username: r.get(0)?,
                password_hash: r.get(1)?,
                role: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                last_login_ns: r.get(4)?,
                created_at_ns: r.get(5)?,
                updated_at_ns: r.get(6)?,
            })
        })?;
        match rows.next() {
            Some(row) => row
                .map(Some)
                .map_err(|e| anyhow!("user lookup failed: {e}")),
            None => Ok(None),
        }
    }

    /// Insert or update a user. `created_at_ns` is preserved on update.
    pub fn upsert_user(&self, user: &AuthUser) -> Result<()> {
        let conn = self.lock()?;
        let now = now_ns();
        conn.execute(
            "INSERT INTO users(username, password_hash, role, enabled, last_login_ns, \
                    created_at_ns, updated_at_ns) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6) \
             ON CONFLICT(username) DO UPDATE SET \
               password_hash = excluded.password_hash, \
               role = excluded.role, \
               enabled = excluded.enabled, \
               last_login_ns = excluded.last_login_ns, \
               updated_at_ns = excluded.updated_at_ns",
            params![
                user.username,
                user.password_hash,
                user.role,
                user.enabled as i64,
                user.last_login_ns,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn set_user_enabled(&self, username: &str, enabled: bool) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE users SET enabled = ?1, updated_at_ns = ?2 WHERE username = ?3",
            params![enabled as i64, now_ns(), username],
        )?;
        Ok(n > 0)
    }

    // ── API keys ───────────────────────────────────────────────────────────

    pub fn list_api_keys(&self) -> Result<Vec<AuthApiKey>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, alias, scope, expires_at_ns, last_used_at_ns, enabled, \
                    created_at_ns, updated_at_ns FROM api_keys ORDER BY created_at_ns",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AuthApiKey {
                id: r.get(0)?,
                alias: r.get(1)?,
                scope: r.get(2)?,
                expires_at_ns: r.get(3)?,
                last_used_at_ns: r.get(4)?,
                enabled: r.get::<_, i64>(5)? != 0,
                created_at_ns: r.get(6)?,
                updated_at_ns: r.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| anyhow!("api_keys list failed: {e}"))
    }

    /// Authenticate a presented raw `bsk_` key. Returns the matching key
    /// record on success. Constant-time hash comparison per candidate.
    /// Storage failure returns `Err` — callers must deny.
    pub fn authenticate_api_key(&self, raw_key: &str) -> Result<Option<AuthApiKey>> {
        use subtle::ConstantTimeEq;
        let presented = hex::encode(Sha256::digest(raw_key.as_bytes()));
        for key in self.list_api_keys()? {
            if !key.enabled {
                continue;
            }
            // key_hash is not exposed on AuthApiKey; fetch via dedicated query
            // to keep the summary type free of verifier material.
            let stored_hash = self.key_hash_for_id(&key.id)?;
            let Some(stored_hash) = stored_hash else {
                continue;
            };
            if !bool::from(stored_hash.as_bytes().ct_eq(presented.as_bytes())) {
                continue;
            }
            if key.expires_at_ns > 0 && now_ns() > key.expires_at_ns {
                continue;
            }
            self.touch_api_key_last_used(&key.id)?;
            return Ok(Some(key));
        }
        Ok(None)
    }

    fn key_hash_for_id(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT key_hash FROM api_keys WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => row
                .map(Some)
                .map_err(|e| anyhow!("api key hash lookup failed: {e}")),
            None => Ok(None),
        }
    }

    fn touch_api_key_last_used(&self, id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE api_keys SET last_used_at_ns = ?1 WHERE id = ?2",
            params![now_ns(), id],
        )?;
        Ok(())
    }

    /// Create an API key. Returns `(id, raw_key)` — the raw key is returned
    /// exactly once and only its SHA-256 hash is persisted.
    pub fn create_api_key(
        &self,
        alias: &str,
        scope: &str,
        expires_in_secs: i64,
    ) -> Result<(String, String)> {
        let raw_key = format!(
            "bsk_{}_{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let key_hash = hex::encode(Sha256::digest(raw_key.as_bytes()));
        let id = format!("apikey-{}", &key_hash[..16]);
        let now = now_ns();
        let expires_at_ns = if expires_in_secs > 0 {
            now + expires_in_secs * 1_000_000_000
        } else {
            0
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO api_keys(id, alias, key_hash, scope, expires_at_ns, \
                    last_used_at_ns, enabled, created_at_ns, updated_at_ns) \
             VALUES(?1, ?2, ?3, ?4, ?5, 0, 1, ?6, ?6)",
            params![id, alias, key_hash, scope, expires_at_ns, now],
        )
        .map_err(|e| anyhow!("api key insert failed: {e}"))?;
        drop(conn);
        self.audit("admin", "api_key_create", &id, "success");
        Ok((id, raw_key))
    }

    /// Provision an API key from a pre-computed SHA-256 hex verifier.
    ///
    /// Used by operator tooling and tests that must pin a known raw key;
    /// the raw key itself is never accepted or stored. `key_hash` must be
    /// 64 lowercase/uppercase hex chars. Upserts on `id` conflict.
    pub fn upsert_api_key_verifier(
        &self,
        id: &str,
        alias: &str,
        key_hash: &str,
        scope: &str,
        expires_at_ns: i64,
    ) -> Result<()> {
        if key_hash.len() != 64 || !key_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("api key verifier must be a 64-char hex SHA-256 digest");
        }
        let now = now_ns();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO api_keys(id, alias, key_hash, scope, expires_at_ns, \
                    last_used_at_ns, enabled, created_at_ns, updated_at_ns) \
             VALUES(?1, ?2, ?3, ?4, ?5, 0, 1, ?6, ?6) \
             ON CONFLICT(id) DO UPDATE SET \
               alias = excluded.alias, key_hash = excluded.key_hash, \
               scope = excluded.scope, expires_at_ns = excluded.expires_at_ns, \
               enabled = 1, updated_at_ns = excluded.updated_at_ns",
            params![id, alias, key_hash, scope, expires_at_ns, now],
        )
        .map_err(|e| anyhow!("api key verifier upsert failed: {e}"))?;
        Ok(())
    }

    /// Rotate an API key atomically: disable the old key and create its
    /// replacement (new id/hash, same alias+scope+expiry) in one transaction.
    /// Returns `(new_id, raw_key, alias)` or `None` if the key does not exist.
    pub fn rotate_api_key(&self, id: &str) -> Result<Option<(String, String, String)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let old = tx
            .query_row(
                "SELECT alias, scope, expires_at_ns, enabled FROM api_keys WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        // Preserve the old enabled flag: rotating a disabled/revoked key
        // must not silently produce an enabled replacement.
        let Some((alias, scope, expires_at_ns, enabled)) = old else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE api_keys SET enabled = 0, updated_at_ns = ?1 WHERE id = ?2",
            params![now_ns(), id],
        )?;
        let raw_key = format!(
            "bsk_{}_{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let key_hash = hex::encode(Sha256::digest(raw_key.as_bytes()));
        let new_id = format!("apikey-{}", &key_hash[..16]);
        let now = now_ns();
        tx.execute(
            "INSERT INTO api_keys(id, alias, key_hash, scope, expires_at_ns, \
                    last_used_at_ns, enabled, created_at_ns, updated_at_ns) \
             VALUES(?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?7)",
            params![new_id, alias, key_hash, scope, expires_at_ns, enabled, now],
        )?;
        tx.commit()?;
        drop(conn);
        self.audit("admin", "api_key_rotate", id, "success");
        Ok(Some((new_id, raw_key, alias)))
    }

    /// Disable (revoke) an API key. Returns `false` when not found.
    pub fn disable_api_key(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE api_keys SET enabled = 0, updated_at_ns = ?1 WHERE id = ?2",
            params![now_ns(), id],
        )?;
        drop(conn);
        if n > 0 {
            self.audit("admin", "api_key_disable", id, "success");
        }
        Ok(n > 0)
    }

    // ── Auth config / audit ────────────────────────────────────────────────

    pub fn get_auth_config(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT value FROM auth_config WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |r| r.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => row
                .map(Some)
                .map_err(|e| anyhow!("auth_config read failed: {e}")),
            None => Ok(None),
        }
    }

    pub fn set_auth_config(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO auth_config(key, value) VALUES(?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Record authentication audit metadata. Never pass secret material —
    /// `subject` is an identifier (username, key id, jti), not a credential.
    pub fn audit(&self, actor: &str, action: &str, subject: &str, outcome: &str) {
        let result = (|| -> Result<()> {
            let conn = self.lock()?;
            conn.execute(
                "INSERT INTO auth_audit(ts_ns, actor, action, subject, outcome) \
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![now_ns(), actor, action, subject, outcome],
            )?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!(error = %e, "auth audit write failed");
        }
    }

    // ── Graph → auth migration (WP3) ───────────────────────────────────────

    /// Ensure legacy graph credential records have been imported. Runs at
    /// most once per process; the durable `auth_config` marker makes the
    /// migration itself idempotent and resumable across restarts.
    ///
    /// MUST be called before serving auth decisions on an upgraded
    /// installation. Failure is returned to the caller — auth paths treat
    /// it as fail-closed.
    pub async fn ensure_migrated(&self, store: &GraphStore) -> Result<()> {
        if self.migrated.load(Ordering::Acquire) {
            return Ok(());
        }
        let last = self.last_attempt_ns.load(Ordering::Acquire);
        if last > 0 && now_ns().saturating_sub(last) < MIGRATION_RETRY_INTERVAL_NS {
            bail!("auth migration previously failed; retrying shortly");
        }
        let _guard = self.migration_lock.lock().await;
        if self.migrated.load(Ordering::Acquire) {
            return Ok(());
        }
        self.last_attempt_ns.store(now_ns(), Ordering::Release);
        let report = self.migrate_from_graph(store).await?;
        if !report.already_complete {
            tracing::info!(
                users = report.users_imported,
                api_keys = report.api_keys_imported,
                jwt_revocations = report.jwt_revocations_imported,
                malformed = report.malformed_records,
                quarantined = report.quarantined_rows,
                "auth storage migration complete"
            );
        }
        self.migrated.store(true, Ordering::Release);
        Ok(())
    }

    /// Staged, idempotent migration from graph `ConfigItem` records.
    /// See `docs/auth_storage.md` §4 for the stage contract.
    pub async fn migrate_from_graph(&self, store: &GraphStore) -> Result<MigrationReport> {
        let mut report = MigrationReport::default();

        // Stage marker: 'complete' short-circuits; anything else resumes.
        if self.get_auth_config("migration_state")?.as_deref() == Some("complete") {
            report.already_complete = true;
            return Ok(report);
        }
        self.set_auth_config("migration_stage", "detect")?;

        // ── detect + validate: read all legacy classes ──────────────────
        let classes = ["user", "api_key", "jwt_secret", "jwt_revoked", "session"];
        let mut legacy_users = Vec::new();
        let mut legacy_keys = Vec::new();
        let mut legacy_jwt_secret: Option<String> = None;
        let mut legacy_revocations = Vec::new();
        for class in classes {
            let items = store
                .list_config_items(Some(class.to_string()))
                .await
                .with_context(|| format!("migration: failed to list legacy '{class}' items"))?;
            for item in items {
                match class {
                    "user" => match parse_legacy_user(&item) {
                        Ok(u) => legacy_users.push((item, u)),
                        Err(_) => report.malformed_records += 1,
                    },
                    "api_key" => match parse_legacy_api_key(&item) {
                        Ok(k) => legacy_keys.push((item, k)),
                        Err(_) => report.malformed_records += 1,
                    },
                    "jwt_secret" => {
                        if item.id == "jwt-secret-v1" && item.enabled {
                            if item.content_json.len() >= 32 {
                                legacy_jwt_secret = Some(item.content_json.clone());
                            } else {
                                report.malformed_records += 1;
                            }
                        }
                    }
                    "jwt_revoked" => {
                        if let Some(jti) = item.id.strip_prefix("jwt-revoked-") {
                            // Legacy content_json {"exp": <unix seconds>};
                            // auth store expiry is nanoseconds. An
                            // unparseable expiry keeps the revocation
                            // effectively permanent — safer than dropping
                            // a denylist entry.
                            let exp_ns =
                                serde_json::from_str::<serde_json::Value>(&item.content_json)
                                    .ok()
                                    .and_then(|v| v.get("exp")?.as_i64())
                                    .filter(|s| *s > 0)
                                    .map(|s| s.saturating_mul(1_000_000_000))
                                    .unwrap_or(i64::MAX);
                            legacy_revocations.push((jti.to_string(), exp_ns));
                        } else {
                            report.malformed_records += 1;
                        }
                    }
                    "session" => report.legacy_sessions_seen += 1,
                    _ => {}
                }
            }
        }
        self.set_auth_config("migration_stage", "import")?;

        // ── import (idempotent: PK conflicts skipped, content conflicts error) ──
        for (item, user) in &legacy_users {
            if let Some(existing) = self.get_user(&user.username)? {
                if existing.password_hash != user.password_hash || existing.role != user.role {
                    bail!(
                        "migration conflict: user '{}' exists in auth store with different credentials",
                        user.username
                    );
                }
            } else {
                self.upsert_user(&AuthUser {
                    username: user.username.clone(),
                    password_hash: user.password_hash.clone(),
                    role: user.role.clone(),
                    enabled: user.enabled,
                    last_login_ns: user.last_login_ns,
                    created_at_ns: 0,
                    updated_at_ns: 0,
                })?;
                report.users_imported += 1;
            }
            let _ = item;
        }
        for (_item, key) in &legacy_keys {
            let exists = self.list_api_keys()?.iter().any(|k| k.id == key.id);
            if exists {
                continue;
            }
            let now = now_ns();
            let conn = self.lock()?;
            conn.execute(
                "INSERT INTO api_keys(id, alias, key_hash, scope, expires_at_ns, \
                        last_used_at_ns, enabled, created_at_ns, updated_at_ns) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    key.id,
                    key.alias,
                    key.key_hash,
                    key.scope,
                    key.expires_at_ns,
                    key.last_used_at_ns,
                    key.enabled as i64,
                    now,
                ],
            )?;
            report.api_keys_imported += 1;
        }
        for (jti, exp) in &legacy_revocations {
            self.revoke_jwt(jti, *exp)?;
            report.jwt_revocations_imported += 1;
        }

        // JWT secret: only import when no env override is configured and no
        // active key exists yet. Requires the vault passphrase for encryption.
        if std::env::var(JWT_SECRET_ENV).is_err() {
            let has_active = {
                let conn = self.lock()?;
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jwt_keys WHERE is_active = 1)",
                    [],
                    |r| r.get::<_, bool>(0),
                )?
            };
            if !has_active {
                if let Some(secret) = legacy_jwt_secret {
                    // import_jwt_secret encrypts under the vault passphrase;
                    // a missing passphrase fails here — explicit, resumable.
                    self.import_jwt_secret(&secret, false)?;
                    report.jwt_secrets_imported += 1;
                }
            }
        }
        self.set_auth_config("migration_stage", "verify")?;

        // ── verify completeness before switching authority ──────────────
        // db_users/db_keys count ALL rows including any created through the
        // new API before migration ran; they must be at least the imported
        // counts (a smaller count means import loss).
        let db_users = self.list_users()?.len();
        let db_keys = self.list_api_keys()?.len();
        if db_users < report.users_imported {
            bail!("migration verify failed: user count mismatch");
        }
        if db_keys < report.api_keys_imported {
            bail!("migration verify failed: api key count mismatch");
        }

        // ── quarantine legacy graph rows (disable + scrub, never delete) ──
        // Runs BEFORE the completion marker so a crash mid-quarantine
        // re-runs the whole (idempotent) migration and finishes scrubbing
        // every legacy row — no enabled credential row is left behind, and
        // no credential material remains queryable through generic graph
        // APIs (including admin queries). The authoritative copy now lives
        // only in AuthStore; the graph row becomes an audit tombstone.
        self.set_auth_config("migration_stage", "quarantine")?;
        for class in ["user", "api_key", "jwt_secret", "jwt_revoked", "session"] {
            let items = store
                .list_config_items(Some(class.to_string()))
                .await
                .with_context(|| format!("migration quarantine: list '{class}' failed"))?;
            for mut item in items {
                if !item.enabled && item.content_json.contains("\"migrated_to\":\"auth_store\"") {
                    continue;
                }
                item.enabled = false;
                item.content_json = serde_json::json!({
                    "migrated_to": "auth_store",
                    "quarantined_at_ns": now_ns(),
                })
                .to_string();
                store
                    .upsert_config_item(item)
                    .await
                    .with_context(|| format!("migration quarantine: scrub '{class}' row failed"))?;
                report.quarantined_rows += 1;
            }
        }
        self.set_auth_config("migration_stage", "done")?;
        // ── switch: auth authority is AuthStore from this point on ───────
        self.set_auth_config("migration_state", "complete")?;
        Ok(report)
    }
}

// ── Legacy record parsing ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct LegacyUser {
    username: String,
    password_hash: String,
    role: String,
    enabled: bool,
    last_login_ns: i64,
}

#[derive(Debug, Clone)]
struct LegacyApiKey {
    id: String,
    alias: String,
    key_hash: String,
    scope: String,
    expires_at_ns: i64,
    last_used_at_ns: i64,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct LegacyUserContent {
    password_hash: String,
    #[serde(default = "default_legacy_role")]
    role: String,
    #[serde(default)]
    last_login_ns: i64,
}

fn default_legacy_role() -> String {
    "viewer".to_string()
}

#[derive(Debug, Deserialize)]
struct LegacyApiKeyContent {
    key_hash: String,
    #[serde(default = "default_legacy_scope")]
    scope: String,
    #[serde(default)]
    expires_at_ns: i64,
    #[serde(default)]
    last_used_at_ns: i64,
}

fn default_legacy_scope() -> String {
    "read".to_string()
}

fn parse_legacy_user(item: &ConfigItemRecord) -> Result<LegacyUser> {
    if item.name.trim().is_empty() {
        bail!("legacy user record has empty name");
    }
    let content: LegacyUserContent =
        serde_json::from_str(&item.content_json).context("legacy user content_json invalid")?;
    if content.password_hash.is_empty() {
        bail!("legacy user has empty password_hash");
    }
    Ok(LegacyUser {
        username: item.name.clone(),
        password_hash: content.password_hash,
        role: content.role,
        enabled: item.enabled,
        last_login_ns: content.last_login_ns,
    })
}

fn parse_legacy_api_key(item: &ConfigItemRecord) -> Result<LegacyApiKey> {
    let content: LegacyApiKeyContent =
        serde_json::from_str(&item.content_json).context("legacy api_key content_json invalid")?;
    if content.key_hash.len() != 64 || !content.key_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("legacy api_key has malformed key_hash");
    }
    Ok(LegacyApiKey {
        id: item.id.clone(),
        alias: item.name.clone(),
        key_hash: content.key_hash,
        scope: content.scope,
        expires_at_ns: content.expires_at_ns,
        last_used_at_ns: content.last_used_at_ns,
        enabled: item.enabled,
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn generate_secret_hex() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

/// Resolve the at-rest key for JWT secret encryption.
///
/// `BONSAI_VAULT_PASSPHRASE` wins when set (external key ownership). Otherwise
/// a random keyfile `<auth.db>.key` is created (mode 0600) or read. Any
/// failure is fatal — protected storage that cannot protect its key material
/// must not come up.
fn resolve_secret_key(auth_db_path: &Path) -> Result<age::secrecy::SecretString> {
    if let Ok(pass) = std::env::var(VAULT_PASSPHRASE_ENV) {
        if !pass.is_empty() {
            return Ok(age::secrecy::SecretString::new(pass.into()));
        }
    }
    let key_path = auth_db_path.with_extension("db.key");
    if key_path.exists() {
        let raw = std::fs::read_to_string(&key_path)
            .with_context(|| format!("failed to read auth keyfile '{}'", key_path.display()))?;
        let trimmed = raw.trim();
        if trimmed.len() < 32 {
            bail!(
                "auth keyfile '{}' is too short; regenerate it and rotate the JWT key",
                key_path.display()
            );
        }
        return Ok(age::secrecy::SecretString::new(trimmed.to_string().into()));
    }
    // Generate a fresh keyfile atomically.
    let mut bytes = [0u8; 32];
    {
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut bytes);
    }
    let key = hex::encode(bytes);
    let tmp = key_path.with_extension("key.tmp");
    std::fs::write(&tmp, format!("{key}\n"))
        .with_context(|| format!("failed to write auth keyfile tmp '{}'", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &key_path)
        .with_context(|| format!("failed to install auth keyfile '{}'", key_path.display()))?;
    Ok(age::secrecy::SecretString::new(key.into()))
}

/// Restrict the auth database (and its SQLite sidecars) to owner-only.
fn restrict_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [
            path.to_path_buf(),
            path.with_extension("db-wal"),
            path.with_extension("db-shm"),
            path.with_extension("db-journal"),
        ] {
            if p.exists() {
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("failed to restrict '{}'", p.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Open a store with a deterministic secret key: env overrides are
    /// cleared under the process-wide env lock (returned to the caller so
    /// the vars stay cleared for the test's duration).
    fn temp_store() -> (
        tempfile::TempDir,
        AuthStore,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("BONSAI_VAULT_PASSPHRASE");
            std::env::remove_var("BONSAI_JWT_SECRET");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = AuthStore::path_for_graph(&dir.path().join("bonsai.db").to_string_lossy());
        // path_for_graph puts auth db in parent of graph path = tempdir root
        let store = AuthStore::open(&path).expect("open auth store");
        (dir, store, guard)
    }

    #[test]
    fn jwt_secret_is_csprng_stable_and_persisted() {
        let (_dir, store, _env) = temp_store();
        let s1 = store.jwt_secret().expect("jwt secret");
        assert!(s1.len() >= 32);
        // Re-open: same secret (stable across restarts).
        let path = store.path().to_path_buf();
        drop(store);
        let store2 = AuthStore::open(&path).expect("reopen");
        assert_eq!(store2.jwt_secret().unwrap(), s1);
    }

    #[test]
    fn jwt_secret_env_override_wins_and_short_env_fails() {
        let (_dir, store, _env) = temp_store();
        unsafe { std::env::set_var("BONSAI_JWT_SECRET", "a".repeat(40)) };
        assert_eq!(store.jwt_secret().unwrap(), "a".repeat(40));
        unsafe { std::env::set_var("BONSAI_JWT_SECRET", "short") };
        assert!(store.jwt_secret().is_err());
        unsafe { std::env::remove_var("BONSAI_JWT_SECRET") };
    }

    #[test]
    fn jwt_secret_fails_when_key_material_is_wrong() {
        let (dir, store, _env) = temp_store();
        let s1 = store.jwt_secret().expect("jwt secret");
        let keyfile = dir.path().join("bonsai_auth.db.key");
        assert!(keyfile.exists(), "keyfile should be auto-created");
        // Corrupt the keyfile: decryption must fail, never fall back.
        std::fs::write(&keyfile, format!("{}\n", "f".repeat(64))).unwrap();
        drop(store);
        let path = AuthStore::path_for_graph(&dir.path().join("bonsai.db").to_string_lossy());
        let store2 = AuthStore::open(&path).expect("reopen");
        assert!(store2.jwt_secret().is_err());
        let _ = s1;
    }

    #[test]
    fn jwt_rotation_keeps_old_key_verifying_during_overlap() {
        let (_dir, store, _env) = temp_store();
        let old = store.jwt_secret().unwrap();
        store.rotate_jwt_key().expect("rotate");
        let new = store.jwt_secret().unwrap();
        assert_ne!(old, new);
        // Both old (retired, in overlap) and new must be verification candidates.
        let candidates = store.jwt_verify_candidates().unwrap();
        assert!(candidates.contains(&old));
        assert!(candidates.contains(&new));
        // Exactly one active key.
        let conn = store.lock().unwrap();
        let active: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jwt_keys WHERE is_active = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(active, 1);
    }

    #[test]
    fn api_key_lifecycle_is_atomic() {
        let (_dir, store, _env) = temp_store();
        let (id, raw) = store.create_api_key("svc", "read", 0).unwrap();
        assert!(raw.starts_with("bsk_"));
        // Raw key never persisted: hash column holds sha256 hex only.
        let conn = store.lock().unwrap();
        let stored: String = conn
            .query_row(
                "SELECT key_hash FROM api_keys WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored.len(), 64);
        assert!(!stored.contains(&raw));
        drop(conn);

        let auth = store.authenticate_api_key(&raw).unwrap().expect("auth");
        assert_eq!(auth.id, id);
        assert_eq!(auth.scope, "read");
        assert!(store.authenticate_api_key("bsk_wrong").unwrap().is_none());

        // Rotate: old key dead, new key live, same alias/scope.
        let (new_id, new_raw, alias) = store.rotate_api_key(&id).unwrap().unwrap();
        assert_ne!(new_id, id);
        assert_eq!(alias, "svc");
        assert!(store.authenticate_api_key(&raw).unwrap().is_none());
        assert!(store.authenticate_api_key(&new_raw).unwrap().is_some());

        // Disable new key.
        assert!(store.disable_api_key(&new_id).unwrap());
        assert!(store.authenticate_api_key(&new_raw).unwrap().is_none());
        assert!(!store.disable_api_key("apikey-nope").unwrap());

        // Rotating a disabled key must not silently re-enable the
        // replacement — the revoked state carries over.
        let (rot_id, rot_raw, _) = store.rotate_api_key(&new_id).unwrap().unwrap();
        assert!(store.authenticate_api_key(&rot_raw).unwrap().is_none());
        let rotated = store
            .list_api_keys()
            .unwrap()
            .into_iter()
            .find(|k| k.id == rot_id)
            .unwrap();
        assert!(!rotated.enabled);
    }

    #[test]
    fn user_lifecycle_preserves_verifier() {
        let (_dir, store, _env) = temp_store();
        let hash = crate::http_server::auth::hash_password("Correct-Horse-Battery!").unwrap();
        let user = AuthUser {
            username: "alice".into(),
            password_hash: hash,
            role: "operator".into(),
            enabled: true,
            last_login_ns: 0,
            created_at_ns: 0,
            updated_at_ns: 0,
        };
        store.upsert_user(&user).unwrap();
        let got = store.get_user("alice").unwrap().unwrap();
        assert!(got.enabled);
        assert_eq!(got.role, "operator");
        assert!(store.set_user_enabled("alice", false).unwrap());
        assert!(!store.get_user("alice").unwrap().unwrap().enabled);
        assert!(!store.set_user_enabled("ghost", false).unwrap());
    }

    #[test]
    fn jwt_revocation_roundtrip() {
        let (_dir, store, _env) = temp_store();
        assert!(!store.is_jwt_revoked("jti-1").unwrap());
        store.revoke_jwt("jti-1", now_ns() + 1_000_000_000).unwrap();
        assert!(store.is_jwt_revoked("jti-1").unwrap());
        // Expired revocation entries do not match.
        store.revoke_jwt("jti-2", now_ns() - 1).unwrap();
        assert!(!store.is_jwt_revoked("jti-2").unwrap());
    }

    #[test]
    fn file_permissions_are_owner_only() {
        let (dir, _store, _env) = temp_store();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("bonsai_auth.db"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let kmode = std::fs::metadata(dir.path().join("bonsai_auth.db.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(kmode, 0o600);
        }
    }
}
