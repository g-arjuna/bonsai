/// T4-1 — Environment probe: classify the runtime host into a ResourceProfile
/// and derive safe default tuning parameters for every subsystem.
///
/// Operator config always takes precedence; these defaults fill in the gaps
/// for un-configured deployments (e.g. fresh installs, CI, small VMs).
use std::path::Path;
use tracing::info;

// ── Profile enum ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceProfile {
    /// <2 GB RAM or strong cgroup cap. CI / edge / tiny VM.
    Tiny,
    /// 2–6 GB RAM. Small cloud instance or laptop dev.
    Small,
    /// 6–14 GB RAM. Mid-size server or workstation.
    Medium,
    /// 14–30 GB RAM. Dedicated server.
    Large,
    /// >30 GB RAM. Production server or large workstation.
    XLarge,
}

impl ResourceProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::XLarge => "xlarge",
        }
    }

    /// Derive the profile from available system RAM.
    fn from_ram(ram_bytes: u64) -> Self {
        let gb = ram_bytes / (1024 * 1024 * 1024);
        match gb {
            0..=1 => Self::Tiny,
            2..=5 => Self::Small,
            6..=13 => Self::Medium,
            14..=29 => Self::Large,
            _ => Self::XLarge,
        }
    }

    /// Profile-specific subsystem defaults.
    pub fn defaults(&self) -> ProfileDefaults {
        match self {
            Self::Tiny => ProfileDefaults {
                memory_budget_bytes: 256 * 1024 * 1024,       // 256 MB
                lru_cache_bytes: 8 * 1024 * 1024,             // 8 MB
                write_coordinator_batch_size: 64,
                event_bus_capacity: 512,
                archive_flush_interval_secs: 30,
                rate_budget_events_per_sec: 500,
            },
            Self::Small => ProfileDefaults {
                memory_budget_bytes: 512 * 1024 * 1024,       // 512 MB
                lru_cache_bytes: 32 * 1024 * 1024,            // 32 MB
                write_coordinator_batch_size: 128,
                event_bus_capacity: 2048,
                archive_flush_interval_secs: 20,
                rate_budget_events_per_sec: 2_000,
            },
            Self::Medium => ProfileDefaults {
                memory_budget_bytes: 1024 * 1024 * 1024,      // 1 GB
                lru_cache_bytes: 128 * 1024 * 1024,           // 128 MB
                write_coordinator_batch_size: 256,
                event_bus_capacity: 8192,
                archive_flush_interval_secs: 10,
                rate_budget_events_per_sec: 10_000,
            },
            Self::Large => ProfileDefaults {
                memory_budget_bytes: 2 * 1024 * 1024 * 1024,  // 2 GB
                lru_cache_bytes: 512 * 1024 * 1024,           // 512 MB
                write_coordinator_batch_size: 512,
                event_bus_capacity: 16384,
                archive_flush_interval_secs: 10,
                rate_budget_events_per_sec: 50_000,
            },
            Self::XLarge => ProfileDefaults {
                memory_budget_bytes: 4 * 1024 * 1024 * 1024,  // 4 GB
                lru_cache_bytes: 1024 * 1024 * 1024,          // 1 GB
                write_coordinator_batch_size: 1024,
                event_bus_capacity: 32768,
                archive_flush_interval_secs: 10,
                rate_budget_events_per_sec: 200_000,
            },
        }
    }
}

// ── Profile defaults ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProfileDefaults {
    /// RSS budget before governance actions kick in.
    pub memory_budget_bytes: u64,
    /// Total bytes budget for all LRU debounce caches.
    pub lru_cache_bytes: u64,
    /// Write coordinator batch size (updates per transaction).
    pub write_coordinator_batch_size: usize,
    /// Event bus channel capacity (buffered events).
    pub event_bus_capacity: usize,
    /// Archive flush interval in seconds.
    pub archive_flush_interval_secs: u64,
    /// Aggregate inbound event rate budget (all sources combined) events/second.
    pub rate_budget_events_per_sec: u64,
}

// ── Environment probe ─────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct ProbeResult {
    pub profile: ResourceProfile,
    pub ram_bytes: u64,
    pub cpu_cores: usize,
    pub disk_free_bytes: u64,
    pub cgroup_memory_limit_bytes: Option<u64>,
    pub in_container: bool,
    pub defaults: ProfileDefaults,
}

/// Probe the runtime environment and return a `ProbeResult`.
/// Errors reading system info are non-fatal; the probe falls back to conservative values.
pub fn probe(archive_path: &Path, log_path: &Path) -> ProbeResult {
    let ram_bytes = available_ram_bytes();
    let cgroup_limit = cgroup_memory_limit();
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let disk_free_bytes = disk_free_at(archive_path)
        .or_else(|| disk_free_at(log_path))
        .unwrap_or(0);
    let in_container = std::path::Path::new("/.dockerenv").exists();

    // Effective RAM: the lesser of physical RAM and cgroup memory cap (if set).
    let effective_ram = match cgroup_limit {
        Some(cap) if cap < ram_bytes => cap,
        _ => ram_bytes,
    };

    let profile = ResourceProfile::from_ram(effective_ram);
    let defaults = profile.defaults();

    info!(
        profile = profile.as_str(),
        ram_gb = ram_bytes / (1024 * 1024 * 1024),
        effective_ram_gb = effective_ram / (1024 * 1024 * 1024),
        cpu_cores,
        disk_free_gb = disk_free_bytes / (1024 * 1024 * 1024),
        in_container,
        cgroup_cap = cgroup_limit.map(|b| b / (1024 * 1024 * 1024)),
        memory_budget_mb = defaults.memory_budget_bytes / (1024 * 1024),
        "resource profile selected"
    );

    ProbeResult {
        profile,
        ram_bytes,
        cpu_cores,
        disk_free_bytes,
        cgroup_memory_limit_bytes: cgroup_limit,
        in_container,
        defaults,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read total available RAM from /proc/meminfo (Linux). Returns 0 on failure or non-Linux.
fn available_ram_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
            for line in s.lines() {
                // "MemTotal:       16384000 kB"
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    let kb: u64 = rest
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    return kb * 1024;
                }
            }
        }
    }
    0
}

/// Read cgroup v2 memory.max (Linux). Returns None if not cgroup-constrained or on error.
fn cgroup_memory_limit() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // cgroup v2 path
        let s = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
        let trimmed = s.trim();
        if trimmed == "max" {
            return None; // unconstrained
        }
        trimmed.parse::<u64>().ok()
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Free bytes available at the given path. Uses statvfs on Linux.
fn disk_free_at(path: &Path) -> Option<u64> {
    // Walk up to find an existing ancestor to stat.
    let mut probe = path.to_path_buf();
    for _ in 0..8 {
        if probe.exists() {
            break;
        }
        probe = probe.parent()?.to_path_buf();
    }
    if !probe.exists() {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let c_path = CString::new(probe.to_str()?).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret == 0 {
            return Some(stat.f_bsize as u64 * stat.f_bavail as u64);
        }
    }
    None
}
