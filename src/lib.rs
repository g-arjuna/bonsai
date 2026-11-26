#![recursion_limit = "512"]

pub mod ai_provider;
pub mod api;
pub mod archive;
pub mod assignment;
pub mod audit;
pub mod auth_store;
pub mod catalogue;
pub mod change_detection;
pub mod collector;
pub mod collector_auth;
pub mod config;
pub mod config_store;
pub mod correlation_buffer;
pub mod counter_summarizer;
pub mod credentials;
pub mod discovery;
pub mod disk_guard;
pub mod enrichment;
pub mod event_bus;
pub mod forwarding_domain_derivation;
pub mod frr_interface_json;
pub mod gnmi_path;
pub mod gnmi_security;
pub mod gnmi_set;
pub mod graph;
pub mod ha_coordinator;
pub mod health_emitter;
pub mod http_server;
pub mod ingest;
pub mod integrations;
pub mod interface_facts;
pub mod interface_normalizer;
pub mod interface_relationships;
pub mod investigation_runtime;
pub mod investigation_trigger;
pub mod lldp_facts;
pub mod lldp_normalizer;
pub mod lldp_relationships;
pub mod mcp_client;
pub mod mcp_server;
pub mod memory_profile;
pub mod ml_event_bus;
pub mod neighbor_facts;
pub mod neighbor_normalizer;
pub mod neighbor_reconciliation;
pub mod normalizer_registry;
pub mod nos_features;
pub mod output;
pub mod parser_chain;
pub mod platform_registry;
pub mod playbook;
pub mod receiver_supervisor;
pub mod reconciler;
pub mod registry;
pub mod remediation;
pub mod resource_governor;
pub mod resource_profile;
pub mod retention;
pub mod routing_service;
pub mod schema_binding;
pub mod schema_coverage;
pub mod security;
pub mod semantic_evidence;
pub mod service_discovery;
pub mod shun;
pub mod sidecar_manager;
pub mod sidecar_registry;
pub mod signal_filter;
pub mod signals;
pub mod sqlite_store;
pub mod store;
pub mod streaming;
pub mod subscriber;
pub mod subscription_sessions;
pub mod subscription_status;
pub mod synthesizer;
pub mod telemetry;
pub mod telemetry_identity;
pub mod tls_util;
pub mod typed_facts;
pub mod vlan_facts;
pub mod vlan_normalizer;
pub mod write_coordinator;
pub mod xrd_openconfig_interfaces;
pub mod yang;
pub mod yang_applicability;
pub mod yang_discovery;
pub mod yang_library;
pub mod yang_schema;
pub mod yang_source;

pub use async_stream;
pub use async_trait;

pub mod proto {
    pub mod gnmi {
        #![allow(clippy::all)]
        tonic::include_proto!("gnmi");
    }
    pub mod gnmi_ext {
        #![allow(clippy::all)]
        tonic::include_proto!("gnmi_ext");
    }
}
