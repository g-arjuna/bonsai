use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use aya::maps::HashMap as BpfHashMap;
use aya::programs::TracePoint;
use aya::{include_bytes_aligned, Bpf};
use aya_log::BpfLogger;
use log::{info, warn};
use tokio::time;

mod bonsai_bridge;

/// Key stored in the BPF drop-counter HashMap:
///   ifindex (u32) | drop_reason (u32)  — packed as u64.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct DropKey {
    ifindex: u32,
    reason: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    // Load the compiled eBPF object.
    // The bytes are embedded at compile time via include_bytes_aligned!.
    #[cfg(target_os = "linux")]
    let mut bpf = {
        let bytes = include_bytes_aligned!(
            "../../ebpf-program/target/bpfel-unknown-none/release/ebpf_drop_counter"
        );
        Bpf::load(bytes).context("failed to load eBPF program")?
    };

    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("eBPF is Linux-only. This binary compiled successfully but cannot load.");
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = BpfLogger::init(&mut bpf) {
            warn!("eBPF logger init failed (non-fatal): {e}");
        }

        let program: &mut TracePoint = bpf
            .program_mut("kfree_skb_drop")
            .context("program not found")?
            .try_into()?;
        program.load()?;
        program.attach("skb", "kfree_skb")?;
        info!("kfree_skb tracepoint attached");

        let mut poll_interval = time::interval(Duration::from_secs(5));

        loop {
            poll_interval.tick().await;

            let map: BpfHashMap<_, u64, u64> = BpfHashMap::try_from(
                bpf.map("DROP_COUNTS").context("DROP_COUNTS map not found")?,
            )?;

            let mut by_ifindex: HashMap<u32, Vec<(u32, u64)>> = HashMap::new();
            for item in map.iter() {
                let (key_raw, count) = item?;
                let ifindex = (key_raw >> 32) as u32;
                let reason = (key_raw & 0xFFFF_FFFF) as u32;
                by_ifindex.entry(ifindex).or_default().push((reason, count));

                // Forward to bonsai InProcessBus stub.
                bonsai_bridge::emit_drop_event(ifindex, reason, count);
            }

            for (ifindex, reasons) in &by_ifindex {
                let total: u64 = reasons.iter().map(|(_, c)| c).sum();
                let detail: Vec<String> = reasons
                    .iter()
                    .map(|(r, c)| format!("reason_{r}={c}"))
                    .collect();
                println!(
                    "[ifindex={ifindex}] drops={total}  ({})",
                    detail.join(", ")
                );
            }
        }
    }
}
