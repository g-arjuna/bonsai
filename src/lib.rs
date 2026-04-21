pub mod api;
pub mod archive;
pub mod config;
pub mod credentials;
pub mod discovery;
pub mod event_bus;
pub mod gnmi_set;
pub mod graph;
pub mod http_server;
pub mod ingest;
pub mod registry;
pub mod retention;
pub mod subscriber;
pub mod subscription_status;
pub mod telemetry;

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
