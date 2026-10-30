//! Persisted inventory ownership. Audit-only: these records do not grant or deny access.
use super::{Connection, Context, GraphStore, Result, Value, read_str};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Temporary compatibility policy requested for the ownership rollout.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipAccessMode {
    AuditOnly,
    Strict,
}

pub(super) fn init_schema(conn: &Connection<'_>) -> Result<()> {
    conn.query(
        "CREATE NODE TABLE IF NOT EXISTS OwnershipBinding(\
         resource_key STRING, speaker STRING, vrf_rd STRING, tenant_id STRING,\
         updated_by STRING, updated_at_ns INT64, PRIMARY KEY(resource_key))",
    )?;
    Ok(())
}

/// A device or one routing instance on a specific exporter. No implicit inheritance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnershipResource {
    pub speaker: String,
    #[serde(default)]
    pub vrf_rd: Option<String>,
}

fn validate_label(name: &str, value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control),
        "{name} must be 1–256 bytes, trimmed, without control characters"
    );
    Ok(())
}

impl OwnershipResource {
    pub fn validate(&self) -> Result<()> {
        validate_label("speaker", &self.speaker)?;
        if let Some(rd) = &self.vrf_rd {
            validate_label("vrf_rd", rd)?;
        }
        Ok(())
    }

    fn key(&self) -> Result<String> {
        self.validate()?;
        // Tuple encoding distinguishes absent/empty RD and delimiter-containing names.
        Ok(serde_json::to_string(&(&self.speaker, &self.vrf_rd))?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnershipAssignment {
    pub resource: OwnershipResource,
    pub tenant_id: String,
}

impl OwnershipAssignment {
    pub fn validate(&self) -> Result<()> {
        self.resource.validate()?;
        validate_label("tenant_id", &self.tenant_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnershipBinding {
    #[serde(flatten)]
    pub assignment: OwnershipAssignment,
    pub updated_by: String,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipStatus {
    Matched,
    Mismatched,
    Unmapped,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OwnershipAudit {
    pub mode: String,
    /// Always false in audit mode. This result is not an access authorization.
    pub enforcement_enabled: bool,
    pub status: OwnershipStatus,
    pub expected_tenant_id: String,
    pub resource: OwnershipResource,
    pub binding: Option<OwnershipBinding>,
}

impl GraphStore {
    /// An absent setting enables the explicitly requested compatibility rollout.
    /// Invalid configuration or storage failure retains the strict authorization contract.
    pub async fn ownership_access_mode(&self) -> OwnershipAccessMode {
        match self.list_config_items(Some("app_config".into())).await {
            Ok(items) => {
                let mut matches = items
                    .iter()
                    .filter(|i| i.enabled && i.name == "ownership_access_mode");
                let first = matches.next();
                if matches.next().is_some() {
                    return OwnershipAccessMode::Strict;
                }
                match first {
                    None => OwnershipAccessMode::AuditOnly,
                    Some(item) => match item.content_json.trim().trim_matches('"') {
                        "audit_only" => OwnershipAccessMode::AuditOnly,
                        "strict" => OwnershipAccessMode::Strict,
                        _ => OwnershipAccessMode::Strict,
                    },
                }
            }
            Err(_) => OwnershipAccessMode::Strict,
        }
    }

    /// Administrative assignment/reassignment; durable and serialized with other graph writes.
    /// No Device node is fabricated and no ownership is inferred from a token or live packet.
    pub async fn assign_ownership(
        &self,
        assignment: OwnershipAssignment,
        updated_by: String,
    ) -> Result<OwnershipBinding> {
        assignment.validate()?;
        validate_label("updated_by", &updated_by)?;
        let key = assignment.resource.key()?;
        let record = OwnershipBinding {
            assignment,
            updated_by,
            updated_at_ns: super::common::now_ns(),
        };
        let result = record.clone();
        let db = Arc::clone(&self.db);
        let lock = Arc::clone(&self.write_lock);
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _guard = lock
                .lock()
                .map_err(|_| anyhow::anyhow!("write lock poisoned"))?;
            let conn = Connection::new(&db)?;
            conn.set_query_timeout(5000);
            let mut stmt = conn.prepare(
                "MERGE (o:OwnershipBinding {resource_key: $key}) \
                 SET o.speaker = $speaker, o.vrf_rd = $rd, o.tenant_id = $tenant,\
                     o.updated_by = $actor, o.updated_at_ns = $at",
            )?;
            conn.execute(
                &mut stmt,
                vec![
                    ("key", Value::String(key)),
                    ("speaker", Value::String(record.assignment.resource.speaker)),
                    (
                        "rd",
                        record
                            .assignment
                            .resource
                            .vrf_rd
                            .map(Value::String)
                            .unwrap_or(Value::Null(lbug::LogicalType::String)),
                    ),
                    ("tenant", Value::String(record.assignment.tenant_id)),
                    ("actor", Value::String(record.updated_by)),
                    ("at", Value::Int64(record.updated_at_ns)),
                ],
            )?;
            Ok(())
        })
        .await
        .context("assign ownership worker")??;
        Ok(result)
    }

    /// Bounded, stable-key pagination. Fetch limit+1 to expose continuation truthfully.
    pub async fn list_ownership(
        &self,
        after_key: String,
        limit: usize,
    ) -> Result<Vec<(String, OwnershipBinding)>> {
        anyhow::ensure!((1..=501).contains(&limit), "ownership limit must be 1–501");
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || -> Result<_> {
            let conn = Connection::new(&db)?;
            conn.set_query_timeout(5000);
            let mut stmt = conn.prepare(&format!(
                "MATCH (o:OwnershipBinding) WHERE o.resource_key > $after \
                 RETURN o.resource_key, o.speaker, o.vrf_rd, o.tenant_id, o.updated_by, o.updated_at_ns \
                 ORDER BY o.resource_key LIMIT {limit}"
            ))?;
            let rows = conn.execute(&mut stmt, vec![("after", Value::String(after_key))])?;
            rows.map(|row| Ok((read_str(&row[0]), decode_binding(&row[1..])?))).collect()
        }).await.context("list ownership worker")?
    }

    pub async fn audit_ownership(&self, expected: OwnershipAssignment) -> Result<OwnershipAudit> {
        expected.validate()?;
        let key = expected.resource.key()?;
        let db = Arc::clone(&self.db);
        let found = tokio::task::spawn_blocking(move || -> Result<_> {
            let conn = Connection::new(&db)?;
            conn.set_query_timeout(5000);
            let mut stmt = conn.prepare(
                "MATCH (o:OwnershipBinding) WHERE o.resource_key = $key \
                 RETURN o.speaker, o.vrf_rd, o.tenant_id, o.updated_by, o.updated_at_ns",
            )?;
            let mut rows = conn.execute(&mut stmt, vec![("key", Value::String(key))])?;
            rows.next().map(|row| decode_binding(&row)).transpose()
        })
        .await
        .context("audit ownership worker")??;
        let status = match &found {
            None => OwnershipStatus::Unmapped,
            Some(b) if b.assignment.tenant_id == expected.tenant_id => OwnershipStatus::Matched,
            Some(_) => OwnershipStatus::Mismatched,
        };
        Ok(OwnershipAudit {
            mode: "audit_only".into(),
            enforcement_enabled: false,
            status,
            expected_tenant_id: expected.tenant_id,
            resource: expected.resource,
            binding: found,
        })
    }
}

fn decode_binding(row: &[Value]) -> Result<OwnershipBinding> {
    Ok(OwnershipBinding {
        assignment: OwnershipAssignment {
            resource: OwnershipResource {
                speaker: read_str(&row[0]),
                vrf_rd: match &row[1] {
                    Value::Null(_) => None,
                    v => Some(read_str(v)),
                },
            },
            tenant_id: read_str(&row[2]),
        },
        updated_by: read_str(&row[3]),
        updated_at_ns: match row[4] {
            Value::Int64(n) => n,
            _ => anyhow::bail!("invalid ownership timestamp"),
        },
    })
}
