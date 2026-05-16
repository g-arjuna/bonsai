pub(super) fn schema_components() -> serde_json::Value {
    serde_json::json!({
        "schemas": {
            "Device": {
                "type": "object",
                "properties": {
                    "address": { "type": "string", "description": "Management IP address (gNMI target)" },
                    "hostname": { "type": "string", "description": "Device hostname from gNMI telemetry" },
                    "vendor": { "type": "string", "enum": ["nokia", "cisco", "juniper", "arista", "frr", "holo", "unknown"] },
                    "role": { "type": "string", "description": "Topology role e.g. spine, leaf, pe, p, rr, super-spine" },
                    "site": { "type": "string", "description": "Site name from graph enrichment" },
                    "health": { "type": "string", "enum": ["healthy", "warn", "critical"] },
                    "bgp": { "type": "array", "items": { "$ref": "#/components/schemas/BgpSession" }},
                    "_schema_version": { "type": "string", "description": "Bonsai version that produced this record" }
                }
            },
            "BgpSession": {
                "type": "object",
                "properties": {
                    "peer": { "type": "string", "description": "Peer IP address" },
                    "state": { "type": "string", "description": "BGP session state: Established, Active, Idle, etc." },
                    "peer_as": { "type": "integer", "description": "Peer autonomous system number" }
                }
            },
            "Link": {
                "type": "object",
                "properties": {
                    "src_device": { "type": "string" },
                    "src_iface": { "type": "string" },
                    "dst_device": { "type": "string" },
                    "dst_iface": { "type": "string" },
                    "bytes_total": { "type": "integer", "description": "Sum of in_octets+out_octets on both ends — used for utilisation heatmap colouring" },
                    "is_mgmt": { "type": "boolean", "description": "True for out-of-band management-plane LLDP links" }
                }
            },
            "DetectionEvent": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "device_address": { "type": "string" },
                    "rule_id": { "type": "string", "description": "Detection rule that fired, e.g. bgp_session_down" },
                    "severity": { "type": "string", "enum": ["critical", "warn", "info"] },
                    "features_json": { "type": "string", "description": "JSON-serialized Features struct — used for ML training and GNN input" },
                    "fired_at_ns": { "type": "integer", "description": "Unix timestamp in nanoseconds" },
                    "remediation_id": { "type": "string", "format": "uuid" },
                    "remediation_action": { "type": "string" },
                    "remediation_status": { "type": "string", "enum": ["pending", "approved", "rejected", "executed", "rolled_back"] },
                    "_schema_version": { "type": "string" }
                }
            },
            "TopologyResponse": {
                "type": "object",
                "properties": {
                    "_schema_version": { "type": "string" },
                    "devices": { "type": "array", "items": { "$ref": "#/components/schemas/Device" }},
                    "links": { "type": "array", "items": { "$ref": "#/components/schemas/Link" }}
                },
                "required": ["_schema_version", "devices", "links"]
            },
            "DetectionsResponse": {
                "type": "object",
                "properties": {
                    "_schema_version": { "type": "string" },
                    "detections": { "type": "array", "items": { "$ref": "#/components/schemas/DetectionEvent" }}
                },
                "required": ["_schema_version", "detections"]
            },
            "Incident": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "root": { "$ref": "#/components/schemas/DetectionEvent" },
                    "cascading": { "type": "array", "items": { "$ref": "#/components/schemas/DetectionEvent" }},
                    "affected_devices": { "type": "array", "items": { "type": "string" }},
                    "severity": { "type": "string" },
                    "started_at_ns": { "type": "integer" },
                    "ended_at_ns": { "type": "integer" },
                    "remediation_status": { "type": "string" }
                },
                "required": ["id", "root", "cascading", "affected_devices", "severity", "started_at_ns", "ended_at_ns", "remediation_status"]
            },
            "IncidentsResponse": {
                "type": "object",
                "properties": {
                    "_schema_version": { "type": "string" },
                    "incidents": { "type": "array", "items": { "$ref": "#/components/schemas/Incident" }}
                },
                "required": ["_schema_version", "incidents"]
            },
            "ReadinessResponse": {
                "type": "object",
                "properties": {
                    "_schema_version": { "type": "string" },
                    "detection_events": { "type": "integer" },
                    "state_change_events": { "type": "integer" },
                    "rule_distribution": { "type": "object", "additionalProperties": { "type": "integer" }},
                    "cutoff_iso": { "type": "string" },
                    "remediation_rows_post_cutoff": { "type": "integer" },
                    "action_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }},
                    "status_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }}
                },
                "required": ["_schema_version", "detection_events", "state_change_events", "rule_distribution", "cutoff_iso", "remediation_rows_post_cutoff", "action_distribution_post_cutoff", "status_distribution_post_cutoff"]
            },
            "OperationsResponse": {
                "type": "object",
                "properties": {
                    "_schema_version": { "type": "string" },
                    "detection_events": { "type": "integer" },
                    "state_change_events": { "type": "integer" },
                    "remediation_rows_post_cutoff": { "type": "integer" },
                    "rule_distribution": { "type": "object", "additionalProperties": { "type": "integer" }},
                    "action_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }},
                    "status_distribution_post_cutoff": { "type": "object", "additionalProperties": { "type": "integer" }},
                    "device_count": { "type": "integer" },
                    "enabled_device_count": { "type": "integer" },
                    "observed_subscriptions": { "type": "integer" },
                    "pending_subscriptions": { "type": "integer" },
                    "silent_subscriptions": { "type": "integer" },
                    "collectors_connected": { "type": "integer" },
                    "collectors_total": { "type": "integer" },
                    "unassigned_devices": { "type": "integer" },
                    "event_bus_depth": { "type": "integer" },
                    "event_bus_receivers": { "type": "integer" },
                    "archive_lag_millis": { "type": "integer" },
                    "archive_buffer_rows": { "type": "integer" },
                    "archive_last_flush_millis": { "type": "integer" },
                    "archive_last_compression_ppm": { "type": "integer" },
                    "cutoff_iso": { "type": "string" },
                    "rss_bytes": { "type": "integer" },
                    "archive_disk_bytes": { "type": "integer" },
                    "archive_disk_pct": { "type": "integer" },
                    "graph_disk_bytes": { "type": "integer" },
                    "graph_disk_pct": { "type": "integer" },
                    "memory_budget_bytes": { "type": "integer" },
                    "memory_rss_pct_of_budget": { "type": "number" },
                    "counter_mode": { "type": "string" },
                    "counter_window_secs": { "type": "integer" },
                    "counter_debounce_secs": { "type": "integer" }
                },
                "required": ["_schema_version", "detection_events", "state_change_events", "remediation_rows_post_cutoff", "rule_distribution", "action_distribution_post_cutoff", "status_distribution_post_cutoff", "device_count", "enabled_device_count", "observed_subscriptions", "pending_subscriptions", "silent_subscriptions", "collectors_connected", "collectors_total", "unassigned_devices", "event_bus_depth", "event_bus_receivers", "archive_lag_millis", "archive_buffer_rows", "archive_last_flush_millis", "archive_last_compression_ppm", "cutoff_iso", "rss_bytes", "archive_disk_bytes", "archive_disk_pct", "graph_disk_bytes", "graph_disk_pct", "memory_budget_bytes", "memory_rss_pct_of_budget", "counter_mode", "counter_window_secs", "counter_debounce_secs"]
            },
            "ManagedDevice": {
                "type": "object",
                "properties": {
                    "address": { "type": "string" },
                    "enabled": { "type": "boolean" },
                    "collector_id": { "type": "string" },
                    "tls_domain": { "type": "string" },
                    "ca_cert": { "type": "string" },
                    "vendor": { "type": "string" },
                    "credential_alias": { "type": "string" },
                    "username_env": { "type": "string" },
                    "password_env": { "type": "string" },
                    "hostname": { "type": "string" },
                    "role": { "type": "string" },
                    "site": { "type": "string" },
                    "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }},
                    "subscription_statuses": { "type": "array", "items": { "$ref": "#/components/schemas/SubscriptionStatus" }},
                    "resolution_audit": { "type": "array", "items": { "type": "string" }}
                },
                "required": ["address", "enabled", "collector_id", "tls_domain", "ca_cert", "vendor", "credential_alias", "username_env", "password_env", "hostname", "role", "site", "selected_paths", "subscription_statuses", "resolution_audit"]
            },
            "ManagedDevicesResponse": {
                "type": "object",
                "properties": {
                    "devices": { "type": "array", "items": { "$ref": "#/components/schemas/ManagedDevice" }}
                },
                "required": ["devices"]
            },
            "SubscriptionStatus": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "origin": { "type": "string" },
                    "mode": { "type": "string" },
                    "sample_interval_ns": { "type": "integer" },
                    "status": { "type": "string" },
                    "first_observed_at_ns": { "type": "integer" },
                    "last_observed_at_ns": { "type": "integer" },
                    "updated_at_ns": { "type": "integer" }
                },
                "required": ["path", "origin", "mode", "sample_interval_ns", "status", "first_observed_at_ns", "last_observed_at_ns", "updated_at_ns"]
            },
            "SetupStatusResponse": {
                "type": "object",
                "properties": {
                    "is_first_run": { "type": "boolean" },
                    "has_environments": { "type": "boolean" },
                    "has_credentials": { "type": "boolean" },
                    "has_devices": { "type": "boolean" }
                },
                "required": ["is_first_run", "has_environments", "has_credentials", "has_devices"]
            },
            "InterfaceDetail": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "in_errors": { "type": "integer" },
                    "out_errors": { "type": "integer" },
                    "in_octets": { "type": "integer" },
                    "out_octets": { "type": "integer" },
                    "carrier_transitions": { "type": "integer" },
                    "updated_at_ns": { "type": "integer" }
                },
                "required": ["name", "in_errors", "out_errors", "in_octets", "out_octets", "carrier_transitions", "updated_at_ns"]
            },
            "LldpNeighbor": {
                "type": "object",
                "properties": {
                    "local_if": { "type": "string" },
                    "system_name": { "type": "string" },
                    "port_id": { "type": "string" },
                    "chassis_id": { "type": "string" }
                },
                "required": ["local_if", "system_name", "port_id", "chassis_id"]
            },
            "StateChange": {
                "type": "object",
                "properties": {
                    "event_type": { "type": "string" },
                    "detail": { "type": "string" },
                    "occurred_at_ns": { "type": "integer" }
                },
                "required": ["event_type", "detail", "occurred_at_ns"]
            },
            "DeviceDetailResponse": {
                "type": "object",
                "properties": {
                    "address": { "type": "string" },
                    "hostname": { "type": "string" },
                    "vendor": { "type": "string" },
                    "role": { "type": "string" },
                    "site": { "type": "string" },
                    "enabled": { "type": "boolean" },
                    "collector_id": { "type": "string" },
                    "credential_alias": { "type": "string" },
                    "health": { "type": "string" },
                    "interfaces": { "type": "array", "items": { "$ref": "#/components/schemas/InterfaceDetail" }},
                    "bgp_neighbors": { "type": "array", "items": { "$ref": "#/components/schemas/BgpSession" }},
                    "lldp_neighbors": { "type": "array", "items": { "$ref": "#/components/schemas/LldpNeighbor" }},
                    "recent_state_changes": { "type": "array", "items": { "$ref": "#/components/schemas/StateChange" }},
                    "recent_detections": { "type": "array", "items": { "$ref": "#/components/schemas/DetectionEvent" }},
                    "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }},
                    "subscription_statuses": { "type": "array", "items": { "$ref": "#/components/schemas/SubscriptionStatus" }},
                    "resolution_audit": { "type": "array", "items": { "type": "string" }},
                    "created_at_ns": { "type": "integer" },
                    "updated_at_ns": { "type": "integer" },
                    "created_by": { "type": "string" },
                    "updated_by": { "type": "string" },
                    "last_operator_action": { "type": "string" }
                },
                "required": ["address", "hostname", "vendor", "role", "site", "enabled", "collector_id", "credential_alias", "health", "interfaces", "bgp_neighbors", "lldp_neighbors", "recent_state_changes", "recent_detections", "selected_paths", "subscription_statuses", "resolution_audit", "created_at_ns", "updated_at_ns", "created_by", "updated_by", "last_operator_action"]
            },
            "DeviceGnmiReadinessResponse": {
                "type": "object",
                "properties": {
                    "address": { "type": "string" },
                    "report": { "type": "object", "additionalProperties": true }
                },
                "required": ["address", "report"]
            },
            "DeviceStreamingReadinessResponse": {
                "type": "object",
                "properties": {
                    "address": { "type": "string" },
                    "report": { "type": "object", "additionalProperties": true }
                },
                "required": ["address", "report"]
            },
            "DeviceRecommendationsResponse": {
                "type": "object",
                "properties": {
                    "report": { "type": "object", "additionalProperties": true }
                },
                "required": ["report"]
            },
            "ApplySelectedPathsRequest": {
                "type": "object",
                "properties": {
                    "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                },
                "required": ["selected_paths"]
            },
            "ApplySelectedPathsResponse": {
                "type": "object",
                "properties": {
                    "success": { "type": "boolean" },
                    "error": { "type": "string" },
                    "selected_paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                },
                "required": ["success", "error", "selected_paths"]
            },
            "YangModulesResponse": {
                "type": "object",
                "properties": {
                    "modules": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                },
                "required": ["modules"]
            },
            "YangSearchResponse": {
                "type": "object",
                "properties": {
                    "result": { "type": "object", "additionalProperties": true }
                },
                "required": ["result"]
            },
            "Profile": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "environment": { "type": "array", "items": { "type": "string" }},
                    "vendor_scope": { "type": "array", "items": { "type": "string" }},
                    "roles": { "type": "array", "items": { "type": "string" }},
                    "description": { "type": "string" },
                    "rationale": { "type": "string" },
                    "path_count": { "type": "integer" },
                    "source": { "type": "string" }
                },
                "required": ["name", "environment", "vendor_scope", "roles", "description", "rationale", "path_count", "source"]
            },
            "Plugin": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "version": { "type": "string" },
                    "author": { "type": "string" },
                    "profile_count": { "type": "integer" },
                    "conflicts": { "type": "array", "items": { "type": "string" }}
                },
                "required": ["name", "version", "author", "profile_count", "conflicts"]
            },
            "ProfilesResponse": {
                "type": "object",
                "properties": {
                    "profiles": { "type": "array", "items": { "$ref": "#/components/schemas/Profile" }},
                    "plugins": { "type": "array", "items": { "$ref": "#/components/schemas/Plugin" }},
                    "load_errors": { "type": "array", "items": { "type": "string" }}
                },
                "required": ["profiles", "plugins", "load_errors"]
            },
            "SaveCustomProfileRequest": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "rationale": { "type": "string" },
                    "environment": { "type": "array", "items": { "type": "string" }},
                    "vendor_scope": { "type": "array", "items": { "type": "string" }},
                    "roles": { "type": "array", "items": { "type": "string" }},
                    "paths": { "type": "array", "items": { "type": "object", "additionalProperties": true }}
                },
                "required": ["name", "description", "rationale", "environment", "vendor_scope", "roles", "paths"]
            },
            "SaveCustomProfileResponse": {
                "type": "object",
                "properties": {
                    "success": { "type": "boolean" },
                    "error": { "type": "string", "nullable": true }
                },
                "required": ["success"]
            },
            "SnowTestRequest": {
                "type": "object",
                "properties": {
                    "instance_url": { "type": "string" },
                    "credential_alias": { "type": "string" }
                },
                "required": ["instance_url", "credential_alias"]
            },
            "SnowTestResponse": {
                "type": "object",
                "properties": {
                    "success": { "type": "boolean" },
                    "message": { "type": "string" }
                },
                "required": ["success", "message"]
            },
            "SnowAiopsSyncResponse": {
                "type": "object",
                "properties": {
                    "success": { "type": "boolean" },
                    "error": { "type": "string" },
                    "stats": { "type": "object", "additionalProperties": true }
                },
                "required": ["success", "error", "stats"]
            },
            "SiteRecord": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                    "location": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["name"]
            },
            "EnvironmentRecord": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                    "archetype": { "type": "string", "enum": ["data_center", "service_provider", "home_lab"] },
                    "description": { "type": "string" }
                },
                "required": ["name", "archetype"]
            },
            "AddDeviceRequest": {
                "type": "object",
                "properties": {
                    "address": { "type": "string", "description": "Management IP:port, e.g. 192.168.1.1:57400" },
                    "credential_alias": { "type": "string", "description": "Vault alias for gNMI credentials" },
                    "role_hint": { "type": "string", "description": "Optional topology role hint, e.g. spine, leaf, pe" },
                    "ca_cert_path": { "type": "string", "description": "Path to CA cert for TLS verification (defaults to lab CA)" },
                    "tls_domain": { "type": "string" }
                },
                "required": ["address", "credential_alias"]
            },
            "DiscoverRequest": {
                "type": "object",
                "properties": {
                    "address": { "type": "string", "description": "Management IP:port to probe" },
                    "credential_alias": { "type": "string" },
                    "username_env": { "type": "string", "description": "Env var holding username (alternative to alias)" },
                    "password_env": { "type": "string", "description": "Env var holding password (alternative to alias)" },
                    "ca_cert_path": { "type": "string" },
                    "tls_domain": { "type": "string" },
                    "role_hint": { "type": "string" },
                    "environment_archetype": { "type": "string" }
                },
                "required": ["address"]
            }
        }
    })
}
