/// Stub integration bridge: converts eBPF drop-counter map entries into the
/// `TelemetryEvent` shape that bonsai's `InProcessBus` expects.
///
/// In a full integration (DV2), this would import `bonsai::event_bus::InProcessBus`
/// and call `bus.publish(BonsaiEvent::Telemetry(...))`. For the spike, it prints
/// a structured log line matching the expected event format.
///
/// Gated on Linux: on Mac the function compiles but is a no-op.

#[allow(unused_variables)]
pub fn emit_drop_event(ifindex: u32, drop_reason: u32, count: u64) {
    #[cfg(target_os = "linux")]
    {
        // In DV2 this becomes:
        //   bus.publish(BonsaiEvent::Telemetry(TelemetryUpdate {
        //       target: resolve_ifindex(ifindex),
        //       path: format!("ebpf/drops/if{ifindex}/reason{drop_reason}"),
        //       value: serde_json::json!({ "count": count }),
        //       timestamp_ns: now_ns(),
        //   }));
        log::debug!(
            target: "bonsai_bridge",
            "drop_event ifindex={ifindex} reason={drop_reason} count={count}"
        );
    }
}

/// Resolve a Linux ifindex to an interface name via `/sys/class/net/`.
/// Returns "unknown" if the ifindex doesn't match any interface.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn resolve_ifindex(ifindex: u32) -> String {
    use std::fs;
    for entry in fs::read_dir("/sys/class/net/").into_iter().flatten().flatten() {
        let path = entry.path().join("ifindex");
        if let Ok(content) = fs::read_to_string(&path) {
            if content.trim().parse::<u32>().ok() == Some(ifindex) {
                return entry.file_name().to_string_lossy().into_owned();
            }
        }
    }
    format!("if{ifindex}")
}
