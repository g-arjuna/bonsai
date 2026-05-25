#![recursion_limit = "512"]

pub mod ai_provider;
pub mod api;
pub mod archive;
pub mod assignment;
pub mod audit;
pub mod catalogue;
pub mod change_detection;
pub mod correlation_buffer;
pub mod collector;
pub mod config;
pub mod config_store;
pub mod counter_summarizer;
pub mod sqlite_store;
pub mod credentials;
pub mod discovery;
pub mod disk_guard;
pub mod enrichment;
pub mod event_bus;
pub mod gnmi_set;
pub mod graph;
pub mod service_discovery;
pub mod security;
pub mod ha_coordinator;
pub mod health_emitter;
pub mod http_server;
pub mod ingest;
pub mod investigation_runtime;
pub mod investigation_trigger;
pub mod integrations;
pub mod mcp_client;
pub mod mcp_server;
pub mod memory_profile;
pub mod output;
pub mod parser_chain;
pub mod playbook;
pub mod receiver_supervisor;
pub mod reconciler;
pub mod registry;
pub mod remediation;
pub mod resource_governor;
pub mod resource_profile;
pub mod retention;
pub mod sidecar_registry;
pub mod shun;
pub mod signals;
pub mod store;
pub mod streaming;
pub mod subscriber;
pub mod tls_util;
pub mod subscription_status;
pub mod synthesizer;
pub mod telemetry;
pub mod write_coordinator;
pub mod yang;

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
