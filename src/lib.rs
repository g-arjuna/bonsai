pub mod api;
pub mod archive;
pub mod assignment;
pub mod audit;
pub mod catalogue;
pub mod change_detection;
pub mod collector;
pub mod config;
pub mod config_store;
pub mod counter_summarizer;
pub mod credentials;
pub mod discovery;
pub mod disk_guard;
pub mod enrichment;
pub mod event_bus;
pub mod gnmi_set;
pub mod graph;
pub mod http_server;
pub mod ingest;
pub mod mcp_client;
pub mod memory_profile;
pub mod output;
pub mod parser_chain;
pub mod registry;
pub mod remediation;
pub mod retention;
pub mod signals;
pub mod store;
pub mod subscriber;
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
