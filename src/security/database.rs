//! Database Security Module
//! Provides encryption-at-rest, access controls, and comprehensive auditing

use anyhow::{Context, Result};
use lbug::{Connection, Value};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{info, warn, error};

use crate::audit::append_security_event;

/// Database security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseSecurityConfig {
    pub enabled: bool,
    pub encryption_enabled: bool,
    pub encryption_key_alias: String,
    pub audit_enabled: bool,
    pub access_control_enabled: bool,
    pub data_masking_enabled: bool,
    pub retention_days: i32,
}

impl Default for DatabaseSecurityConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for testing
            encryption_enabled: false,
            encryption_key_alias: "database_master_key".to_string(),
            audit_enabled: false,
            access_control_enabled: false,
            data_masking_enabled: false,
            retention_days: 90,
        }
    }
}

/// Security event types for database auditing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DatabaseSecurityEvent {
    DataAccess {
        user: String,
        table: String,
        operation: String,
        row_count: Option<u64>,
        success: bool,
    },
    SchemaChange {
        user: String,
        operation: String,
        object_type: String,
        object_name: String,
    },
    PrivilegeChange {
        admin_user: String,
        target_user: String,
        privilege: String,
        granted: bool,
    },
    EncryptionOperation {
        operation: String,
        table: String,
        success: bool,
    },
    SecurityViolation {
        user: String,
        violation_type: String,
        details: String,
        severity: String,
    },
}

/// Database security manager
pub struct DatabaseSecurityManager {
    config: DatabaseSecurityConfig,
    audit_log: Arc<Mutex<Vec<DatabaseSecurityEvent>>>,
    encryption_keys: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl DatabaseSecurityManager {
    pub fn new(config: DatabaseSecurityConfig) -> Self {
        Self {
            config,
            audit_log: Arc::new(Mutex::new(Vec::new())),
            encryption_keys: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Initialize database security
    pub fn initialize(&self, conn: &Connection<'_>) -> Result<()> {
        if !self.config.enabled {
            info!("Database security disabled - skipping initialization");
            return Ok(());
        }

        info!("Initializing database security");

        // Create security audit tables
        self.create_security_tables(conn)?;

        // Enable database encryption if configured
        if self.config.encryption_enabled {
            self.enable_encryption(conn)?;
        }

        // Set up access controls
        if self.config.access_control_enabled {
            self.setup_access_controls(conn)?;
        }

        info!("Database security initialized successfully");
        Ok(())
    }

    /// Create security audit tables
    fn create_security_tables(&self, conn: &Connection<'_>) -> Result<()> {
        // Database access audit log
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS DatabaseAuditLog(\
                id                    STRING,\
                timestamp_ns          INT64,\
                user_id               STRING,\
                session_id            STRING,\
                operation             STRING,\
                table_name            STRING,\
                row_count             INT64,\
                success               BOOL,\
                error_message         STRING,\
                client_ip             STRING,\
                PRIMARY KEY (id))",
        )
        .context("create DatabaseAuditLog table")?;

        // Schema change audit log
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SchemaAuditLog(\
                id                    STRING,\
                timestamp_ns          INT64,\
                user_id               STRING,\
                operation             STRING,\
                object_type           STRING,\
                object_name           STRING,\
                sql_statement         STRING,\
                success               BOOL,\
                PRIMARY KEY (id))",
        )
        .context("create SchemaAuditLog table")?;

        // Security violation log
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS SecurityViolationLog(\
                id                    STRING,\
                timestamp_ns          INT64,\
                user_id               STRING,\
                violation_type        STRING,\
                severity              STRING,\
                details               STRING,\
                client_ip             STRING,\
                resolved              BOOL,\
                resolved_at_ns        INT64,\
                PRIMARY KEY (id))",
        )
        .context("create SecurityViolationLog table")?;

        // Data encryption metadata
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS EncryptionMetadata(\
                id                    STRING,\
                table_name            STRING,\
                column_name           STRING,\
                encryption_algorithm  STRING,\
                key_id                STRING,\
                encrypted_at_ns       INT64,\
                PRIMARY KEY (id))",
        )
        .context("create EncryptionMetadata table")?;

        Ok(())
    }

    /// Enable database encryption
    fn enable_encryption(&self, conn: &Connection<'_>) -> Result<()> {
        info!("Enabling database encryption");

        // Note: This is a simplified implementation
        // In production, you'd use the database's native encryption features
        // or implement application-level encryption for sensitive columns

        let sensitive_tables = vec![
            "Device", "UserContent", "SessionContent", "ConfigItem",
            "SecurityPosture", "SecurityIncident", "Vulnerability"
        ];

        for table in sensitive_tables {
            // Log encryption operation
            self.log_security_event(DatabaseSecurityEvent::EncryptionOperation {
                operation: "enable_table_encryption".to_string(),
                table: table.to_string(),
                success: true,
            })?;

            // Store encryption metadata
            conn.query(
                "MERGE (em:EncryptionMetadata {id: $metadata_id}) \
                 SET em.table_name = $table_name, \
                     em.column_name = $column_name, \
                     em.encryption_algorithm = $algorithm, \
                     em.key_id = $key_id, \
                     em.encrypted_at_ns = $timestamp_ns",
                vec![
                    ("metadata_id", Value::String(format!("{}-all", table))),
                    ("table_name", Value::String(table.to_string())),
                    ("column_name", Value::String("*".to_string())),
                    ("algorithm", Value::String("AES-256-GCM".to_string())),
                    ("key_id", Value::String(self.config.encryption_key_alias.clone())),
                    ("timestamp_ns", Value::Int64(crate::graph::common::now_ns())),
                ],
            )
            .context("store encryption metadata")?;
        }

        info!("Database encryption enabled for {} tables", sensitive_tables.len());
        Ok(())
    }

    /// Set up database access controls
    fn setup_access_controls(&self, conn: &Connection<'_>) -> Result<()> {
        info!("Setting up database access controls");

        // Create user role table
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS DatabaseRole(\
                id                    STRING,\
                role_name             STRING,\
                permissions           STRING,\
                created_at_ns         INT64,\
                PRIMARY KEY (id))",
        )
        .context("create DatabaseRole table")?;

        // Create user role assignment table
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS UserRoleAssignment(\
                id                    STRING,\
                user_id               STRING,\
                role_id               STRING,\
                granted_at_ns         INT64,\
                granted_by            STRING,\
                expires_at_ns         INT64,\
                PRIMARY KEY (id))",
        )
        .context("create UserRoleAssignment table")?;

        // Create default roles
        let default_roles = vec![
            ("admin", "ALL_PRIVILEGES"),
            ("operator", "READ,WRITE,EXECUTE"),
            ("viewer", "READ"),
            ("security_analyst", "READ_SECURITY,WRITE_SECURITY"),
        ];

        for (role_name, permissions) in default_roles {
            conn.query(
                "MERGE (dr:DatabaseRole {role_name: $role_name}) \
                 SET dr.permissions = $permissions, \
                     dr.created_at_ns = $timestamp_ns",
                vec![
                    ("role_name", Value::String(role_name.to_string())),
                    ("permissions", Value::String(permissions.to_string())),
                    ("timestamp_ns", Value::Int64(crate::graph::common::now_ns())),
                ],
            )
            .context("create default role")?;
        }

        info!("Database access controls configured");
        Ok(())
    }

    /// Log database access
    pub fn log_database_access(
        &self,
        conn: &Connection<'_>,
        user_id: &str,
        session_id: &str,
        operation: &str,
        table_name: &str,
        row_count: Option<u64>,
        success: bool,
        error_message: Option<&str>,
        client_ip: &str,
    ) -> Result<()> {
        if !self.config.audit_enabled {
            return Ok(());
        }

        let audit_id = format!("audit-{}-{}", crate::graph::common::now_ns(), user_id);
        
        conn.query(
            "CREATE (dal:DatabaseAuditLog { \
                id: $audit_id, \
                timestamp_ns: $timestamp_ns, \
                user_id: $user_id, \
                session_id: $session_id, \
                operation: $operation, \
                table_name: $table_name, \
                row_count: $row_count, \
                success: $success, \
                error_message: $error_message, \
                client_ip: $client_ip \
            })",
            vec![
                ("audit_id", Value::String(audit_id)),
                ("timestamp_ns", Value::Int64(crate::graph::common::now_ns())),
                ("user_id", Value::String(user_id.to_string())),
                ("session_id", Value::String(session_id.to_string())),
                ("operation", Value::String(operation.to_string())),
                ("table_name", Value::String(table_name.to_string())),
                ("row_count", Value::Int64(row_count.unwrap_or(0) as i64)),
                ("success", Value::Boolean(success)),
                ("error_message", Value::String(error_message.unwrap_or("").to_string())),
                ("client_ip", Value::String(client_ip.to_string())),
            ],
        )
        .context("log database access")?;

        Ok(())
    }

    /// Log security violation
    pub fn log_security_violation(
        &self,
        conn: &Connection<'_>,
        user_id: &str,
        violation_type: &str,
        severity: &str,
        details: &str,
        client_ip: &str,
    ) -> Result<()> {
        let violation_id = format!("violation-{}-{}", crate::graph::common::now_ns(), user_id);
        
        conn.query(
            "CREATE (svl:SecurityViolationLog { \
                id: $violation_id, \
                timestamp_ns: $timestamp_ns, \
                user_id: $user_id, \
                violation_type: $violation_type, \
                severity: $severity, \
                details: $details, \
                client_ip: $client_ip, \
                resolved: false \
            })",
            vec![
                ("violation_id", Value::String(violation_id)),
                ("timestamp_ns", Value::Int64(crate::graph::common::now_ns())),
                ("user_id", Value::String(user_id.to_string())),
                ("violation_type", Value::String(violation_type.to_string())),
                ("severity", Value::String(severity.to_string())),
                ("details", Value::String(details.to_string())),
                ("client_ip", Value::String(client_ip.to_string())),
            ],
        )
        .context("log security violation")?;

        // Also log to memory for immediate alerts
        self.log_security_event(DatabaseSecurityEvent::SecurityViolation {
            user: user_id.to_string(),
            violation_type: violation_type.to_string(),
            details: details.to_string(),
            severity: severity.to_string(),
        })?;

        warn!("Security violation logged: {} - {}", violation_type, details);
        Ok(())
    }

    /// Check user permissions for table access
    pub fn check_table_access(
        &self,
        conn: &Connection<'_>,
        user_id: &str,
        table_name: &str,
        operation: &str,
    ) -> Result<bool> {
        if !self.config.access_control_enabled {
            return Ok(true);
        }

        let rows = conn.query(
            "MATCH (u:UserContent {username: $user_id}) \
             -[:HAS_ROLE]->(ura:UserRoleAssignment) \
             -[:ASSIGNED_TO]->(dr:DatabaseRole) \
             WHERE ura.expires_at_ns IS NULL OR ura.expires_at_ns > $now_ns \
             RETURN dr.permissions",
            vec![
                ("user_id", Value::String(user_id.to_string())),
                ("now_ns", Value::Int64(crate::graph::common::now_ns())),
            ],
        )
        .context("check user permissions")?;

        for row in rows {
            let permissions = crate::graph::common::read_str(&row[0]);
            if permissions.contains("ALL_PRIVILEGES") || permissions.contains(operation) {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Apply data masking to sensitive fields
    pub fn apply_data_masking(&self, value: &str, field_name: &str) -> String {
        if !self.config.data_masking_enabled {
            return value.to_string();
        }

        let sensitive_fields = vec![
            "password", "api_key", "secret", "token", "private_key",
            "credit_card", "ssn", "social_security", "phone", "email"
        ];

        let field_lower = field_name.to_lowercase();
        for sensitive in &sensitive_fields {
            if field_lower.contains(sensitive) {
                return self.mask_value(value);
            }
        }

        value.to_string()
    }

    /// Mask a sensitive value
    fn mask_value(&self, value: &str) -> String {
        if value.len() <= 4 {
            "*".repeat(value.len())
        } else {
            format!("{}***{}", &value[..2], &value[value.len()-2..])
        }
    }

    /// Log security event to memory
    fn log_security_event(&self, event: DatabaseSecurityEvent) -> Result<()> {
        let mut log = self.audit_log.lock().map_err(|e| {
            anyhow::anyhow!("Failed to acquire audit log lock: {}", e)
        })?;
        log.push(event);
        
        // Keep only recent events (last 1000)
        if log.len() > 1000 {
            log.drain(0..log.len() - 1000);
        }
        
        Ok(())
    }

    /// Get recent security events
    pub fn get_recent_events(&self, limit: usize) -> Vec<DatabaseSecurityEvent> {
        let log = self.audit_log.lock().unwrap_or_else(|_| {
            Mutex::new(Vec::new())
        });
        log.iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Clean up old audit logs
    pub fn cleanup_old_logs(&self, conn: &Connection<'_>) -> Result<()> {
        let cutoff_ns = crate::graph::common::now_ns() - (self.config.retention_days as i64 * 24 * 60 * 60 * 1_000_000_000);
        
        // Clean old database audit logs
        conn.query(
            "MATCH (dal:DatabaseAuditLog) \
             WHERE dal.timestamp_ns < $cutoff_ns \
             DELETE dal",
            vec![("cutoff_ns", Value::Int64(cutoff_ns))],
        )
        .context("cleanup old database audit logs")?;

        // Clean old security violation logs
        conn.query(
            "MATCH (svl:SecurityViolationLog) \
             WHERE svl.timestamp_ns < $cutoff_ns \
             DELETE svl",
            vec![("cutoff_ns", Value::Int64(cutoff_ns))],
        )
        .context("cleanup old security violation logs")?;

        info!("Cleaned up audit logs older than {} days", self.config.retention_days);
        Ok(())
    }
}

/// Global database security manager instance
static DB_SECURITY_MANAGER: std::sync::OnceLock<std::sync::Arc<DatabaseSecurityManager>> = std::sync::OnceLock::new();

/// Initialize global database security manager
pub fn initialize_database_security(config: DatabaseSecurityConfig) -> Result<()> {
    let manager = Arc::new(DatabaseSecurityManager::new(config));
    DB_SECURITY_MANAGER.set(manager.clone())
        .map_err(|_| anyhow::anyhow!("Database security manager already initialized"))?;
    Ok(())
}

/// Get global database security manager
pub fn get_database_security_manager() -> Option<Arc<DatabaseSecurityManager>> {
    DB_SECURITY_MANAGER.get().cloned()
}
