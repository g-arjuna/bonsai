#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── Device detail endpoint ────────────────────────────────────────────────────

async fn device_detail_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceDetailResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    let db = state.store.db();
    let addr_clone = address.clone();

    let (ifaces, bgp, lldp, state_changes, detections) = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare(
                "MATCH (i:Interface) WHERE i.device_address = $addr \
                     RETURN i.name, i.in_errors, i.out_errors, i.in_octets, i.out_octets, \
                            i.carrier_transitions, i.updated_at \
                     ORDER BY i.name",
            )
            .map_err(|e| e.to_string())?;
        let iface_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let ifaces: Vec<InterfaceDetailJson> = iface_rows
            .map(|row| InterfaceDetailJson {
                name: read_str(&row[0]),
                in_errors: read_i64(&row[1]),
                out_errors: read_i64(&row[2]),
                in_octets: read_i64(&row[3]),
                out_octets: read_i64(&row[4]),
                carrier_transitions: read_i64(&row[5]),
                updated_at_ns: read_ts_ns(&row[6]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (n:BgpNeighbor) WHERE n.device_address = $addr \
                     RETURN n.peer_address, n.session_state, n.peer_as \
                     ORDER BY n.peer_address",
            )
            .map_err(|e| e.to_string())?;
        let bgp_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let bgp: Vec<BgpJson> = bgp_rows
            .map(|row| BgpJson {
                peer: read_str(&row[0]),
                state: read_str(&row[1]),
                peer_as: read_i64(&row[2]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (n:LldpNeighbor) WHERE n.device_address = $addr \
                     RETURN n.local_if, n.system_name, n.port_id, n.chassis_id \
                     ORDER BY n.local_if",
            )
            .map_err(|e| e.to_string())?;
        let lldp_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let lldp: Vec<LldpNeighborJson> = lldp_rows
            .map(|row| LldpNeighborJson {
                local_if: read_str(&row[0]),
                system_name: read_str(&row[1]),
                port_id: read_str(&row[2]),
                chassis_id: read_str(&row[3]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (e:StateChangeEvent) WHERE e.device_address = $addr \
                     RETURN e.event_type, e.detail, e.occurred_at \
                     ORDER BY e.occurred_at DESC LIMIT 20",
            )
            .map_err(|e| e.to_string())?;
        let sc_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let state_changes: Vec<StateChangeJson> = sc_rows
            .map(|row| StateChangeJson {
                event_type: read_str(&row[0]),
                detail: read_str(&row[1]),
                occurred_at_ns: read_ts_ns(&row[2]),
            })
            .collect();

        let mut stmt = conn
            .prepare(
                "MATCH (e:DetectionEvent) WHERE e.device_address = $addr \
                     OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(e) \
                     RETURN e.id, e.device_address, e.rule_id, e.severity, \
                            e.features_json, e.fired_at, r.id, r.action, r.status \
                     ORDER BY e.fired_at DESC LIMIT 10",
            )
            .map_err(|e| e.to_string())?;
        let det_rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone.clone()))])
            .map_err(|e| e.to_string())?;
        let mut detections: Vec<DetectionRow> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in det_rows {
            let id = read_str(&row[0]);
            if seen.insert(id.clone()) {
                detections.push(crate::graph::DetectionRow {
                    id,
                    device_address: read_str(&row[1]),
                    rule_id: read_str(&row[2]),
                    severity: read_str(&row[3]),
                    features_json: read_str(&row[4]),
                    fired_at_ns: read_ts_ns(&row[5]),
                    remediation_id: read_str(&row[6]),
                    remediation_action: read_str(&row[7]),
                    remediation_status: read_str(&row[8]),
                });
            }
        }

        Ok::<_, String>((ifaces, bgp, lldp, state_changes, detections))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let all_statuses = read_subscription_statuses(state.store.db()).await?;
    let subscription_statuses = all_statuses.get(&address).cloned().unwrap_or_default();
    let health = compute_health(&bgp);

    let overrides = state.registry.list_overrides().unwrap_or_default();
    let (_, audit) = crate::discovery::resolve_subscription_paths(&target, &overrides);
    Ok(Json(DeviceDetailResponse {
        address: address.clone(),
        hostname: target.hostname.unwrap_or_default(),
        vendor: target.vendor.unwrap_or_default(),
        role: target.role.unwrap_or_default(),
        site: target.site.unwrap_or_default(),
        enabled: target.enabled,
        collector_id: target.collector_id.unwrap_or_default(),
        credential_alias: target.credential_alias.unwrap_or_default(),
        health,
        interfaces: ifaces,
        bgp_neighbors: bgp,
        lldp_neighbors: lldp,
        recent_state_changes: state_changes,
        recent_detections: detections,
        selected_paths: target.selected_paths.clone(),
        subscription_statuses,
        resolution_audit: audit,
        created_at_ns: target.created_at_ns,
        updated_at_ns: target.updated_at_ns,
        created_by: target.created_by,
        updated_by: target.updated_by,
        last_operator_action: target.last_operator_action,
    }))
}

// ── Device enrichment endpoint ────────────────────────────────────────────────

#[derive(Serialize)]
struct EnrichmentPropertyJson {
    key: String,
    value: String,
    source_name: String,
    updated_at_ns: i64,
    confidence: String,
    parser: String,
}

#[derive(Serialize)]
struct DeviceEnrichmentResponse {
    address: String,
    /// Properties grouped by source_name for display.
    properties: Vec<EnrichmentPropertyJson>,
}

async fn device_enrichment_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceEnrichmentResponse>, (StatusCode, String)> {
    let db = state.store.db();
    let addr_clone = address.clone();

    let props = tokio::task::spawn_blocking(move || {
        let conn = Connection::new(&db).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "MATCH (d:Device {address: $addr})-[:HAS_ENRICHMENT_PROPERTY]->(p:EnrichmentProperty) \
                 OPTIONAL MATCH (p)-[:ENRICHMENT_PROPERTY_PROVENANCE]->(prov:PropertyProvenance) \
                 RETURN p.key, p.value, p.source_name, p.updated_at, prov.confidence, prov.parser \
                 ORDER BY p.source_name, p.key",
            )
            .map_err(|e| e.to_string())?;
        let rows = conn
            .execute(&mut stmt, vec![("addr", Value::String(addr_clone))])
            .map_err(|e| e.to_string())?;
        let props: Vec<EnrichmentPropertyJson> = rows
            .map(|row| EnrichmentPropertyJson {
                key: read_str(&row[0]),
                value: read_str(&row[1]),
                source_name: read_str(&row[2]),
                updated_at_ns: read_ts_ns(&row[3]),
                confidence: read_str(&row[4]),
                parser: read_str(&row[5]),
            })
            .collect();
        Ok::<_, String>(props)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(DeviceEnrichmentResponse {
        address,
        properties: props,
    }))
}

#[derive(Serialize)]
struct DeviceConfigHistoryResponse {
    address: String,
    snapshots: Vec<change_detection::ConfigSnapshotSummary>,
    changes: Vec<change_detection::ConfigChangeSummary>,
}

#[derive(Serialize)]
struct DeviceGnmiReadinessResponse {
    address: String,
    report: discovery::GnmiReadinessReport,
}

#[derive(Serialize)]
struct DeviceStreamingReadinessResponse {
    address: String,
    report: StreamingReadinessReport,
}

async fn device_config_history_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceConfigHistoryResponse>, (StatusCode, String)> {
    let (snapshots, changes) = change_detection::config_history(
        Arc::clone(&state.store),
        address.clone(),
        state.change_detection.history_limit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(DeviceConfigHistoryResponse {
        address,
        snapshots,
        changes,
    }))
}

async fn device_gnmi_readiness_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceGnmiReadinessResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;
    let resolved = resolve_target_credentials_for_discovery(&target, &state.credentials)
        .map_err(|e| (StatusCode::FAILED_DEPENDENCY, e.to_string()))?;
    let report = discovery::gnmi_readiness_report(
        DiscoveryInput {
            address: target.address.clone(),
            username: resolved.as_ref().map(|creds| creds.username.clone()),
            password: resolved.as_ref().map(|creds| creds.password.clone()),
            username_env: None,
            password_env: None,
            ca_cert_path: target.ca_cert.clone(),
            tls_domain: target.tls_domain.clone(),
            role_hint: target.role.clone(),
            environment_archetype: None,
        },
        &state.layered_ingestion.gnmi_known_issues_path,
    )
    .await;
    persist_gnmi_readiness(Arc::clone(&state.store), &address, &report)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(DeviceGnmiReadinessResponse { address, report }))
}

async fn device_streaming_readiness_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceStreamingReadinessResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    let resolved = resolve_target_credentials_for_discovery(&target, &state.credentials)
        .map_err(|e| (StatusCode::FAILED_DEPENDENCY, e.to_string()))?;
    let gnmi = if resolved.is_some() || target.ca_cert.is_some() {
        Some(
            discovery::gnmi_readiness_report(
                DiscoveryInput {
                    address: target.address.clone(),
                    username: resolved.as_ref().map(|creds| creds.username.clone()),
                    password: resolved.as_ref().map(|creds| creds.password.clone()),
                    username_env: None,
                    password_env: None,
                    ca_cert_path: target.ca_cert.clone(),
                    tls_domain: target.tls_domain.clone(),
                    role_hint: target.role.clone(),
                    environment_archetype: None,
                },
                &state.layered_ingestion.gnmi_known_issues_path,
            )
            .await,
        )
    } else {
        None
    };

    let report =
        streaming::build_streaming_readiness_report(&target, gnmi.as_ref(), &state.streaming);
    persist_streaming_readiness(Arc::clone(&state.store), &address, &report)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(DeviceStreamingReadinessResponse { address, report }))
}

async fn device_recommendations_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DeviceRecommendationsResponse>, (StatusCode, String)> {
    let target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    let mut warnings = Vec::new();
    let resolved = match resolve_target_credentials_for_discovery(&target, &state.credentials) {
        Ok(resolved) => resolved,
        Err(error) => {
            warnings.push(format!(
                "could not resolve device credentials for live discovery: {error:#}"
            ));
            None
        }
    };

    let discovery_input = DiscoveryInput {
        address: target.address.clone(),
        username: resolved.as_ref().map(|creds| creds.username.clone()),
        password: resolved.as_ref().map(|creds| creds.password.clone()),
        username_env: None,
        password_env: None,
        ca_cert_path: target.ca_cert.clone(),
        tls_domain: target.tls_domain.clone(),
        role_hint: target.role.clone(),
        environment_archetype: None,
    };

    let discovery_report = match discovery::discover_device(discovery_input.clone()).await {
        Ok(report) => Some(report),
        Err(error) => {
            warnings.push(format!(
                "live capabilities discovery unavailable: {error:#}"
            ));
            None
        }
    };

    let readiness_report = if resolved.is_some() || target.ca_cert.is_some() {
        Some(
            discovery::gnmi_readiness_report(
                discovery_input,
                &state.layered_ingestion.gnmi_known_issues_path,
            )
            .await,
        )
    } else {
        None
    };
    let streaming_readiness = streaming::build_streaming_readiness_report(
        &target,
        readiness_report.as_ref(),
        &state.streaming,
    );

    let overrides = state.registry.list_overrides().unwrap_or_default();
    let yang_library_state = YangLibrary::open(
        &state.yang_library_root,
        &state.yang_cache_root,
        &state.yang_bundle_key_env,
    )
    .ok()
    .and_then(|library| library.load_state().ok());

    let report = synthesizer::synthesize_for_target(
        &target,
        discovery_report.as_ref(),
        readiness_report.as_ref(),
        Some(&streaming_readiness),
        warnings,
        &overrides,
        yang_library_state.as_ref(),
    );
    Ok(Json(DeviceRecommendationsResponse { report }))
}

async fn yang_modules_handler(
    State(state): State<AppState>,
) -> Result<Json<YangModulesResponse>, (StatusCode, String)> {
    let library = YangLibrary::open(
        &state.yang_library_root,
        &state.yang_cache_root,
        &state.yang_bundle_key_env,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let modules = library
        .list_modules()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(YangModulesResponse { modules }))
}

async fn yang_search_handler(
    State(state): State<AppState>,
    Query(params): Query<YangSearchParams>,
) -> Result<Json<YangSearchResponse>, (StatusCode, String)> {
    let library = YangLibrary::open(
        &state.yang_library_root,
        &state.yang_cache_root,
        &state.yang_bundle_key_env,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let result = library
        .search(&params.q)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(YangSearchResponse { result }))
}

async fn apply_device_selected_paths_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Json(req): Json<ApplySelectedPathsRequest>,
) -> Result<Json<ApplySelectedPathsResponse>, (StatusCode, String)> {
    let mut target = state
        .registry
        .get_device(&address)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("device '{address}' not found"),
            )
        })?;

    target.selected_paths = req
        .selected_paths
        .into_iter()
        .filter(|path| !path.path.trim().is_empty())
        .collect();

    let updated = state
        .registry
        .update_device_with_audit(target, "api", "api_apply_recommendation_paths")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ApplySelectedPathsResponse {
        success: true,
        error: String::new(),
        selected_paths: updated.selected_paths,
    }))
}

async fn persist_gnmi_readiness(
    store: Arc<GraphStore>,
    address: &str,
    report: &discovery::GnmiReadinessReport,
) -> anyhow::Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let address = address.to_string();
    let report = report.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("graph write lock poisoned"))?;
        let conn = Connection::new(&db)?;
        let readiness_id = format!("{address}:gnmi-readiness");
        let checked_at =
            time::OffsetDateTime::from_unix_timestamp_nanos(report.checked_at_ns.into())?;
        let mut stmt = conn.prepare(
            "MERGE (r:GnmiReadiness {id: $id}) \
             SET r.device_address = $addr, r.service_status = $service_status, \
                 r.tls_status = $tls_status, r.encoding_support = $encoding_support, \
                 r.models_advertised = $models_advertised, r.known_issues = $known_issues, \
                 r.blockers = $blockers, r.recommended_actions = $recommended_actions, \
                 r.checked_at = $checked_at",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(readiness_id.clone())),
                ("addr", Value::String(address.clone())),
                ("service_status", Value::String(report.service_status)),
                ("tls_status", Value::String(report.tls_status)),
                (
                    "encoding_support",
                    Value::String(serde_json::to_string(&report.encoding_support)?),
                ),
                (
                    "models_advertised",
                    Value::String(serde_json::to_string(&report.models_advertised)?),
                ),
                (
                    "known_issues",
                    Value::String(serde_json::to_string(&report.known_issues)?),
                ),
                (
                    "blockers",
                    Value::String(serde_json::to_string(&report.blockers)?),
                ),
                (
                    "recommended_actions",
                    Value::String(serde_json::to_string(&report.recommended_actions)?),
                ),
                ("checked_at", Value::TimestampNs(checked_at)),
            ],
        )?;

        let mut rel_stmt = conn.prepare(
            "MATCH (d:Device {address: $addr}), (r:GnmiReadiness {id: $id}) \
             MERGE (d)-[:HAS_GNMI_READINESS]->(r)",
        )?;
        conn.execute(
            &mut rel_stmt,
            vec![
                ("addr", Value::String(address)),
                ("id", Value::String(readiness_id)),
            ],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("gNMI readiness persistence task panicked: {e}"))?
}

async fn persist_streaming_readiness(
    store: Arc<GraphStore>,
    address: &str,
    report: &StreamingReadinessReport,
) -> anyhow::Result<()> {
    let db = store.db();
    let write_lock = store.write_lock();
    let address = address.to_string();
    let report = report.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("graph write lock poisoned"))?;
        let conn = Connection::new(&db)?;
        let readiness_id = format!("{address}:streaming-readiness");
        let checked_at =
            time::OffsetDateTime::from_unix_timestamp_nanos(report.checked_at_ns.into())?;
        let mut stmt = conn.prepare(
            "MERGE (r:StreamingReadiness {id: $id}) \
             SET r.device_address = $addr, r.vendor = $vendor, r.role = $role, \
                 r.protocols_json = $protocols_json, r.recommended_protocols_json = $recommended_protocols_json, \
                 r.checked_at = $checked_at",
        )?;
        conn.execute(
            &mut stmt,
            vec![
                ("id", Value::String(readiness_id.clone())),
                ("addr", Value::String(address.clone())),
                ("vendor", Value::String(report.vendor)),
                ("role", Value::String(report.role)),
                (
                    "protocols_json",
                    Value::String(serde_json::to_string(&report.protocols)?),
                ),
                (
                    "recommended_protocols_json",
                    Value::String(serde_json::to_string(&report.recommended_protocols)?),
                ),
                ("checked_at", Value::TimestampNs(checked_at)),
            ],
        )?;
        let mut rel_stmt = conn.prepare(
            "MATCH (d:Device {address: $addr}), (r:StreamingReadiness {id: $id}) \
             MERGE (d)-[:HAS_STREAMING_READINESS]->(r)",
        )?;
        conn.execute(
            &mut rel_stmt,
            vec![
                ("addr", Value::String(address)),
                ("id", Value::String(readiness_id)),
            ],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("streaming readiness persistence task panicked: {e}"))?
}

#[derive(Deserialize)]
struct DeviceReparseRequest {
    #[serde(default)]
    reason: String,
}

#[derive(Serialize)]
struct DeviceReparseResponse {
    success: bool,
    message: String,
}

async fn device_reparse_handler(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Json(req): Json<DeviceReparseRequest>,
) -> Json<DeviceReparseResponse> {
    let reason = if req.reason.trim().is_empty() {
        "operator-triggered re-parse".to_string()
    } else {
        req.reason
    };
    match state
        .change_detection
        .enqueue_manual(&address, &reason)
        .await
    {
        Ok(()) => Json(DeviceReparseResponse {
            success: true,
            message: format!("re-parse queued for {address}"),
        }),
        Err(error) => Json(DeviceReparseResponse {
            success: false,
            message: error.to_string(),
        }),
    }
}

fn resolve_target_credentials_for_discovery(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> anyhow::Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref() {
        return credentials
            .resolve(alias, ResolvePurpose::Discover)
            .map(Some);
    }
    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
            _ => None,
        },
    )
}

