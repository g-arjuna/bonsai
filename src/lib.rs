pub mod api;
pub mod audit;
pub mod output;
pub mod remediation;
pub mod archive;
pub mod assignment;
pub mod enrichment;
pub mod catalogue;
pub mod mcp_client;
pub mod collector;
pub mod config;
pub mod counter_summarizer;
pub mod credentials;
pub mod discovery;
pub mod event_bus;
pub mod gnmi_set;
pub mod graph;
pub mod http_server;
pub mod ingest;
pub mod registry;
pub mod retention;
pub mod store;
pub mod subscriber;
pub mod subscription_status;
pub mod telemetry;

pub use async_trait;
pub use async_stream;


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
