#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── Profiles ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ProfilesResponse {
    profiles: Vec<ProfileJson>,
    plugins: Vec<PluginJson>,
    load_errors: Vec<String>,
}

#[derive(Serialize)]
struct ProfileJson {
    name: String,
    environment: Vec<String>,
    vendor_scope: Vec<String>,
    roles: Vec<String>,
    description: String,
    rationale: String,
    path_count: usize,
    source: String,
}

#[derive(Serialize)]
struct PluginJson {
    name: String,
    version: String,
    author: String,
    profile_count: usize,
    conflicts: Vec<String>,
}

async fn profiles_handler(State(state): State<AppState>) -> Json<ProfilesResponse> {
    let cat = state.catalogue.read().await;

    let profiles: Vec<ProfileJson> = cat
        .profiles
        .iter()
        .map(|p| ProfileJson {
            name: p.name.clone(),
            environment: p.environment.clone(),
            vendor_scope: p.vendor_scope.clone(),
            roles: p.roles.clone(),
            description: p.description.clone(),
            rationale: p.rationale.clone(),
            path_count: p.paths.len(),
            source: "built-in".to_string(),
        })
        .chain(cat.plugins.iter().flat_map(|plugin| {
            plugin.profiles.iter().map(move |p| ProfileJson {
                name: p.name.clone(),
                environment: p.environment.clone(),
                vendor_scope: p.vendor_scope.clone(),
                roles: p.roles.clone(),
                description: p.description.clone(),
                rationale: p.rationale.clone(),
                path_count: p.paths.len(),
                source: format!("plugin:{}", plugin.manifest.name),
            })
        }))
        .collect();

    let plugins: Vec<PluginJson> = cat
        .plugins
        .iter()
        .map(|p| PluginJson {
            name: p.manifest.name.clone(),
            version: p.manifest.version.clone(),
            author: p.manifest.author.clone(),
            profile_count: p.profiles.len(),
            conflicts: p.conflicts.clone(),
        })
        .collect();

    Json(ProfilesResponse {
        profiles,
        plugins,
        load_errors: cat.load_errors.clone(),
    })
}

#[derive(Deserialize)]
struct SaveCustomProfileRequest {
    name: String,
    description: String,
    rationale: String,
    environment: Vec<String>,
    vendor_scope: Vec<String>,
    roles: Vec<String>,
    paths: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct SaveCustomProfileResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn save_custom_profile_handler(
    State(state): State<AppState>,
    Json(req): Json<SaveCustomProfileRequest>,
) -> Json<SaveCustomProfileResponse> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some("profile name is required".to_string()),
        });
    }
    // Sanitise: only alphanumeric, underscore, hyphen
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(
                "profile name may only contain letters, digits, underscores, and hyphens"
                    .to_string(),
            ),
        });
    }

    let user_plugin_dir = std::path::Path::new(&state.catalogue_dir)
        .join("plugins")
        .join("user");

    if let Err(e) = std::fs::create_dir_all(&user_plugin_dir) {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(format!("cannot create user plugin dir: {e}")),
        });
    }

    // Build the profile YAML document
    let profile_doc = serde_json::json!({
        "name": name,
        "environment": req.environment,
        "vendor_scope": req.vendor_scope,
        "roles": req.roles,
        "description": req.description,
        "rationale": req.rationale,
        "paths": req.paths,
    });
    let yaml_str = match serde_yaml::to_string(&profile_doc) {
        Ok(s) => s,
        Err(e) => {
            return Json(SaveCustomProfileResponse {
                success: false,
                error: Some(format!("yaml serialisation error: {e}")),
            });
        }
    };

    let profile_filename = format!("{name}.yaml");
    let profile_path = user_plugin_dir.join(&profile_filename);
    if let Err(e) = std::fs::write(&profile_path, yaml_str) {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(format!("cannot write profile file: {e}")),
        });
    }

    // Rebuild the MANIFEST.yaml from all YAMLs in the user plugin dir
    let mut profile_files: Vec<String> = std::fs::read_dir(&user_plugin_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("yaml")
                && p.file_name().and_then(|x| x.to_str()) != Some("MANIFEST.yaml")
            {
                p.file_name()
                    .and_then(|x| x.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    profile_files.sort();

    let manifest_doc = serde_json::json!({
        "name": "user",
        "version": "0.1.0",
        "author": "operator",
        "profiles": profile_files,
    });
    let manifest_str = match serde_yaml::to_string(&manifest_doc) {
        Ok(s) => s,
        Err(e) => {
            return Json(SaveCustomProfileResponse {
                success: false,
                error: Some(format!("manifest serialisation error: {e}")),
            });
        }
    };
    if let Err(e) = std::fs::write(user_plugin_dir.join("MANIFEST.yaml"), manifest_str) {
        return Json(SaveCustomProfileResponse {
            success: false,
            error: Some(format!("cannot write MANIFEST.yaml: {e}")),
        });
    }

    // Reload catalogue and swap in
    let new_catalogue =
        crate::catalogue::load_catalogue(std::path::Path::new(&state.catalogue_dir));
    *state.catalogue.write().await = new_catalogue;

    Json(SaveCustomProfileResponse {
        success: true,
        error: None,
    })
}

// ── Enrichment handlers ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct EnricherEntry {
    config: crate::enrichment::EnricherConfig,
    state: crate::enrichment::EnricherRunState,
}

#[derive(Serialize)]
struct EnrichmentListResponse {
    enrichers: Vec<EnricherEntry>,
}

async fn enrichment_list_handler(State(state): State<AppState>) -> Json<EnrichmentListResponse> {
    let reg = state.enricher_registry.read().await;
    let enrichers = reg
        .list()
        .into_iter()
        .map(|(config, st)| EnricherEntry { config, state: st })
        .collect();
    Json(EnrichmentListResponse { enrichers })
}

#[derive(Deserialize)]
struct EnrichmentUpsertRequest {
    config: EnricherConfig,
}

#[derive(Serialize)]
struct EnrichmentMutationResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn enrichment_upsert_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentUpsertRequest>,
) -> Json<EnrichmentMutationResponse> {
    state.enricher_registry.write().await.upsert(req.config);
    Json(EnrichmentMutationResponse {
        success: true,
        error: None,
    })
}

#[derive(Deserialize)]
struct EnrichmentNameRequest {
    name: String,
}

async fn enrichment_remove_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentNameRequest>,
) -> Json<EnrichmentMutationResponse> {
    let removed = state.enricher_registry.write().await.remove(&req.name);
    if removed {
        Json(EnrichmentMutationResponse {
            success: true,
            error: None,
        })
    } else {
        Json(EnrichmentMutationResponse {
            success: false,
            error: Some(format!("enricher '{}' not found", req.name)),
        })
    }
}

#[derive(Serialize)]
struct EnrichmentTestResponse {
    success: bool,
    message: String,
}

async fn enrichment_test_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentNameRequest>,
) -> Json<EnrichmentTestResponse> {
    let config = {
        let reg = state.enricher_registry.read().await;
        reg.get(&req.name).cloned()
    };
    let Some(config) = config else {
        return Json(EnrichmentTestResponse {
            success: false,
            message: format!("enricher '{}' not found", req.name),
        });
    };

    let audit = crate::enrichment::EnricherAuditLog::new(
        std::path::Path::new(&state.runtime_dir),
        &config.name,
    );

    match crate::enrichment::factory::build_enricher(&config) {
        Err(e) => Json(EnrichmentTestResponse {
            success: false,
            message: format!("cannot build enricher: {e:#}"),
        }),
        Ok(enricher) => match enricher.test_connection(&state.credentials, &audit).await {
            Ok(()) => Json(EnrichmentTestResponse {
                success: true,
                message: "connection successful".to_string(),
            }),
            Err(e) => Json(EnrichmentTestResponse {
                success: false,
                message: format!("{e:#}"),
            }),
        },
    }
}

#[derive(Serialize)]
struct EnrichmentRunResponse {
    success: bool,
    message: String,
}

async fn enrichment_run_handler(
    State(state): State<AppState>,
    Json(req): Json<EnrichmentNameRequest>,
) -> Json<EnrichmentRunResponse> {
    let config = {
        let reg = state.enricher_registry.read().await;
        reg.get(&req.name).cloned()
    };
    let Some(config) = config else {
        return Json(EnrichmentRunResponse {
            success: false,
            message: format!("enricher '{}' not found", req.name),
        });
    };

    let enricher = match crate::enrichment::factory::build_enricher(&config) {
        Ok(e) => e,
        Err(e) => {
            return Json(EnrichmentRunResponse {
                success: false,
                message: format!("cannot build enricher: {e:#}"),
            });
        }
    };

    state
        .enricher_registry
        .write()
        .await
        .set_running(&req.name, true);

    let registry_clone = Arc::clone(&state.enricher_registry);
    let name = req.name.clone();
    let runtime_dir = state.runtime_dir.clone();
    let store = Arc::clone(&state.store);
    let creds = Arc::clone(&state.credentials);

    tokio::spawn(async move {
        let audit =
            crate::enrichment::EnricherAuditLog::new(std::path::Path::new(&runtime_dir), &name);
        let report = match enricher.enrich(store.as_ref(), &creds, &audit).await {
            Ok(r) => r,
            Err(e) => crate::enrichment::EnrichmentReport {
                enricher_name: name.clone(),
                error: Some(format!("{e:#}")),
                ..Default::default()
            },
        };
        registry_clone.write().await.record_run(&name, &report);
    });

    Json(EnrichmentRunResponse {
        success: true,
        message: format!("enricher '{}' run started", req.name),
    })
}

#[derive(Serialize)]
struct EnrichmentAuditResponse {
    entries: Vec<serde_json::Value>,
}

async fn enrichment_audit_handler(State(state): State<AppState>) -> Json<EnrichmentAuditResponse> {
    // Read audit log files and return the last 100 enrichment_run entries.
    let audit_dir = std::path::Path::new(&state.runtime_dir).join("audit");
    let entries = read_recent_enrichment_audit(&audit_dir, 100);
    Json(EnrichmentAuditResponse { entries })
}

fn read_recent_enrichment_audit(
    audit_dir: &std::path::Path,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut files: Vec<_> = std::fs::read_dir(audit_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    files.sort();

    let mut entries: Vec<serde_json::Value> = Vec::new();
    for file in files.iter().rev() {
        if let Ok(content) = std::fs::read_to_string(file) {
            for line in content.lines().rev() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                    && val.get("event").and_then(|v| v.as_str()) == Some("enrichment_run")
                {
                    entries.push(val);
                    if entries.len() >= limit {
                        return entries;
                    }
                }
            }
        }
    }
    entries
}

