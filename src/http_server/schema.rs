pub(super) fn openapi_schema() -> serde_json::Value {
    let topology_example = load_openapi_example("topology");
    let detections_example = load_openapi_example("detections");
    let incidents_example = load_openapi_example("incidents");
    let readiness_example = load_openapi_example("readiness");
    let operations_example = load_openapi_example("operations");
    let grounded_incident_example = load_openapi_example("grounded_incident");
    let managed_devices_example = load_openapi_example("managed_devices");
    let onboarding_discover_example = load_openapi_example("onboarding_discover");
    let device_detail_example = load_openapi_example("device_detail");
    let device_gnmi_readiness_example = load_openapi_example("device_gnmi_readiness");
    let device_streaming_readiness_example = load_openapi_example("device_streaming_readiness");
    let device_recommendations_example = load_openapi_example("device_recommendations");
    let apply_selected_paths_example = load_openapi_example("apply_selected_paths");
    let setup_status_example = load_openapi_example("setup_status");
    let yang_modules_example = load_openapi_example("yang_modules");
    let yang_search_example = load_openapi_example("yang_search");
    let profiles_example = load_openapi_example("profiles");
    let save_custom_profile_example = load_openapi_example("save_custom_profile");
    let servicenow_test_example = load_openapi_example("servicenow_test");
    let servicenow_sync_example = load_openapi_example("servicenow_aiops_sync");

    serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Bonsai Network State Engine API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST + SSE API for the Bonsai network state engine. Graph-native, gNMI-first, closed-loop detect-heal. Streaming telemetry from Nokia SR Linux, Cisco IOS-XRd, Juniper cRPD, and Arista cEOS. Browse endpoints by tag. Mutation endpoints require no auth in lab deployments; production deployments should sit behind a TLS-terminating reverse proxy.",
            "x-schema-version": env!("CARGO_PKG_VERSION"),
            "contact": { "name": "Bonsai", "url": "https://github.com/bonsai-network/bonsai" },
            "license": { "name": "MIT" }
        },
        "servers": [{ "url": "http://localhost:3000", "description": "Local lab instance" }],
        "tags": [
            { "name": "Observability", "description": "Topology snapshot, detection events, incidents, blast-radius, path tracing, and SSE live stream" },
            { "name": "Devices & Onboarding", "description": "Device lifecycle management — discovery, subscription path selection, gNMI readiness, enrichment" },
            { "name": "Sites & Environments", "description": "Site and environment archetype management for multi-site topologies" },
            { "name": "YANG & Path Profiles", "description": "YANG module discovery, path search, subscription path overrides, and profile management" },
            { "name": "Enrichment", "description": "NetBox, ServiceNow, and custom enrichment adapters that write business context into the graph" },
            { "name": "Output Adapters", "description": "Splunk HEC, Elasticsearch, ServiceNow EM, and Prometheus remote-write output adapters" },
            { "name": "Credentials", "description": "Device credential vault — all APIs accept alias names only; plaintext credentials never appear in requests or responses" },
            { "name": "Trust & Approvals", "description": "Graduated remediation trust model — human approval gates before autonomous gNMI Set execution" },
            { "name": "Collectors & Assignment", "description": "Distributed collector management and device-to-collector assignment rules" },
            { "name": "Graph Explorer", "description": "Direct Cypher query interface, graph insights, saved queries, and node embedding management" },
            { "name": "Investigations", "description": "AI-assisted investigation sessions with per-tool-call audit trail" },
            { "name": "Integrations", "description": "ServiceNow AIOps and EM integration connectors" },
            { "name": "Governance", "description": "Adaptive resource governor — memory pressure, write pressure, and load-shedding state" },
            { "name": "Operations", "description": "Operational health, daily check results, weekly trends, and readiness probes" },
            { "name": "Test & Verification", "description": "Internal endpoints for CI automation, chaos harness, and AI feedback loop" },
            { "name": "MCP", "description": "Model Context Protocol JSON-RPC 2.0 endpoint for AI agent tool use" },
            { "name": "Schema", "description": "API self-description, OpenAPI spec, and natural-language reference resolution" }
        ],
        "paths": {
            "/api/topology": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Network topology snapshot",
                    "description": "Returns all devices, LLDP fabric links, management-plane links, and BGP neighbour states. Link bytes_total is the sum of in/out octets on both ends for utilisation heatmap colouring.",
                    "responses": {
                        "200": {
                            "description": "Topology snapshot",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/TopologyResponse" },
                                "example": topology_example
                            }}
                        }
                    }
                }
            },
            "/api/detections": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Recent detection events",
                    "description": "Returns the most recent DetectionEvents with associated remediation status. Each row includes severity, rule_id, device_address, features_json for ML inspection, and remediation outcome.",
                    "parameters": [{ "name": "limit", "in": "query", "schema": { "type": "integer", "default": 50 }, "description": "Maximum number of detections to return" }],
                    "responses": { "200": { "description": "Detection list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DetectionsResponse" },
                        "example": detections_example
                    }}}}
                }
            },
            "/api/incidents": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Detections grouped into incidents by time window",
                    "description": "Groups recent DetectionEvents into incidents using a sliding time window. Root detection is the highest-topology-degree device in the group. Provides the view ServiceNow EM receives as a correlated alert.",
                    "parameters": [
                        { "name": "window_secs", "in": "query", "schema": { "type": "integer", "default": 30 }, "description": "Time window in seconds for grouping co-occurring detections" },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 200 }, "description": "Maximum detections to consider before grouping" }
                    ],
                    "responses": { "200": { "description": "Incident list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/IncidentsResponse" },
                        "example": incidents_example
                    }}}}
                }
            },
            "/api/incidents/grouped": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Detections grouped by rule and device (alternate view)",
                    "description": "Returns detections pre-grouped by rule_id and device for the dashboard aggregated view. Distinct from /api/incidents which uses a sliding time-window.",
                    "parameters": [
                        { "name": "window_secs", "in": "query", "schema": { "type": "integer", "default": 30 }},
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 200 }}
                    ],
                    "responses": { "200": { "description": "Grouped incident list" }}
                }
            },
            "/api/incidents/{id}/grounded": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Grounded incident bundle",
                    "description": "Returns a detection event enriched with topological blast radius, rule documentation, recurrence indicators, and procedural references. Three-source grounding: topology (which nodes/services are impacted) + procedure (what the runbook says) + live state (current device telemetry). This is the unit of value bonsai delivers to a ServiceNow operator.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }, "description": "Root DetectionEvent UUID" }],
                    "responses": {
                        "200": { "description": "Grounded incident bundle", "content": { "application/json": {
                            "schema": { "type": "object" },
                            "example": grounded_incident_example
                        }}},
                        "404": { "description": "Detection not found" }
                    }
                }
            },
            "/api/blast-radius/{address}": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Blast radius from a device",
                    "description": "Returns all devices, applications, and active detections reachable within max_hops physical network hops from the origin device. Used to bound the service impact of a fault before executing remediation.",
                    "parameters": [
                        { "name": "address", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Management IP address of origin device" },
                        { "name": "max_hops", "in": "query", "schema": { "type": "integer", "default": 2, "minimum": 1, "maximum": 5 }, "description": "Maximum LLDP hops to traverse" }
                    ],
                    "responses": { "200": { "description": "Blast radius: affected devices, services, and active detections" }}
                }
            },
            "/api/trace/{id}": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Closed-loop trace for a detection",
                    "description": "Returns the ordered sequence of steps for a single DetectionEvent: trigger (gNMI state change) → rule evaluation → detection fired → remediation proposed → approval → gNMI Set executed → verification. Each step has a timestamp and outcome.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }, "description": "DetectionEvent UUID" }],
                    "responses": { "200": { "description": "Ordered trace steps" }}
                }
            },
            "/api/path": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "Shortest topology path between two devices",
                    "description": "Returns the shortest LLDP-derived physical path between two devices. Useful for understanding propagation paths for link faults.",
                    "parameters": [
                        { "name": "src", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Source device management IP" },
                        { "name": "dst", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Destination device management IP" }
                    ],
                    "responses": { "200": { "description": "Hop list and link list along shortest path" }}
                }
            },
            "/api/events": {
                "get": {
                    "tags": ["Observability"],
                    "summary": "SSE live event stream",
                    "description": "Server-Sent Events stream of all BonsaiEvents (StateChangeEvent, DetectionEvent, RemediationEvent). Each event is a JSON object with device_address, event_type, detail_json, and occurred_at_ns. Clients should reconnect on disconnect; the stream has no backfill.",
                    "responses": {
                        "200": {
                            "description": "text/event-stream — continuous SSE feed",
                            "content": { "text/event-stream": { "schema": { "type": "string" }}}
                        }
                    }
                }
            },
            "/api/readiness": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "Readiness probe",
                    "description": "Returns HTTP 200 when the bonsai core is ready to serve traffic (graph DB open, registry loaded). Returns 503 during startup. Safe to use as a Kubernetes/Docker HEALTHCHECK target.",
                    "responses": {
                        "200": {
                            "description": "Core is ready",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/ReadinessResponse" },
                                "example": readiness_example
                            }}
                        },
                        "503": { "description": "Core is starting up" }
                    }
                }
            },
            "/api/operations": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "Operational health summary",
                    "description": "Returns current counts of detection events, state change events, remediations, device counts, event bus depth, archive stats (bytes, file count), RSS memory usage vs budget, and disk usage. This is the primary health dashboard endpoint.",
                    "responses": { "200": { "description": "Operations snapshot", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/OperationsResponse" },
                        "example": operations_example
                    }}}}
                }
            },
            "/api/operations/daily-check": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "Latest daily check results",
                    "description": "Returns the most recent daily_check.sh result with pass/fail/skip/prereq_missing breakdowns per driver. Used by the AI feedback loop to surface operational regressions.",
                    "responses": { "200": { "description": "Daily check result JSON" }}
                }
            },
            "/api/operations/weekly-trend": {
                "get": {
                    "tags": ["Operations"],
                    "summary": "7-day operational trend",
                    "description": "Returns per-day aggregates of detection counts, remediation outcomes, archive growth, and chaos injection counts for the trailing 7 days. Drives the weekly trend sparklines in the Operations workspace.",
                    "responses": { "200": { "description": "7-day trend data" }}
                }
            },
            "/api/onboarding/devices": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "List managed devices",
                    "description": "Returns all devices currently in the device registry with their gNMI subscription status, collector assignment, health, and last-seen timestamps.",
                    "responses": { "200": { "description": "Managed device list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ManagedDevicesResponse" },
                        "example": managed_devices_example
                    }}}}
                },
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Add a managed device",
                    "description": "Adds a device to the registry with a credential alias and optional role hint. Bonsai will initiate a gNMI Capabilities exchange and subscribe to the paths appropriate for the device's role.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AddDeviceRequest" }}}
                    },
                    "responses": {
                        "200": { "description": "Device added" },
                        "400": { "description": "Invalid request or unreachable device" }
                    }
                }
            },
            "/api/onboarding/devices/with_paths": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Add device with explicit path selection",
                    "description": "Adds a device and immediately applies a specific set of subscription paths (from a prior /api/devices/{address}/recommendations response). Bypasses the auto-discovery step.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Device added with selected paths" }}
                }
            },
            "/api/onboarding/devices/remove": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Remove a managed device",
                    "description": "Removes a device from the registry, cancels active gNMI subscriptions, and removes associated graph nodes. Does not delete historical StateChangeEvents.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }}, "required": ["address"] }}}
                    },
                    "responses": { "200": { "description": "Device removed" }}
                }
            },
            "/api/onboarding/devices/remove-impact": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Preview blast radius before removing a device",
                    "description": "Returns which graph nodes, detection rules, and enrichment linkages would be affected by removing a device. Use before /api/onboarding/devices/remove to understand impact.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }}, "required": ["address"] }}}
                    },
                    "responses": { "200": { "description": "Impact assessment" }}
                }
            },
            "/api/onboarding/devices/bulk": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Bulk device action",
                    "description": "Applies an action (add / remove / reparse) to multiple devices atomically. Returns per-device success/error results.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "action": { "type": "string", "enum": ["add", "remove", "reparse"] }, "addresses": { "type": "array", "items": { "type": "string" }}}}}}
                    },
                    "responses": { "200": { "description": "Per-device results" }}
                }
            },
            "/api/onboarding/discover": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Discovery wizard — connect and probe a device",
                    "description": "Connects to a device via gNMI, exchanges Capabilities, and returns vendor identification, available YANG modules, and recommended subscription paths. This is step 1 of the onboarding wizard; the result feeds into /api/devices/{address}/recommendations.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/DiscoverRequest" },
                            "example": {
                                "address": "172.100.103.31:57400",
                                "credential_alias": "lab-srlinux",
                                "tls_domain": "leaf1",
                                "role_hint": "leaf",
                                "environment_archetype": "data_center"
                            }
                        }}
                    },
                    "responses": {
                        "200": { "description": "Discovery report with vendor, modules, and recommended paths", "content": { "application/json": {
                            "schema": { "type": "object", "additionalProperties": true },
                            "example": onboarding_discover_example
                        }}},
                        "400": { "description": "Unreachable or unsupported device" }
                    }
                }
            },
            "/api/devices/{address}": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Device detail",
                    "description": "Returns full device detail: interfaces, BGP sessions, BFD sessions, IS-IS/OSPF adjacencies, subscription paths, health, enrichment linkages (site, environment, NetBox CI, ServiceNow CI).",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Management IP address" }],
                    "responses": {
                        "200": { "description": "Device detail", "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/DeviceDetailResponse" },
                            "example": device_detail_example
                        }}},
                        "404": { "description": "Device not found in graph" }
                    }
                }
            },
            "/api/devices/{address}/gnmi-readiness": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "gNMI subscription readiness per path",
                    "description": "Returns per-path subscription status for a device: which OpenConfig paths are being streamed, which are absent from Capabilities, and which have known issues (from config/gnmi_known_issues/).",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Per-path readiness report", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DeviceGnmiReadinessResponse" },
                        "example": device_gnmi_readiness_example
                    }}}}
                }
            },
            "/api/devices/{address}/streaming-readiness": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Streaming readiness assessment",
                    "description": "Runs a live gNMI Capabilities exchange and returns a full streaming readiness report: vendor, supported paths, recommended profile, and any blocking issues.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Streaming readiness report", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DeviceStreamingReadinessResponse" },
                        "example": device_streaming_readiness_example
                    }}}}
                }
            },
            "/api/devices/{address}/recommendations": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Path recommendations for a device",
                    "description": "Returns the recommended gNMI subscription path set for this device based on its role, vendor, and discovered YANG capability set. Groups paths by category (interfaces, BGP, OSPF, IS-IS, LLDP, platform).",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Recommended path groups", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DeviceRecommendationsResponse" },
                        "example": device_recommendations_example
                    }}}}
                }
            },
            "/api/devices/{address}/selected-paths": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Apply selected subscription paths",
                    "description": "Persists the operator-selected subscription paths for a device (from the onboarding wizard) and restarts the gNMI subscription with the new path set. This is the commit step of the onboarding wizard.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/ApplySelectedPathsRequest" },
                            "example": {
                                "selected_paths": [
                                    { "path": "/interfaces/interface/state/counters", "origin": "openconfig-interfaces", "reason": "baseline_counters" },
                                    { "path": "/network-instances/network-instance/protocols/protocol/bgp/neighbors/neighbor/state/session-state", "origin": "openconfig-bgp", "reason": "bgp_state" }
                                ]
                            }
                        }}
                    },
                    "responses": { "200": { "description": "Paths applied and subscription restarted", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ApplySelectedPathsResponse" },
                        "example": apply_selected_paths_example
                    }}}}
                }
            },
            "/api/devices/{address}/config-history": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "gNMI subscription path change history",
                    "description": "Returns the history of subscription path configuration changes for a device, including who changed what and when.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Config history" }}
                }
            },
            "/api/devices/{address}/reparse": {
                "post": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Re-parse device state from archive",
                    "description": "Replays archived Parquet telemetry for a device through the ingest pipeline to rebuild graph state. Useful after a schema migration or rule change.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Reparse initiated" }}
                }
            },
            "/api/devices/{address}/enrichment": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Enrichment data for a device",
                    "description": "Returns the business context enrichment for a device: NetBox device/interface records, ServiceNow CI linkages, site assignment, and environment membership.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Enrichment data" }}
                }
            },
            "/api/devices/{address}/enrichment/conflicts": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Enrichment conflicts for a device",
                    "description": "Returns conflicting enrichment properties where the same key is set by multiple sources (e.g. CLI, NetBox, ServiceNow). Includes provenance winner/loser tracking, confidence, and timestamps.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "List of enrichment conflicts grouped by key" }}
                }
            },
            "/api/devices/{address}/cmdb": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "CMDB hierarchy for a device",
                    "description": "Returns the ServiceNow CMDB context for a device: parent/child CI relationships (CMDB_PARENT_OF), business service bindings (RUNS_SERVICE, CARRIES_APPLICATION), and location hierarchy from cmn_location.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "CMDB hierarchy data" }}
                }
            },
            "/api/setup/status": {
                "get": {
                    "tags": ["Devices & Onboarding"],
                    "summary": "Setup wizard completion status",
                    "description": "Returns the first-run status Bonsai uses to decide whether to route a user into the onboarding flow. Current fields reflect whether non-default environments exist and whether credentials or devices have been configured.",
                    "responses": { "200": { "description": "Setup status", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SetupStatusResponse" },
                        "example": setup_status_example
                    }}}}
                }
            },
            "/api/sites": {
                "get": {
                    "tags": ["Sites & Environments"],
                    "summary": "List all sites",
                    "description": "Returns all site records with name, location, and associated device count. Sites are first-class graph entities used for multi-site topology segmentation.",
                    "responses": { "200": { "description": "Site list" }}
                },
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Create or update a site",
                    "description": "Upserts a site record. Site names must be unique. Sites can be assigned to environments.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SiteRecord" }}}
                    },
                    "responses": { "200": { "description": "Site upserted" }}
                }
            },
            "/api/sites/{id}": {
                "get": {
                    "tags": ["Sites & Environments"],
                    "summary": "Site summary with device detail",
                    "description": "Returns detailed site view: all devices assigned to this site with their health, role, vendor, and active detection counts.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Site name or UUID" }],
                    "responses": { "200": { "description": "Site summary" }}
                }
            },
            "/api/sites/remove": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Remove a site",
                    "description": "Removes a site record. Devices assigned to this site become unassigned; they are not removed from monitoring.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" }}, "required": ["id"] }}}
                    },
                    "responses": { "200": { "description": "Site removed" }}
                }
            },
            "/api/environments": {
                "get": {
                    "tags": ["Sites & Environments"],
                    "summary": "List all environments",
                    "description": "Returns all environment records with their archetype (data_center, service_provider, home_lab), assigned sites, and resource governance profile.",
                    "responses": { "200": { "description": "Environment list" }}
                },
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Create an environment",
                    "description": "Creates a new environment with an archetype. The archetype determines default resource governance parameters and path profile selection.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EnvironmentRecord" }}}
                    },
                    "responses": { "200": { "description": "Environment created" }}
                }
            },
            "/api/environments/update": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Update an environment",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EnvironmentRecord" }}}
                    },
                    "responses": { "200": { "description": "Environment updated" }}
                }
            },
            "/api/environments/remove": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Remove an environment",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" }}, "required": ["id"] }}}
                    },
                    "responses": { "200": { "description": "Environment removed" }}
                }
            },
            "/api/environments/assign-site": {
                "post": {
                    "tags": ["Sites & Environments"],
                    "summary": "Assign a site to an environment",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "site_id": { "type": "string" }, "environment_id": { "type": "string" }}, "required": ["site_id", "environment_id"] }}}
                    },
                    "responses": { "200": { "description": "Site assigned" }}
                }
            },
            "/api/yang/modules": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "List available YANG modules",
                    "description": "Returns all YANG modules available in the local YANG library, grouped by source (OpenConfig, vendor-native, universal). Modules are discovered by the discover_yang_paths.py tooling.",
                    "responses": { "200": { "description": "YANG module list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/YangModulesResponse" },
                        "example": yang_modules_example
                    }}}}
                }
            },
            "/api/yang/search": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Search YANG paths",
                    "description": "Full-text search across YANG path names, descriptions, and module names. Returns matching paths with their module, access type (read-only / read-write), and any known gNMI streaming issues.",
                    "parameters": [{ "name": "q", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Search query, e.g. 'bgp neighbor state'" }],
                    "responses": { "200": { "description": "YANG path search results", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/YangSearchResponse" },
                        "example": yang_search_example
                    }}}}
                }
            },
            "/api/profiles": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "List path profiles",
                    "description": "Returns all built-in and custom path profiles (dc_leaf_minimal, dc_spine_standard, sp_pe_full, etc.). Each profile is a named collection of gNMI subscription paths for a device role.",
                    "responses": { "200": { "description": "Profile list", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/ProfilesResponse" },
                        "example": profiles_example
                    }}}}
                }
            },
            "/api/profiles/save-custom": {
                "post": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Save a custom path profile",
                    "description": "Persists a custom path profile to the catalogue directory. Custom profiles are versioned alongside built-in profiles and appear in /api/profiles.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/SaveCustomProfileRequest" },
                            "example": {
                                "name": "dc_leaf_bgp_minimal",
                                "description": "Leaf profile with interface counters and BGP state only",
                                "rationale": "Lean onboarding profile for first-pass lab validation",
                                "environment": ["data_center"],
                                "vendor_scope": ["nokia", "cisco", "juniper", "arista"],
                                "roles": ["leaf"],
                                "paths": [
                                    { "path": "/interfaces/interface/state/counters", "origin": "openconfig-interfaces", "reason": "baseline_counters" },
                                    { "path": "/network-instances/network-instance/protocols/protocol/bgp/neighbors/neighbor/state/session-state", "origin": "openconfig-bgp", "reason": "bgp_state" }
                                ]
                            }
                        }}
                    },
                    "responses": { "200": { "description": "Profile saved", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SaveCustomProfileResponse" },
                        "example": save_custom_profile_example
                    }}}}
                }
            },
            "/api/overrides": {
                "get": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "List gNMI path overrides",
                    "description": "Returns per-device gNMI path overrides — paths that are force-enabled or force-disabled relative to the device's base profile. Overrides survive profile updates.",
                    "responses": { "200": { "description": "Override list" }}
                },
                "post": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Add a path override",
                    "description": "Adds a force-enable or force-disable override for a specific gNMI path on a specific device.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }, "path": { "type": "string" }, "mode": { "type": "string", "enum": ["enable", "disable"] }}, "required": ["address", "path", "mode"] }}}
                    },
                    "responses": { "200": { "description": "Override added" }}
                }
            },
            "/api/overrides/remove": {
                "post": {
                    "tags": ["YANG & Path Profiles"],
                    "summary": "Remove a path override",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }, "path": { "type": "string" }}, "required": ["address", "path"] }}}
                    },
                    "responses": { "200": { "description": "Override removed" }}
                }
            },
            "/api/enrichment": {
                "get": {
                    "tags": ["Enrichment"],
                    "summary": "List enrichment adapters",
                    "description": "Returns all configured enrichment adapters with their type (netbox, servicenow, custom), connection state, last run timestamp, and enrichment statistics (nodes enriched, errors).",
                    "responses": { "200": { "description": "Enricher list" }}
                },
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Add or update an enrichment adapter",
                    "description": "Upserts an enrichment adapter. Supported types: netbox (URL + token alias), servicenow (instance URL + credential alias), custom (Python module path).",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "config": { "type": "object", "properties": { "name": { "type": "string" }, "type": { "type": "string", "enum": ["netbox", "servicenow", "custom"] }, "url": { "type": "string" }, "credential_alias": { "type": "string" }}}}}}}
                    },
                    "responses": { "200": { "description": "Enricher upserted" }}
                }
            },
            "/api/enrichment/remove": {
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Remove an enrichment adapter",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Enricher removed" }}
                }
            },
            "/api/enrichment/test": {
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Test enrichment adapter connectivity",
                    "description": "Verifies that bonsai can reach the enrichment source and authenticate. Does not write any graph state.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Connection test result with success flag and message" }}
                }
            },
            "/api/enrichment/run": {
                "post": {
                    "tags": ["Enrichment"],
                    "summary": "Trigger an enrichment run",
                    "description": "Immediately runs an enrichment cycle for the named adapter outside of the scheduled interval. Returns a run report with nodes enriched and any errors.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Enrichment run report" }}
                }
            },
            "/api/enrichment/audit": {
                "get": {
                    "tags": ["Enrichment"],
                    "summary": "Enrichment audit log",
                    "description": "Returns the enrichment audit log: every enrichment run with timestamp, adapter name, outcome, nodes written, and any errors.",
                    "responses": { "200": { "description": "Audit log entries" }}
                }
            },
            "/api/adapters": {
                "get": {
                    "tags": ["Output Adapters"],
                    "summary": "List output adapters",
                    "description": "Returns all configured output adapters with their type (splunk_hec, elasticsearch, servicenow_em, prometheus), run state, cursor position, and last push statistics.",
                    "responses": { "200": { "description": "Adapter list" }}
                },
                "post": {
                    "tags": ["Output Adapters"],
                    "summary": "Add or update an output adapter",
                    "description": "Upserts an output adapter. Supported types: splunk_hec (HEC URL + token alias), elasticsearch (URL + credential alias), servicenow_em (instance URL + credential alias), prometheus (remote-write URL).",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Adapter upserted" }}
                }
            },
            "/api/adapters/remove": {
                "post": {
                    "tags": ["Output Adapters"],
                    "summary": "Remove an output adapter",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Adapter removed" }}
                }
            },
            "/api/adapters/test": {
                "post": {
                    "tags": ["Output Adapters"],
                    "summary": "Test output adapter connectivity",
                    "description": "Verifies that bonsai can reach the output destination and authenticate. Sends a test payload; does not affect the cursor position.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }}, "required": ["name"] }}}
                    },
                    "responses": { "200": { "description": "Test result" }}
                }
            },
            "/api/adapters/audit": {
                "get": {
                    "tags": ["Output Adapters"],
                    "summary": "Output adapter audit log",
                    "description": "Returns push history per adapter: timestamp, records pushed, bytes sent, errors, and cursor position.",
                    "responses": { "200": { "description": "Audit log entries" }}
                }
            },
            "/api/credentials": {
                "get": {
                    "tags": ["Credentials"],
                    "summary": "List credential aliases",
                    "description": "Returns all credential aliases stored in the vault. Never returns plaintext credentials — only alias names, associated device count, and last-used timestamp.",
                    "responses": { "200": { "description": "Credential alias list" }}
                },
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Add a credential",
                    "description": "Stores a new credential in the age-encrypted vault under an alias. The request body contains the alias name and env var names that hold the plaintext username/password — plaintext values must never appear in the request body.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }, "username_env": { "type": "string", "description": "Env var name containing the username" }, "password_env": { "type": "string", "description": "Env var name containing the password" }}, "required": ["alias", "username_env", "password_env"] }}}
                    },
                    "responses": { "200": { "description": "Credential stored" }}
                }
            },
            "/api/credentials/update": {
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Update an existing credential",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }, "username_env": { "type": "string" }, "password_env": { "type": "string" }}, "required": ["alias"] }}}
                    },
                    "responses": { "200": { "description": "Credential updated" }}
                }
            },
            "/api/credentials/remove": {
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Remove a credential alias",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }}, "required": ["alias"] }}}
                    },
                    "responses": { "200": { "description": "Credential removed" }}
                }
            },
            "/api/credentials/test": {
                "post": {
                    "tags": ["Credentials"],
                    "summary": "Test a credential against a device",
                    "description": "Attempts a gNMI Capabilities RPC using the stored credential to verify it is valid for the target device.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "alias": { "type": "string" }, "address": { "type": "string" }}, "required": ["alias", "address"] }}}
                    },
                    "responses": { "200": { "description": "Test result with success flag" }}
                }
            },
            "/api/approvals": {
                "get": {
                    "tags": ["Trust & Approvals"],
                    "summary": "List pending remediation approvals",
                    "description": "Returns all pending RemediationProposals awaiting operator approval. Each proposal includes the proposed gNMI Set command, the triggering DetectionEvent, and the estimated blast radius.",
                    "responses": { "200": { "description": "Pending approval list" }}
                },
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Create a manual remediation proposal",
                    "description": "Creates a manual remediation proposal for operator review. Used when an operator wants to test the approval workflow with a specific remediation action.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Proposal created" }}
                }
            },
            "/api/approvals/{id}/approve": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Approve a remediation proposal",
                    "description": "Approves a remediation proposal, triggering execution via gNMI Set. Each approval increments the consecutive-success counter toward graduated autonomous trust.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": {
                        "200": { "description": "Approved and executing" },
                        "404": { "description": "Proposal not found" }
                    }
                }
            },
            "/api/approvals/{id}/reject": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Reject a remediation proposal",
                    "description": "Rejects a remediation proposal and resets the consecutive-success counter for this rule+device, preventing graduation to autonomous trust.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": { "200": { "description": "Rejected" }}
                }
            },
            "/api/approvals/{id}/rollback": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Rollback an executed remediation",
                    "description": "Issues a gNMI Set to undo the effect of an already-executed remediation. Marks the rollback window as used; a given remediation can only be rolled back once.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": {
                        "200": { "description": "Rollback initiated" },
                        "409": { "description": "Rollback window expired or already used" }
                    }
                }
            },
            "/api/trust": {
                "get": {
                    "tags": ["Trust & Approvals"],
                    "summary": "List trust records",
                    "description": "Returns the graduated trust state for every rule+device combination: current trust level (manual_only, auto_with_notification, auto_silent), consecutive success count, and graduation threshold.",
                    "responses": { "200": { "description": "Trust record list" }}
                }
            },
            "/api/trust/graduate": {
                "post": {
                    "tags": ["Trust & Approvals"],
                    "summary": "Manually graduate a trust record",
                    "description": "Forces a trust record to a higher trust level without waiting for the consecutive-success threshold. Requires operator intent to be explicit.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "rule_id": { "type": "string" }, "device_address": { "type": "string" }, "target_level": { "type": "string", "enum": ["manual_only", "auto_with_notification", "auto_silent"] }}, "required": ["rule_id", "device_address", "target_level"] }}}
                    },
                    "responses": { "200": { "description": "Trust record graduated" }}
                }
            },
            "/api/collectors": {
                "get": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "List distributed collectors",
                    "description": "Returns all collector instances registered with the core: address, runtime mode, last heartbeat, device count, and queue statistics.",
                    "responses": { "200": { "description": "Collector list" }}
                }
            },
            "/api/assignment/rules": {
                "get": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "List collector assignment rules",
                    "description": "Returns the ordered list of assignment rules that determine which collector handles which devices. Rules are evaluated in order; first match wins.",
                    "responses": { "200": { "description": "Assignment rule list" }}
                },
                "post": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "Replace collector assignment rules",
                    "description": "Replaces the full assignment rule set atomically. All devices are re-evaluated against the new rules and reassigned if necessary.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "rules": { "type": "array", "items": { "type": "object" }}}}}}
                    },
                    "responses": { "200": { "description": "Rules replaced, reassignment triggered" }}
                }
            },
            "/api/assignment/status": {
                "get": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "Current device-to-collector assignment status",
                    "description": "Returns per-device collector assignment: which collector is handling each device, the assignment rule that matched, and any assignment warnings.",
                    "responses": { "200": { "description": "Assignment status" }}
                }
            },
            "/api/assignment/override": {
                "post": {
                    "tags": ["Collectors & Assignment"],
                    "summary": "Override collector assignment for a device",
                    "description": "Forces a specific device to a specific collector regardless of assignment rules. Overrides are persisted and survive restarts.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "address": { "type": "string" }, "collector_id": { "type": "string" }}, "required": ["address", "collector_id"] }}}
                    },
                    "responses": { "200": { "description": "Override applied" }}
                }
            },
            "/api/graph/insights": {
                "get": {
                    "tags": ["Graph Explorer"],
                    "summary": "Graph structure insights",
                    "description": "Returns high-level graph statistics: node counts by label, edge counts by type, graph density, average degree, and any structural anomalies (isolated nodes, missing enrichment linkages).",
                    "responses": { "200": { "description": "Graph insights" }}
                }
            },
            "/api/explorer/ask": {
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Natural language graph query",
                    "description": "Accepts a plain English question about the network, generates a read-only Cypher query using an LLM (Anthropic Claude), executes it against LadybugDB, and returns the generated Cypher, explanation, and result rows. Requires ANTHROPIC_API_KEY environment variable.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "question": { "type": "string", "description": "Natural language question, e.g. 'Which servers are connected to spine1?'" }}, "required": ["question"] }}}
                    },
                    "responses": {
                        "200": { "description": "Generated Cypher, explanation, and result rows" },
                        "400": { "description": "Empty question" },
                        "503": { "description": "ANTHROPIC_API_KEY not set" }
                    }
                }
            },
            "/api/explorer/nl-budget": {
                "get": {
                    "tags": ["Graph Explorer"],
                    "summary": "NL query token budget status",
                    "description": "Returns the daily token usage and limit for natural language graph queries.",
                    "responses": { "200": { "description": "Token usage and daily limit" }}
                }
            },
            "/api/explorer/query": {
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Execute a Cypher query",
                    "description": "Executes a read-only Cypher query against the LadybugDB graph. Mutations (CREATE, MERGE, SET, DELETE) are rejected. Returns rows as JSON arrays.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "query": { "type": "string", "description": "Cypher query string, e.g. MATCH (d:Device) RETURN d.address, d.hostname" }}, "required": ["query"] }}}
                    },
                    "responses": {
                        "200": { "description": "Query result rows" },
                        "400": { "description": "Invalid or disallowed Cypher" }
                    }
                }
            },
            "/api/explorer/saved-queries": {
                "get": {
                    "tags": ["Graph Explorer"],
                    "summary": "List saved Cypher queries",
                    "description": "Returns saved queries with their name, Cypher text, description, and last-run timestamp.",
                    "responses": { "200": { "description": "Saved query list" }}
                },
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Save a Cypher query",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }, "query": { "type": "string" }, "description": { "type": "string" }}, "required": ["name", "query"] }}}
                    },
                    "responses": { "200": { "description": "Query saved" }}
                }
            },
            "/api/explorer/saved-queries/{id}/delete": {
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Delete a saved query",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Query deleted" }}
                }
            },
            "/api/graph/embeddings/upsert": {
                "post": {
                    "tags": ["Graph Explorer"],
                    "summary": "Upsert node embeddings",
                    "description": "Stores vector embeddings for graph nodes (Device, Interface, BgpNeighbor). Used by the GNN training pipeline to persist learned representations.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" }}}
                    },
                    "responses": { "200": { "description": "Embeddings stored" }}
                }
            },
            "/api/graph/embeddings/{address}": {
                "get": {
                    "tags": ["Graph Explorer"],
                    "summary": "Get embeddings for a device",
                    "description": "Returns stored vector embeddings for a device and its associated interface and BGP neighbor nodes.",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" }}],
                    "responses": { "200": { "description": "Embedding vectors" }}
                }
            },
            "/api/investigations": {
                "get": {
                    "tags": ["Investigations"],
                    "summary": "List investigations",
                    "description": "Returns all investigation sessions with their status (open, complete), associated detection IDs, and tool-call counts.",
                    "responses": { "200": { "description": "Investigation list" }}
                },
                "post": {
                    "tags": ["Investigations"],
                    "summary": "Create an investigation",
                    "description": "Opens a new AI-assisted investigation session anchored to a DetectionEvent or a natural-language problem statement. The session accumulates tool calls and findings.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "detection_id": { "type": "string" }, "problem": { "type": "string" }}}}}
                    },
                    "responses": { "200": { "description": "Investigation created with ID" }}
                }
            },
            "/api/investigations/{id}": {
                "get": {
                    "tags": ["Investigations"],
                    "summary": "Get investigation detail",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": {
                        "200": { "description": "Investigation detail" },
                        "404": { "description": "Investigation not found" }
                    }
                }
            },
            "/api/investigations/{id}/tool-calls": {
                "get": {
                    "tags": ["Investigations"],
                    "summary": "List tool calls for an investigation",
                    "description": "Returns the ordered audit trail of tool calls made during an investigation session: tool name, input, output, and timestamp.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "responses": { "200": { "description": "Tool call audit trail" }}
                }
            },
            "/api/investigations/{id}/complete": {
                "post": {
                    "tags": ["Investigations"],
                    "summary": "Complete an investigation",
                    "description": "Closes an investigation session with a summary finding and optional recommended action.",
                    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }}],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "finding": { "type": "string" }, "recommended_action": { "type": "string" }}, "required": ["finding"] }}}
                    },
                    "responses": { "200": { "description": "Investigation closed" }}
                }
            },
            "/api/integrations/servicenow/test": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Test ServiceNow connectivity",
                    "description": "Verifies bonsai can reach the ServiceNow instance and authenticate with the configured credential alias. Checks EM (Event Management) and CMDB table access.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/SnowTestRequest" },
                            "example": {
                                "instance_url": "https://dev394753.service-now.com",
                                "credential_alias": "servicenow-pdi"
                            }
                        }}
                    },
                    "responses": { "200": { "description": "Connectivity test result", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SnowTestResponse" },
                        "example": servicenow_test_example
                    }}}}
                }
            },
            "/api/integrations/servicenow/aiops/sync": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Sync topology to ServiceNow AIOps",
                    "description": "Pushes current bonsai topology graph to ServiceNow CMDB as CI records with CONNECTED_TO relationships. Idempotent — existing CIs are updated in place.",
                    "responses": { "200": { "description": "Sync report with CI counts", "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/SnowAiopsSyncResponse" },
                        "example": servicenow_sync_example
                    }}}}
                }
            },
            "/api/governance/state": {
                "get": {
                    "tags": ["Governance"],
                    "summary": "Adaptive resource governance state",
                    "description": "Returns current governance profile, active policies (write_pressure_active, memory_pressure_active, load_shedding), shedding statistics, and recent governance actions. Memory at >90% of budget triggers memory_pressure_active.",
                    "responses": { "200": { "description": "Governance state" }}
                }
            },
            "/api/_test/status": {
                "get": {
                    "tags": ["Test & Verification"],
                    "summary": "Test driver health status",
                    "description": "Returns the results of all registered test drivers (api_driver, event_driver, ui_driver). Used by the Gemini AI feedback loop to surface test regressions. Each driver result includes pass/fail/skip counts and a last-run timestamp.",
                    "responses": { "200": { "description": "Test driver status aggregation" }}
                }
            },
            "/api/_test/inject_detection": {
                "post": {
                    "tags": ["Test & Verification"],
                    "summary": "Inject a synthetic detection event",
                    "description": "Publishes a synthetic DetectionEvent on the event bus for testing the remediation and output adapter pipelines. The event is written to the graph and flows through the full detect-heal loop. Do not use in production.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "rule_id": { "type": "string" }, "device_address": { "type": "string" }, "severity": { "type": "string", "enum": ["critical", "warn", "info"] }}, "required": ["rule_id", "device_address"] }}}
                    },
                    "responses": { "200": { "description": "Detection injected" }}
                }
            },
            "/api/_test/syslog/parse": {
                "post": {
                    "tags": ["Test & Verification"],
                    "summary": "Parse one syslog fixture",
                    "description": "Internal parser-validation endpoint used by the fixture-driven syslog smoke. Parses one raw syslog line, extracts SyslogFacts using the configured vendor pattern catalogue, and reports whether the line matches a config-change trigger pattern.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "raw": { "type": "string" }, "vendor": { "type": "string" }, "transport": { "type": "string" }, "peer_addr": { "type": "string" }}, "required": ["raw", "vendor"] }}}
                    },
                    "responses": { "200": { "description": "Parsed syslog event plus extracted facts" }}
                }
            },
            "/mcp": {
                "post": {
                    "tags": ["MCP"],
                    "summary": "MCP JSON-RPC 2.0 endpoint",
                    "description": "Model Context Protocol server for AI agent tool use. Supports initialize, tools/list, and tools/call. Available tools: get_incident (fetch grounded incident), query_devices (filter device list), get_device_blast_radius (impact assessment), list_active_detections (current anomalies), query_graph (read-only Cypher). Binds to localhost only; not exposed externally.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": {
                            "type": "object",
                            "properties": {
                                "jsonrpc": { "type": "string", "enum": ["2.0"] },
                                "id": { "description": "Request ID (integer, string, or null)" },
                                "method": { "type": "string", "enum": ["initialize", "tools/list", "tools/call"] },
                                "params": { "type": "object" }
                            },
                            "required": ["jsonrpc", "id", "method"]
                        }}}
                    },
                    "responses": { "200": { "description": "JSON-RPC 2.0 response object" }}
                }
            },
            "/api/schema": {
                "get": {
                    "tags": ["Schema"],
                    "summary": "OpenAPI 3 specification (legacy path)",
                    "description": "Returns the full OpenAPI 3 specification. Prefer /api/openapi.json which is the canonical path served by the Swagger UI infrastructure.",
                    "responses": { "200": { "description": "OpenAPI 3 JSON" }}
                }
            },
            "/api/openapi.json": {
                "get": {
                    "tags": ["Schema"],
                    "summary": "OpenAPI 3 specification",
                    "description": "Returns the full OpenAPI 3 specification for all bonsai endpoints. Served by utoipa-swagger-ui and consumed by /api/docs. Enables agents and tooling to introspect bonsai without prior knowledge.",
                    "responses": { "200": { "description": "OpenAPI 3 JSON" }}
                }
            },
            "/api/resolve": {
                "get": {
                    "tags": ["Schema"],
                    "summary": "Natural-language reference resolution",
                    "description": "Resolves a natural-language query to stable bonsai IDs. Returns candidate devices, detections, and rules ranked by match confidence. Designed for AI agent sessions to convert informal references (e.g. 'spine1', 'that BGP issue') to API-addressable UUIDs and addresses.",
                    "parameters": [{ "name": "q", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Query string, e.g. 'spine1', 'BGP issue last night', 'bgp_session_down'" }],
                    "responses": { "200": { "description": "Resolution candidates with confidence scores" }}
                }
            }
        },
        "components": super::schema_components::schema_components()
    })
}

fn load_openapi_example(name: &str) -> serde_json::Value {
    let live_path = std::path::Path::new("docs")
        .join("openapi")
        .join("examples")
        .join("live")
        .join(format!("{name}.json"));

    if let Ok(raw) = std::fs::read_to_string(&live_path)
        && let Ok(value) = serde_json::from_str(&raw)
    {
        return value;
    }

    let raw = match name {
        "topology" => include_str!("../../docs/openapi/examples/topology.json"),
        "detections" => include_str!("../../docs/openapi/examples/detections.json"),
        "incidents" => include_str!("../../docs/openapi/examples/incidents.json"),
        "readiness" => include_str!("../../docs/openapi/examples/readiness.json"),
        "operations" => include_str!("../../docs/openapi/examples/operations.json"),
        "grounded_incident" => include_str!("../../docs/openapi/examples/grounded_incident.json"),
        "managed_devices" => include_str!("../../docs/openapi/examples/managed_devices.json"),
        "onboarding_discover" => include_str!("../../docs/openapi/examples/onboarding_discover.json"),
        "device_detail" => include_str!("../../docs/openapi/examples/device_detail.json"),
        "device_gnmi_readiness" => {
            include_str!("../../docs/openapi/examples/device_gnmi_readiness.json")
        }
        "device_streaming_readiness" => {
            include_str!("../../docs/openapi/examples/device_streaming_readiness.json")
        }
        "device_recommendations" => {
            include_str!("../../docs/openapi/examples/device_recommendations.json")
        }
        "apply_selected_paths" => {
            include_str!("../../docs/openapi/examples/apply_selected_paths.json")
        }
        "setup_status" => include_str!("../../docs/openapi/examples/setup_status.json"),
        "yang_modules" => include_str!("../../docs/openapi/examples/yang_modules.json"),
        "yang_search" => include_str!("../../docs/openapi/examples/yang_search.json"),
        "profiles" => include_str!("../../docs/openapi/examples/profiles.json"),
        "save_custom_profile" => include_str!("../../docs/openapi/examples/save_custom_profile.json"),
        "servicenow_test" => include_str!("../../docs/openapi/examples/servicenow_test.json"),
        "servicenow_aiops_sync" => {
            include_str!("../../docs/openapi/examples/servicenow_aiops_sync.json")
        }
        _ => "{}",
    };

    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}))
}
