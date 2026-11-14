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

const SCHEMA_VERSION: i64 = 2;

/// Schema versions this build can open. v1 → v2 is additive only (the
/// `auth_quarantine` table); no credential data changes shape.
const SUPPORTED_SCHEMA_VERSIONS: &[i64] = &[1, 2];

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
    /// Legacy records preserved in `auth_quarantine` (malformed or
    /// otherwise unresolved) before their graph rows were scrubbed.
    pub quarantined_records: usize,
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
    /// Process-local cache of decrypted JWT signing secrets (AUTH-R6).
    /// Populated lazily from `jwt_keys`; every mutation of `jwt_keys`
    /// bumps `jwt_generation` while holding the store mutex, so a cache
    /// entry tagged with an older generation is always stale. The cache
    /// is per-process: it never weakens rotation or revocation semantics
    /// (revocation is the separate `jwt_revoked` table, checked per
    /// request) and is documented as process-local in auth_storage.md.
    jwt_cache: Mutex<Option<JwtSecretCache>>,
    jwt_generation: std::sync::atomic::AtomicI64,
}

struct JwtSecretCache {
    generation: i64,
    /// Decrypted secrets valid for verification, active key first.
    secrets: Vec<String>,
    /// The single active signing key (`is_active = 1`), if any. Kept
    /// separate from `secrets` because `jwt_secret()` must mint with the
    /// active key — never a retired verification-only key.
    active: Option<String>,
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
        // Pre-create the database file with owner-only permissions so it
        // is never observable with permissive mode between creation and
        // the post-open restrict_permissions pass.
        if !path.exists() {
            create_owner_only_file(path)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open auth store '{}'", path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout=5000;
             PRAGMA foreign_keys=ON;",
        )
        .with_context(|| {
            format!(
                "failed to initialize auth store pragmas at '{}'",
                path.display()
            )
        })?;

        // Schema version gate (AUTH-R4): an existing database whose
        // recorded version is unknown/newer must not be silently mutated
        // by CREATE ... IF NOT EXISTS. Missing version on an existing
        // schema_meta means a partially-initialized store — the idempotent
        // DDL below completes it, then the version is (re)written.
        let existing_version = read_schema_version(&conn)?;
        if let Some(v) = existing_version {
            if !SUPPORTED_SCHEMA_VERSIONS.contains(&v) {
                bail!(
                    "auth store '{}' has unsupported schema_version {v}; \
                     this build supports {SUPPORTED_SCHEMA_VERSIONS:?}",
                    path.display()
                );
            }
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (
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
             CREATE INDEX IF NOT EXISTS auth_audit_ts ON auth_audit(ts_ns);
             -- AUTH-R2: recoverable quarantine for legacy credential
             -- records that cannot be safely imported (malformed or
             -- conflicting). Content is preserved inside the protected
             -- store so the graph row can be scrubbed without losing the
             -- only copy. Never exposed through graph or auth APIs.
             CREATE TABLE IF NOT EXISTS auth_quarantine (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               record_class TEXT NOT NULL,
               item_id TEXT NOT NULL,
               content_json TEXT NOT NULL,
               reason TEXT NOT NULL,
               quarantined_at_ns INTEGER NOT NULL,
               UNIQUE(record_class, item_id)
             );",
        )
        .with_context(|| {
            format!(
                "failed to initialize auth store schema at '{}'",
                path.display()
            )
        })?;

        // Durable schema marker: write the current version unconditionally
        // (v1 databases are upgraded in place — the v1→v2 delta is the
        // additive auth_quarantine table created above).
        conn.execute(
            "INSERT OR REPLACE INTO schema_meta(key, value) VALUES('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        // Post-condition: the marker must read back as the supported
        // version, and every required table must exist — a partially
        // initialized store must never appear usable.
        let verify_version = read_schema_version(&conn)?;
        if verify_version != Some(SCHEMA_VERSION) {
            bail!(
                "auth store '{}' failed schema initialization (version {:?})",
                path.display(),
                verify_version
            );
        }
        for table in [
            "users",
            "api_keys",
            "jwt_keys",
            "jwt_revoked",
            "auth_config",
            "auth_audit",
            "auth_quarantine",
        ] {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![table],
                |r| r.get(0),
            )?;
            if !exists {
                bail!(
                    "auth store '{}' missing required table '{table}' after initialization",
                    path.display()
                );
            }
        }

        restrict_permissions(path)?;

        // AUTH-R4: a freshly created keyfile beside a database that already
        // holds encrypted JWT keys means the original key material is lost
        // (or was never this host's). Minting a new key here would produce
        // a store that can never decrypt its signing keys — fail closed
        // instead of silently establishing a divergent encryption identity.
        let (secret_key, keyfile_created) = resolve_secret_key(path)?;
        if keyfile_created {
            let has_jwt_keys: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM jwt_keys)",
                [],
                |r| r.get(0),
            )?;
            if has_jwt_keys {
                bail!(
                    "auth store '{}' contains encrypted JWT keys but its \
                     keyfile was missing and a new one was generated; the \
                     existing keys cannot be decrypted. Restore the original \
                     keyfile or set {VAULT_PASSPHRASE_ENV} before starting.",
                    path.display()
                );
            }
        }

        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
            secret_key,
            migrated: AtomicBool::new(false),
            last_attempt_ns: std::sync::atomic::AtomicI64::new(0),
            migration_lock: tokio::sync::Mutex::new(()),
            jwt_cache: Mutex::new(None),
            jwt_generation: std::sync::atomic::AtomicI64::new(0),
        })
    }

    /// Path of the auth database for a given graph database path: the
    /// `bonsai_auth.db` file in the graph path's parent directory.
    ///
    /// This is **intentional runtime-wide ownership**: Bonsai treats the
    /// parent directory of the graph database as one runtime data
    /// directory, and authentication is scoped to that directory — not to
    /// an individual graph file. Two graph databases in the same
    /// directory therefore share one `AuthStore` (users, keys, JWT keys,
    /// migration marker); graph databases in different directories get
    /// independent auth stores. Operators must not place unrelated
    /// Bonsai instances' graph files in one directory.
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

        // Fast path: process-local cache (AUTH-R6). The active key —
        // never a retired verification-only key — is used for minting.
        if let Some(active) = self.jwt_active_secret()? {
            return Ok(active);
        }

        // No keys at all: generate and persist one. Concurrent first
        // starts are serialized by the store mutex + unique index; the
        // loser re-reads the winner's row.
        let mut conn = self.lock()?;
        self.generate_jwt_secret(&mut conn)
    }

    /// Secrets to try when verifying a JWT, newest first: the active key
    /// plus retired keys still inside their verification overlap window.
    ///
    /// Decrypted material is served from a process-local cache keyed on
    /// the `jwt_keys` generation counter (AUTH-R6): the expensive
    /// age/scrypt decrypt runs once per key generation, not per request.
    /// The cache never extends a retired key's `verify_until_ns` window —
    /// expiry is enforced by the SQL selection, and pruning below keeps
    /// retired-key accumulation bounded.
    pub fn jwt_verify_candidates(&self) -> Result<Vec<String>> {
        if std::env::var(JWT_SECRET_ENV).is_ok() {
            // Env override is the sole authority while set.
            return self.jwt_secret().map(|s| vec![s]);
        }
        self.jwt_secrets()
    }

    /// Ensure the process-local JWT secret cache is fresh for the current
    /// key generation, refreshing it under the store mutex when stale.
    fn ensure_jwt_cache(&self) -> Result<()> {
        let generation = self.jwt_generation.load(Ordering::Acquire);
        {
            let cache = self
                .jwt_cache
                .lock()
                .map_err(|_| anyhow!("jwt cache lock poisoned"))?;
            if let Some(entry) = cache.as_ref() {
                if entry.generation == generation {
                    return Ok(());
                }
            }
        }

        // Refresh under the store mutex: every jwt_keys mutation bumps the
        // generation before releasing this lock, so the rows read here are
        // consistent with `generation`.
        let conn = self.lock()?;
        let generation = self.jwt_generation.load(Ordering::Acquire);
        let now = now_ns();
        // Bound retired-key accumulation (AUTH-R5): drop retired keys whose
        // verification window has closed — they can never verify again.
        conn.execute(
            "DELETE FROM jwt_keys WHERE is_active = 0 AND \
             (verify_until_ns IS NULL OR verify_until_ns <= ?1)",
            params![now],
        )?;
        let mut stmt = conn.prepare(
            "SELECT kid, is_active, secret_enc, source FROM jwt_keys \
             WHERE is_active = 1 \
                OR (verify_until_ns IS NOT NULL AND verify_until_ns > ?1) \
             ORDER BY is_active DESC, created_at_ns DESC",
        )?;
        let rows = stmt.query_map(params![now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut secrets = Vec::new();
        let mut active = None;
        for row in rows {
            let (kid, is_active, enc, source) = row?;
            let plain = self.decrypt_jwt_secret(&kid, &enc, &source)?;
            if is_active == 1 && active.is_none() {
                active = Some(plain.clone());
            }
            secrets.push(plain);
        }
        drop(stmt);
        *self
            .jwt_cache
            .lock()
            .map_err(|_| anyhow!("jwt cache lock poisoned"))? = Some(JwtSecretCache {
            generation,
            secrets,
            active,
        });
        Ok(())
    }

    /// All currently-verifiable JWT secrets (active first) from the
    /// process-local cache.
    fn jwt_secrets(&self) -> Result<Vec<String>> {
        self.ensure_jwt_cache()?;
        let cache = self
            .jwt_cache
            .lock()
            .map_err(|_| anyhow!("jwt cache lock poisoned"))?;
        Ok(cache
            .as_ref()
            .map(|c| c.secrets.clone())
            .unwrap_or_default())
    }

    /// Active signing secret from the cache, or `None` when no active key
    /// exists. Never returns a retired verification-only key.
    fn jwt_active_secret(&self) -> Result<Option<String>> {
        self.ensure_jwt_cache()?;
        let cache = self
            .jwt_cache
            .lock()
            .map_err(|_| anyhow!("jwt cache lock poisoned"))?;
        Ok(cache.as_ref().and_then(|c| c.active.clone()))
    }

    /// Rotate the JWT signing key: generate a new CSPRNG key, retire the
    /// current active key (still verifiable for the overlap window) and
    /// install the new key atomically.
    pub fn rotate_jwt_key(&self) -> Result<()> {
        self.rotate_jwt_key_with_overlap(JWT_KEY_OVERLAP_NS)
    }

    /// Emergency rotation for a suspected-compromised signing key
    /// (AUTH-R5): the retired key's verification window is closed
    /// immediately (`verify_until_ns = now`) and every other retired key
    /// is invalidated too — all previously issued tokens fail signature
    /// verification at once. Users simply re-login; no mass revocation
    /// list is needed because invalidation is cryptographic.
    pub fn rotate_jwt_key_emergency(&self) -> Result<()> {
        if std::env::var(JWT_SECRET_ENV).is_ok() {
            bail!("{JWT_SECRET_ENV} override is active; stored-key emergency rotation does not apply");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_ns();
        tx.execute(
            "UPDATE jwt_keys SET is_active = 0, retired_at_ns = ?1, verify_until_ns = ?1 \
             WHERE is_active = 1",
            params![now],
        )?;
        tx.execute(
            "UPDATE jwt_keys SET verify_until_ns = ?1 WHERE is_active = 0",
            params![now],
        )?;
        self.insert_generated_key(&tx, now)?;
        tx.commit()?;
        self.bump_jwt_generation();
        drop(conn);
        self.audit("system", "jwt_key_rotate_emergency", "jwt_keys", "success");
        Ok(())
    }

    fn rotate_jwt_key_with_overlap(&self, overlap_ns: i64) -> Result<()> {
        if std::env::var(JWT_SECRET_ENV).is_ok() {
            bail!("{JWT_SECRET_ENV} override is active; unset it before rotating the stored key");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_ns();
        tx.execute(
            "UPDATE jwt_keys SET is_active = 0, retired_at_ns = ?1, verify_until_ns = ?2 \
             WHERE is_active = 1",
            params![now, now.saturating_add(overlap_ns)],
        )?;
        self.insert_generated_key(&tx, now)?;
        tx.commit()?;
        self.bump_jwt_generation();
        drop(conn);
        self.audit("system", "jwt_key_rotate", "jwt_keys", "success");
        Ok(())
    }

    fn insert_generated_key(&self, tx: &rusqlite::Transaction<'_>, now: i64) -> Result<()> {
        let secret = generate_secret_hex();
        let enc = self.encrypt_jwt_secret(&secret)?;
        tx.execute(
            "INSERT INTO jwt_keys(kid, is_active, algorithm, secret_enc, source, created_at_ns) \
             VALUES(?1, 1, 'HS256', ?2, 'generated', ?3)",
            params![format!("key-{}", now), enc, now],
        )?;
        Ok(())
    }

    fn bump_jwt_generation(&self) {
        self.jwt_generation.fetch_add(1, Ordering::AcqRel);
        // Invalidate eagerly so a concurrent reader cannot observe the
        // pre-mutation cache under the new generation.
        if let Ok(mut cache) = self.jwt_cache.lock() {
            *cache = None;
        }
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
        let has_active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM jwt_keys WHERE is_active = 1)",
            [],
            |r| r.get(0),
        )?;
        if has_active && !replace {
            bail!("an active jwt key already exists; pass replace=true to rotate");
        }
        let enc = self.encrypt_jwt_secret(plaintext_hex)?;
        tx.execute(
            "INSERT INTO jwt_keys(kid, is_active, algorithm, secret_enc, source, created_at_ns) \
             VALUES(?1, 1, 'HS256', ?2, 'imported', ?3)",
            params![format!("imported-{}", now), enc, now],
        )?;
        tx.commit()?;
        self.bump_jwt_generation();
        drop(conn);
        self.audit("system", "jwt_key_import", "jwt_keys", "success");
        Ok(())
    }

    /// Import a legacy plaintext JWT secret as a **retired,
    /// verification-only** key (migration continuity, AUTH-R1): used when
    /// an active key already exists, so tokens minted under the legacy
    /// secret keep verifying for a bounded overlap window without
    /// displacing the active signing key.
    ///
    /// The `kid` is derived from the secret's hash, so a retried
    /// migration cannot accumulate duplicate retired rows.
    pub fn import_jwt_secret_retired(&self, plaintext_hex: &str) -> Result<()> {
        if plaintext_hex.len() < 32 {
            bail!("legacy jwt_secret record is too short to be a valid signing key");
        }
        let kid = format!(
            "imported-retired-{}",
            &hex::encode(Sha256::digest(plaintext_hex.as_bytes()))[..16]
        );
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_ns();
        let enc = self.encrypt_jwt_secret(plaintext_hex)?;
        tx.execute(
            "INSERT OR IGNORE INTO jwt_keys(kid, is_active, algorithm, secret_enc, source, \
             created_at_ns, retired_at_ns, verify_until_ns) \
             VALUES(?1, 0, 'HS256', ?2, 'imported', ?3, ?3, ?4)",
            params![kid, enc, now, now.saturating_add(JWT_KEY_OVERLAP_NS)],
        )?;
        tx.commit()?;
        self.bump_jwt_generation();
        drop(conn);
        self.audit("system", "jwt_key_import_retired", "jwt_keys", "success");
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
            Ok(_) => {
                self.bump_jwt_generation();
                Ok(secret)
            }
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
    ///
    /// Two queries total (AUTH-R6): one scan of key metadata + one scan of
    /// verifiers, instead of a per-key lookup under repeated lock
    /// acquisitions.
    pub fn authenticate_api_key(&self, raw_key: &str) -> Result<Option<AuthApiKey>> {
        use subtle::ConstantTimeEq;
        let presented = hex::encode(Sha256::digest(raw_key.as_bytes()));
        for (key, stored_hash) in self.api_key_rows()? {
            if !key.enabled {
                continue;
            }
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

    /// All API keys joined with their verifier hashes, in one lock
    /// acquisition. Internal only — `AuthApiKey` never exposes `key_hash`.
    fn api_key_rows(&self) -> Result<Vec<(AuthApiKey, String)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, alias, scope, expires_at_ns, last_used_at_ns, enabled, \
                    created_at_ns, updated_at_ns, key_hash FROM api_keys \
             ORDER BY created_at_ns",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                AuthApiKey {
                    id: r.get(0)?,
                    alias: r.get(1)?,
                    scope: r.get(2)?,
                    expires_at_ns: r.get(3)?,
                    last_used_at_ns: r.get(4)?,
                    enabled: r.get::<_, i64>(5)? != 0,
                    created_at_ns: r.get(6)?,
                    updated_at_ns: r.get(7)?,
                },
                r.get::<_, String>(8)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| anyhow!("api_keys scan failed: {e}"))
    }

    /// Verifier row for migration conflict checks: `(key_hash, scope,
    /// enabled, expires_at_ns)` — internal only.
    fn api_key_verifier_row(&self, id: &str) -> Result<Option<(String, String, bool, i64)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT key_hash, scope, enabled, expires_at_ns FROM api_keys WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? != 0,
                r.get::<_, i64>(3)?,
            ))
        })?;
        match rows.next() {
            Some(row) => row
                .map(Some)
                .map_err(|e| anyhow!("api key verifier lookup failed: {e}")),
            None => Ok(None),
        }
    }

    /// Record a JWT `jti` revocation, keeping the **maximum** expiry
    /// seen (AUTH-R1 monotonic revocation): re-importing an older
    /// revocation must never shorten an existing denylist window.
    /// Returns the effective stored expiry.
    pub fn ensure_jwt_revoked(&self, jti: &str, expires_at_ns: i64) -> Result<i64> {
        let conn = self.lock()?;
        let now = now_ns();
        conn.execute(
            "INSERT INTO jwt_revoked(jti, expires_at_ns, revoked_at_ns) \
             VALUES(?1, ?2, ?3) \
             ON CONFLICT(jti) DO UPDATE SET \
               expires_at_ns = MAX(jwt_revoked.expires_at_ns, excluded.expires_at_ns)",
            params![jti, expires_at_ns, now],
        )?;
        let effective: i64 = conn.query_row(
            "SELECT expires_at_ns FROM jwt_revoked WHERE jti = ?1",
            params![jti],
            |r| r.get(0),
        )?;
        conn.execute(
            "DELETE FROM jwt_revoked WHERE expires_at_ns < ?1",
            params![now],
        )?;
        Ok(effective)
    }

    /// Migration verify helper: stored expiry for a revoked jti
    /// (`None` when absent). Internal.
    fn jwt_revoked_expiry(&self, jti: &str) -> Result<Option<i64>> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT expires_at_ns FROM jwt_revoked WHERE jti = ?1")?;
        let mut rows = stmt.query_map(params![jti], |r| r.get::<_, i64>(0))?;
        match rows.next() {
            Some(row) => row
                .map(Some)
                .map_err(|e| anyhow!("jwt_revoked lookup failed: {e}")),
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
    ///
    /// AUTH-R1/R2 hardening:
    /// - **Validate before scrub.** Every legacy record is parsed and
    ///   cross-checked against existing `AuthStore` rows *before* any
    ///   graph row is touched. Security-relevant conflicts (different
    ///   role/enabled/verifier/scope/expiry for the same identity,
    ///   duplicate verifiers with conflicting identities) abort the
    ///   migration with the graph left intact — never resolved by
    ///   arbitrary first-match.
    /// - **Exact-equivalence verify.** Counts are not sufficient: after
    ///   import, every legacy record is re-read from `AuthStore` and
    ///   compared field-by-field (password verifier, role, enabled;
    ///   key verifier, scope, enabled, expiry; revocation expiry
    ///   monotonicity; JWT secret equality).
    /// - **No unresolved-record loss.** Malformed legacy records are
    ///   preserved verbatim in `auth_quarantine` (inside the protected
    ///   store) *before* their graph rows are scrubbed, so the only copy
    ///   of a potentially recoverable record is never destroyed. Rows
    ///   already tombstoned by a previous run are skipped, so an
    ///   interrupted scrub cannot be mistaken for original records.
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
        let mut legacy_users: Vec<(ConfigItemRecord, LegacyUser)> = Vec::new();
        let mut legacy_keys: Vec<(ConfigItemRecord, LegacyApiKey)> = Vec::new();
        let mut legacy_jwt_secret: Option<String> = None;
        let mut legacy_revocations: Vec<(String, i64)> = Vec::new();
        // Malformed credential-bearing records: preserved for quarantine,
        // never imported, never silently dropped.
        let mut malformed: Vec<(String, ConfigItemRecord, &'static str)> = Vec::new();
        for class in classes {
            let items = store
                .list_config_items(Some(class.to_string()))
                .await
                .with_context(|| format!("migration: failed to list legacy '{class}' items"))?;
            for item in items {
                // Skip rows already tombstoned by an earlier (interrupted)
                // run — they are audit markers, not original records.
                if is_migration_tombstone(&item) {
                    continue;
                }
                match class {
                    "user" => match parse_legacy_user(&item) {
                        Ok(u) => legacy_users.push((item, u)),
                        Err(_) => {
                            report.malformed_records += 1;
                            malformed.push((class.to_string(), item, "unparseable user record"));
                        }
                    },
                    "api_key" => match parse_legacy_api_key(&item) {
                        Ok(k) => legacy_keys.push((item, k)),
                        Err(_) => {
                            report.malformed_records += 1;
                            malformed.push((
                                class.to_string(),
                                item,
                                "unparseable api_key record",
                            ));
                        }
                    },
                    "jwt_secret" => {
                        if item.id == "jwt-secret-v1" && item.enabled {
                            if item.content_json.len() >= 32 {
                                legacy_jwt_secret = Some(item.content_json.clone());
                            } else {
                                report.malformed_records += 1;
                                malformed.push((
                                    class.to_string(),
                                    item,
                                    "jwt_secret record too short",
                                ));
                            }
                        } else if item.enabled {
                            // Any other enabled jwt_secret row is
                            // credential-bearing but not the canonical
                            // record — quarantine, don't guess.
                            report.malformed_records += 1;
                            malformed.push((
                                class.to_string(),
                                item,
                                "non-canonical jwt_secret record",
                            ));
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
                            malformed.push((
                                class.to_string(),
                                item,
                                "jwt_revoked record with unexpected id",
                            ));
                        }
                    }
                    "session" => report.legacy_sessions_seen += 1,
                    _ => {}
                }
            }
        }
        self.set_auth_config("migration_stage", "conflict-check")?;

        // ── conflict check: record-level equivalence against the auth
        // store BEFORE any import or scrub (AUTH-R1). Any conflict aborts
        // with the graph untouched; the operator resolves the conflicting
        // record and migration retries.
        self.check_migration_conflicts(&legacy_users, &legacy_keys)?;

        self.set_auth_config("migration_stage", "import")?;

        // ── import (idempotent: equal records skipped, conflicts already
        // rejected above) ──────────────────────────────────────────────
        for (_item, user) in &legacy_users {
            if self.get_user(&user.username)?.is_none() {
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
        }
        for (_item, key) in &legacy_keys {
            if self.api_key_verifier_row(&key.id)?.is_none() {
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
        }
        for (jti, exp) in &legacy_revocations {
            self.ensure_jwt_revoked(jti, *exp)?;
            report.jwt_revocations_imported += 1;
        }

        // JWT secret: only import when no env override is configured.
        // - No active key → import as the active signing key.
        // - Active key exists → import the legacy secret as a bounded
        //   retired verification key so pre-migration tokens keep
        //   working during the overlap window (never displaces active).
        if std::env::var(JWT_SECRET_ENV).is_err() {
            if let Some(secret) = &legacy_jwt_secret {
                let has_active = {
                    let conn = self.lock()?;
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM jwt_keys WHERE is_active = 1)",
                        [],
                        |r| r.get::<_, bool>(0),
                    )?
                };
                if has_active {
                    self.import_jwt_secret_retired(secret)?;
                } else {
                    // import_jwt_secret encrypts under the vault
                    // passphrase; a missing passphrase fails here —
                    // explicit, resumable.
                    self.import_jwt_secret(secret, false)?;
                    report.jwt_secrets_imported += 1;
                }
            }
        }
        self.set_auth_config("migration_stage", "verify")?;

        // ── verify: exact record-level equivalence, not counts ──────────
        self.verify_migration_equivalence(
            &legacy_users,
            &legacy_keys,
            &legacy_revocations,
            legacy_jwt_secret.as_deref(),
        )?;

        // ── quarantine: preserve malformed records inside the protected
        // store BEFORE scrubbing their graph rows (AUTH-R2). Idempotent
        // via UNIQUE(record_class, item_id).
        self.set_auth_config("migration_stage", "quarantine")?;
        for (class, item, reason) in &malformed {
            let conn = self.lock()?;
            conn.execute(
                "INSERT OR IGNORE INTO auth_quarantine(record_class, item_id, content_json, \
                 reason, quarantined_at_ns) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![class, item.id, item.content_json, reason, now_ns()],
            )?;
            report.quarantined_records += 1;
        }

        // ── scrub legacy graph rows (disable + tombstone, never delete) ──
        // Runs BEFORE the completion marker so a crash mid-scrub re-runs
        // the whole (idempotent) migration and finishes scrubbing every
        // legacy row — no enabled credential row is left behind, and no
        // credential material remains queryable through generic graph
        // APIs. Verified-equivalent records live authoritatively in
        // AuthStore; malformed records are recoverable from
        // auth_quarantine. The graph row becomes an audit tombstone.
        for class in ["user", "api_key", "jwt_secret", "jwt_revoked", "session"] {
            let items = store
                .list_config_items(Some(class.to_string()))
                .await
                .with_context(|| format!("migration quarantine: list '{class}' failed"))?;
            for mut item in items {
                if is_migration_tombstone(&item) {
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
        // Durable manifest of the completed migration (counts + stage
        // history only — never credential material).
        self.set_auth_config(
            "migration_report",
            &serde_json::json!({
                "users_imported": report.users_imported,
                "api_keys_imported": report.api_keys_imported,
                "jwt_secrets_imported": report.jwt_secrets_imported,
                "jwt_revocations_imported": report.jwt_revocations_imported,
                "malformed_records": report.malformed_records,
                "quarantined_rows": report.quarantined_rows,
            })
            .to_string(),
        )?;
        Ok(report)
    }

    /// AUTH-R1: reject the migration when any legacy record conflicts
    /// with an existing `AuthStore` row on security-relevant fields.
    /// Runs before import and before any graph scrub, so a conflict
    /// leaves both stores untouched and retryable.
    fn check_migration_conflicts(
        &self,
        legacy_users: &[(ConfigItemRecord, LegacyUser)],
        legacy_keys: &[(ConfigItemRecord, LegacyApiKey)],
    ) -> Result<()> {
        // Users: same username must carry identical verifier, role and
        // enabled state. Duplicate legacy usernames must agree with each
        // other first — first-match resolution is never allowed.
        let mut seen_users: std::collections::HashMap<&str, &LegacyUser> =
            std::collections::HashMap::new();
        for (_item, user) in legacy_users {
            if let Some(prev) = seen_users.insert(user.username.as_str(), user) {
                if prev.password_hash != user.password_hash
                    || prev.role != user.role
                    || prev.enabled != user.enabled
                {
                    bail!(
                        "migration conflict: duplicate legacy user '{}' with differing \
                         credentials; resolve manually before retrying",
                        user.username
                    );
                }
            }
            if let Some(existing) = self.get_user(&user.username)? {
                if existing.password_hash != user.password_hash
                    || existing.role != user.role
                    || existing.enabled != user.enabled
                {
                    bail!(
                        "migration conflict: user '{}' exists in auth store with different \
                         credentials (role/enabled/verifier mismatch)",
                        user.username
                    );
                }
            }
        }

        // API keys: identity is the row id; the verifier (key_hash) binds
        // the raw credential. Same id + different verifier/scope/enabled/
        // expiry is a conflict. The same verifier under two different
        // ids is an ambiguous identity — also a conflict.
        let mut seen_ids: std::collections::HashMap<&str, &LegacyApiKey> =
            std::collections::HashMap::new();
        let mut seen_hashes: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for (_item, key) in legacy_keys {
            if let Some(prev) = seen_ids.insert(key.id.as_str(), key) {
                if prev.key_hash != key.key_hash
                    || prev.scope != key.scope
                    || prev.enabled != key.enabled
                    || prev.expires_at_ns != key.expires_at_ns
                {
                    bail!(
                        "migration conflict: duplicate legacy api_key '{}' with differing \
                         verifier/scope/enabled/expiry",
                        key.id
                    );
                }
            }
            if let Some(prev_id) = seen_hashes.insert(key.key_hash.as_str(), key.id.as_str()) {
                if prev_id != key.id {
                    bail!(
                        "migration conflict: api key verifier is bound to two different \
                         identities ('{prev_id}' and '{}'); refusing arbitrary resolution",
                        key.id
                    );
                }
            }
            if let Some((hash, scope, enabled, expires)) = self.api_key_verifier_row(&key.id)? {
                if hash != key.key_hash
                    || scope != key.scope
                    || enabled != key.enabled
                    || expires != key.expires_at_ns
                {
                    bail!(
                        "migration conflict: api_key '{}' exists in auth store with \
                         different verifier/scope/enabled/expiry",
                        key.id
                    );
                }
            } else {
                // No row with this id — but the same verifier may already
                // exist under a different id in the store.
                let clash = self
                    .api_key_rows()?
                    .iter()
                    .any(|(k, h)| h == &key.key_hash && k.id != key.id);
                if clash {
                    bail!(
                        "migration conflict: api key verifier already bound to a \
                         different identity in auth store"
                    );
                }
            }
        }
        Ok(())
    }

    /// AUTH-R1: after import, prove record-level equivalence — every
    /// legacy user/key/revocation/JWT secret must be present in
    /// `AuthStore` with identical security-relevant fields. Counts are
    /// never accepted as evidence.
    fn verify_migration_equivalence(
        &self,
        legacy_users: &[(ConfigItemRecord, LegacyUser)],
        legacy_keys: &[(ConfigItemRecord, LegacyApiKey)],
        legacy_revocations: &[(String, i64)],
        legacy_jwt_secret: Option<&str>,
    ) -> Result<()> {
        for (_item, user) in legacy_users {
            let stored = self
                .get_user(&user.username)?
                .ok_or_else(|| anyhow!("migration verify: user '{}' missing after import", user.username))?;
            if stored.password_hash != user.password_hash
                || stored.role != user.role
                || stored.enabled != user.enabled
            {
                bail!(
                    "migration verify failed: user '{}' security fields differ after import",
                    user.username
                );
            }
        }
        for (_item, key) in legacy_keys {
            let Some((hash, scope, enabled, expires)) = self.api_key_verifier_row(&key.id)? else {
                bail!("migration verify: api_key '{}' missing after import", key.id);
            };
            if hash != key.key_hash
                || scope != key.scope
                || enabled != key.enabled
                || expires != key.expires_at_ns
            {
                bail!(
                    "migration verify failed: api_key '{}' fields differ after import",
                    key.id
                );
            }
        }
        for (jti, exp) in legacy_revocations {
            let stored = self.jwt_revoked_expiry(jti)?;
            match stored {
                Some(se) if se >= *exp => {}
                _ => bail!(
                    "migration verify failed: revocation '{jti}' missing or expiry shortened"
                ),
            }
        }
        if let Some(secret) = legacy_jwt_secret {
            if std::env::var(JWT_SECRET_ENV).is_err() {
                let candidates = self.jwt_verify_candidates()?;
                if !candidates.iter().any(|c| c == secret) {
                    bail!(
                        "migration verify failed: legacy jwt secret not verifiable after import"
                    );
                }
            }
        }
        // Malformed records must all be quarantined before their graph
        // rows may be scrubbed — verified by count against the detect
        // phase (the caller passes the same list length implicitly via
        // report.malformed_records; the quarantine insert is INSERT OR
        // IGNORE so retries cannot duplicate).
        Ok(())
    }
}

/// True when a graph ConfigItem is already a migration tombstone
/// (disabled + scrubbed content). Such rows are audit markers, never
/// original credential records — skipping them is what makes an
/// interrupted scrub safe to resume (AUTH-R2).
fn is_migration_tombstone(item: &ConfigItemRecord) -> bool {
    !item.enabled
        && item
            .content_json
            .contains("\"migrated_to\":\"auth_store\"")
}

// ── Protected backup / recovery (WP4) ────────────────────────────────────────

/// Manifest written beside an auth-store backup snapshot. Records what a
/// restore needs WITHOUT containing any secret material.
#[derive(Debug, Serialize)]
pub struct AuthBackupManifest {
    pub format: String,
    pub schema_version: i64,
    pub created_at_ns: i64,
    /// Where the at-rest key lives: `env` (operator-held passphrase) or
    /// `keyfile` (must be backed up separately — never embedded here).
    pub key_source: String,
    pub migration_state: Option<String>,
    pub user_count: usize,
    pub api_key_count: usize,
    pub jwt_key_count: usize,
    pub jwt_revoked_count: usize,
    pub quarantined_record_count: usize,
}

impl AuthStore {
    /// Create a consistent snapshot of the auth database at `dest_path`
    /// using SQLite `VACUUM INTO` (online backup — safe while the store
    /// is open), plus a sidecar `<dest>.manifest.json`.
    ///
    /// The snapshot contains only verifiers, encrypted JWT key material,
    /// revocation state and metadata. **The at-rest key is NOT included:**
    /// recovery requires either `BONSAI_VAULT_PASSPHRASE` (preferred,
    /// operator secret store) or the `bonsai_auth.db.key` keyfile backed up
    /// separately. A graph `/api/db/backup` alone is NOT a complete
    /// disaster-recovery backup — see docs/auth_storage.md §5.
    ///
    /// `dest_path` must not already exist. The snapshot is created
    /// owner-only (0600).
    pub fn backup_to(&self, dest_path: &Path) -> Result<AuthBackupManifest> {
        if dest_path.exists() {
            bail!(
                "auth backup destination '{}' already exists",
                dest_path.display()
            );
        }
        if let Some(parent) = dest_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        {
            let conn = self.lock()?;
            // VACUUM INTO cannot bind the path as a parameter on all
            // SQLite versions; quote it safely.
            let quoted = dest_path.to_string_lossy().replace('\'', "''");
            conn.execute_batch(&format!("VACUUM INTO '{quoted}'"))
                .with_context(|| {
                    format!("auth backup VACUUM INTO '{}' failed", dest_path.display())
                })?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if dest_path.exists() {
                std::fs::set_permissions(dest_path, std::fs::Permissions::from_mode(0o600))?;
            }
        }

        let conn = self.lock()?;
        let count = |sql: &str| -> Result<usize> {
            conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                .map(|n| n as usize)
                .with_context(|| format!("backup manifest count failed: {sql}"))
        };
        let manifest = AuthBackupManifest {
            format: "bonsai-auth-backup-v1".to_string(),
            schema_version: SCHEMA_VERSION,
            created_at_ns: now_ns(),
            key_source: if std::env::var(VAULT_PASSPHRASE_ENV).is_ok_and(|v| !v.is_empty()) {
                "env:BONSAI_VAULT_PASSPHRASE".to_string()
            } else {
                "keyfile".to_string()
            },
            migration_state: self.get_auth_config("migration_state")?,
            user_count: count("SELECT COUNT(*) FROM users")?,
            api_key_count: count("SELECT COUNT(*) FROM api_keys")?,
            jwt_key_count: count("SELECT COUNT(*) FROM jwt_keys")?,
            jwt_revoked_count: count("SELECT COUNT(*) FROM jwt_revoked")?,
            quarantined_record_count: count("SELECT COUNT(*) FROM auth_quarantine")?,
        };
        drop(conn);

        let manifest_path = dest_path.with_extension("manifest.json");
        let json = serde_json::to_string_pretty(&manifest)?;
        create_owner_only_file(&manifest_path)?;
        std::fs::write(&manifest_path, format!("{json}\n"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o600))?;
        }
        self.audit("system", "auth_backup", dest_path.to_string_lossy().as_ref(), "success");
        Ok(manifest)
    }

    /// Number of records preserved in `auth_quarantine` (operator
    /// disposition aid — contents are never exposed, only the count).
    pub fn quarantined_record_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        conn.query_row("SELECT COUNT(*) FROM auth_quarantine", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n as usize)
        .map_err(|e| anyhow!("auth_quarantine count failed: {e}"))
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
/// `BONSAI_VAULT_PASSPHRASE` wins when set (external key ownership).
/// Otherwise a random keyfile `<auth.db>.key` is created or read.
///
/// Returns `(key, created_new)`. `created_new` lets `AuthStore::open`
/// detect the "keyfile lost but encrypted JWT keys remain" case and fail
/// closed instead of silently establishing a divergent encryption
/// identity (AUTH-R4).
///
/// Creation is race-safe: the key is written to a uniquely-named temp
/// file created with mode 0600 *at creation time*, then installed via
/// `hard_link`, which fails if the destination already exists. Two
/// concurrent first starts therefore cannot install different keys —
/// exactly one link wins and the loser reads the winner's file.
fn resolve_secret_key(auth_db_path: &Path) -> Result<(age::secrecy::SecretString, bool)> {
    if let Ok(pass) = std::env::var(VAULT_PASSPHRASE_ENV) {
        if !pass.is_empty() {
            return Ok((age::secrecy::SecretString::new(pass.into()), false));
        }
    }
    let key_path = auth_db_path.with_extension("db.key");
    match std::fs::symlink_metadata(&key_path) {
        Ok(md) => {
            // Reject symlinks and foreign-owned files: the keyfile must
            // be a regular file owned by the service identity.
            if md.file_type().is_symlink() {
                bail!(
                    "auth keyfile '{}' is a symlink; refusing to use it",
                    key_path.display()
                );
            }
            if !md.file_type().is_file() {
                bail!(
                    "auth keyfile '{}' is not a regular file",
                    key_path.display()
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let euid = unsafe { libc::geteuid() };
                if md.uid() != euid {
                    bail!(
                        "auth keyfile '{}' is owned by uid {} but the service \
                         runs as uid {euid}; refusing to use it",
                        key_path.display(),
                        md.uid()
                    );
                }
            }
            // Tighten permissions on the existing file (idempotent).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| {
                        format!("failed to restrict '{}'", key_path.display())
                    })?;
            }
            let raw = std::fs::read_to_string(&key_path)
                .with_context(|| format!("failed to read auth keyfile '{}'", key_path.display()))?;
            let trimmed = raw.trim();
            if trimmed.len() < 32 {
                bail!(
                    "auth keyfile '{}' is too short; regenerate it and rotate the JWT key",
                    key_path.display()
                );
            }
            return Ok((
                age::secrecy::SecretString::new(trimmed.to_string().into()),
                false,
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                format!("failed to stat auth keyfile '{}'", key_path.display())
            })
        }
    }

    // Generate a fresh key and install it race-safely.
    let mut bytes = [0u8; 32];
    {
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut bytes);
    }
    let key = hex::encode(bytes);
    let tmp = unique_keyfile_tmp(&key_path)?;
    // Write + fsync-ish close, then set 0600 BEFORE the file becomes
    // visible under its final name. OpenOptions mode applies at creation.
    {
        use std::io::Write;
        let mut f = open_owner_only_new(&tmp)?;
        f.write_all(format!("{key}\n").as_bytes())
            .with_context(|| format!("failed to write auth keyfile tmp '{}'", tmp.display()))?;
    }
    // hard_link fails with EEXIST when another process won the race —
    // that process's key is authoritative; read it back instead.
    match std::fs::hard_link(&tmp, &key_path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok((age::secrecy::SecretString::new(key.into()), true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp);
            let raw = std::fs::read_to_string(&key_path).with_context(|| {
                format!("failed to read winning auth keyfile '{}'", key_path.display())
            })?;
            let trimmed = raw.trim();
            if trimmed.len() < 32 {
                bail!(
                    "auth keyfile '{}' is too short after concurrent creation",
                    key_path.display()
                );
            }
            Ok((
                age::secrecy::SecretString::new(trimmed.to_string().into()),
                false,
            ))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e).with_context(|| {
                format!("failed to install auth keyfile '{}'", key_path.display())
            })
        }
    }
}

/// Unique temp path for keyfile staging, created with O_EXCL semantics
/// via `create_new` inside `open_owner_only_new`.
fn unique_keyfile_tmp(key_path: &Path) -> Result<PathBuf> {
    for i in 0..100u32 {
        let candidate = key_path.with_extension(format!(
            "key.tmp.{}.{}",
            std::process::id(),
            i
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("could not allocate auth keyfile temp path")
}

/// Create `path` with mode 0600 at creation time (Unix). Fails if the
/// file already exists.
fn open_owner_only_new(path: &Path) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .with_context(|| format!("failed to create owner-only file '{}'", path.display()))
}

/// Create an empty file with mode 0600 at creation time (Unix).
fn create_owner_only_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let _f = open_owner_only_new(path)?;
    Ok(())
}

/// Read `schema_meta.schema_version`; `None` when absent (fresh or
/// partially-initialized store).
fn read_schema_version(conn: &Connection) -> Result<Option<i64>> {
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_meta')",
        [],
        |r| r.get(0),
    )?;
    if !table_exists {
        return Ok(None);
    }
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match version {
        Some(v) => v
            .parse::<i64>()
            .map(Some)
            .with_context(|| format!("auth store schema_version '{v}' is not an integer")),
        None => Ok(None),
    }
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
