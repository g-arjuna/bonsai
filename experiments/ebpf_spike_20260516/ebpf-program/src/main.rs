#![no_std]
#![no_main]

use aya_bpf::{
    macros::{map, tracepoint},
    maps::HashMap,
    programs::TracePointContext,
};
use aya_log_ebpf::debug;

/// Per-(ifindex, drop_reason) drop counter.
/// Key: upper 32 bits = ifindex, lower 32 bits = drop_reason code.
/// Value: u64 packet count.
#[map]
static mut DROP_COUNTS: HashMap<u64, u64> = HashMap::with_max_entries(1024, 0);

/// kfree_skb tracepoint context layout (from linux/skbuff.h):
///   offset 0:  *skbaddr   (u64)
///   offset 8:  *location  (u64)
///   offset 16: protocol   (u16)
///   offset 18: reason     (u8)  — drop_reason enum
///
/// We extract ifindex from skb->dev->ifindex via the skb pointer.
/// For simplicity in the spike, we use reason only and ifindex=0 as a placeholder.
/// A production version uses bpf_probe_read_kernel to dereference skb→dev→ifindex.
#[tracepoint(name = "kfree_skb_drop", category = "skb")]
pub fn kfree_skb_drop(ctx: TracePointContext) -> u32 {
    match try_kfree_skb(&ctx) {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

fn try_kfree_skb(ctx: &TracePointContext) -> Result<u32, i64> {
    // Read drop_reason from tracepoint context at offset 18 (u8).
    let reason: u8 = unsafe { ctx.read_at(18)? };

    // In a full version: read skb pointer, then skb->dev->ifindex.
    // Spike uses ifindex=0 as a placeholder for all interfaces.
    let ifindex: u32 = 0;

    let key: u64 = ((ifindex as u64) << 32) | (reason as u64);

    // Increment the counter atomically.
    unsafe {
        let count = DROP_COUNTS.get(&key).copied().unwrap_or(0);
        let _ = DROP_COUNTS.insert(&key, &(count + 1), 0);
    }

    debug!(ctx, "kfree_skb: ifindex={} reason={}", ifindex, reason);
    Ok(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
