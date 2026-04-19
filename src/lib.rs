pub mod api;
pub mod config;
pub mod gnmi_set;
pub mod graph;
pub mod registry;
pub mod retention;
pub mod subscriber;
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
