/// RV7-F T2/T3: Roto JIT filter engine for rbmp-rib.
///
/// When compiled with `--features roto-jit` the engine JIT-compiles `.roto`
/// scripts via Cranelift and evaluates them at ~native speed per route.
///
/// When the feature is absent (default) the module still compiles and exports
/// the same public API — `RotoFilterEngine::load()` and `evaluate()` — but
/// each call falls through to `FilterVerdict::Default` (accept-all), preserving
/// the YAML DSL as the only active engine.
///
/// Hot-reload: the filter_watcher detects changes to `.roto` files and calls
/// `reload()` without restarting the server.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::warn;
use crate::filter::FilterVerdict;
use crate::roto_ctx::RouteCtx;

// ─── Filter stats ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct RotoFilterStats {
    pub accept_count:  AtomicU64,
    pub deny_count:    AtomicU64,
    pub error_count:   AtomicU64,
    pub total_ns:      AtomicU64,
}

impl RotoFilterStats {
    pub fn record(&self, verdict: FilterVerdict, elapsed: std::time::Duration) {
        match verdict {
            FilterVerdict::Accept  => { self.accept_count.fetch_add(1, Ordering::Relaxed); }
            FilterVerdict::Deny    => { self.deny_count.fetch_add(1, Ordering::Relaxed); }
            FilterVerdict::Default => {}
        }
        self.total_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> serde_json::Value {
        let accept = self.accept_count.load(Ordering::Relaxed);
        let deny   = self.deny_count.load(Ordering::Relaxed);
        let errors = self.error_count.load(Ordering::Relaxed);
        let total  = accept + deny;
        let avg_ns = if total > 0 {
            self.total_ns.load(Ordering::Relaxed) / total
        } else { 0 };
        serde_json::json!({
            "engine":       "roto-jit",
            "accept_count": accept,
            "deny_count":   deny,
            "error_count":  errors,
            "avg_eval_ns":  avg_ns,
        })
    }
}

// ─── Feature-gated engine ─────────────────────────────────────────────────────

#[cfg(feature = "roto-jit")]
mod jit {
    use super::*;
    use roto::{Runtime, Compiler, Script};

    /// Build a Roto Runtime with all RouteCtx fields registered.
    fn build_runtime() -> Runtime {
        let mut rt = Runtime::new();

        // Register every field from RouteCtx so Roto scripts can access them
        // as `route.prefix`, `route.rpki`, etc.
        rt.register_type::<RouteCtx>("RouteCtx")
          .field("prefix",        |r: &RouteCtx| r.prefix.clone())
          .field("prefix_len",    |r: &RouteCtx| r.prefix_len as u32)
          .field("afi",           |r: &RouteCtx| r.afi.clone())
          .field("peer_as",       |r: &RouteCtx| r.peer_as)
          .field("peer_addr",     |r: &RouteCtx| r.peer_addr.clone())
          .field("rib_type",      |r: &RouteCtx| r.rib_type.clone())
          .field("action",        |r: &RouteCtx| r.action.clone())
          .field("as_path",       |r: &RouteCtx| r.as_path.clone())
          .field("as_path_len",   |r: &RouteCtx| r.as_path_len)
          .field("origin_asn",    |r: &RouteCtx| r.origin_asn)
          .field("has_prepend",   |r: &RouteCtx| r.has_prepend)
          .field("next_hop",      |r: &RouteCtx| r.next_hop.clone())
          .field("local_pref",    |r: &RouteCtx| r.local_pref)
          .field("med",           |r: &RouteCtx| r.med)
          .field("origin_attr",   |r: &RouteCtx| r.origin_attr.clone())
          .field("rpki",          |r: &RouteCtx| r.rpki.clone())
          .field("aspa",          |r: &RouteCtx| r.aspa.clone())
          .field("otc_asn",       |r: &RouteCtx| r.otc_asn)
          .field("is_evpn",       |r: &RouteCtx| r.is_evpn)
          .field("is_bgpls",      |r: &RouteCtx| r.is_bgpls)
          .field("is_srpolicy",   |r: &RouteCtx| r.is_srpolicy)
          .field("is_unnumbered", |r: &RouteCtx| r.is_unnumbered)
          .field("evpn_type",     |r: &RouteCtx| r.evpn_type as u32);

        rt.register_function("community_has",
            |ctx: &RouteCtx, c: String| -> bool {
                ctx.community_has(&c)
            },
        );
        rt.register_function("as_path_contains",
            |ctx: &RouteCtx, asn: u32| -> bool {
                ctx.as_path_contains(asn)
            },
        );
        rt.register_function("prefix_in_range",
            |ctx: &RouteCtx, cidr: String| -> bool {
                ctx.prefix_in_range(&cidr)
            },
        );

        rt
    }

    pub struct RotoFilterEngineInner {
        runtime:  Runtime,
        script:   Option<Script>,
        path:     String,
        pub stats: Arc<RotoFilterStats>,
    }

    impl RotoFilterEngineInner {
        pub fn load(path: &str) -> anyhow::Result<Self> {
            let rt     = build_runtime();
            let source = std::fs::read_to_string(path)?;
            let script = Compiler::new(&rt)
                .compile(&source)
                .map_err(|e| anyhow::anyhow!("Roto compile error in {path}: {e}"))?;
            info!(path, "Roto filter JIT-compiled via Cranelift");
            Ok(Self {
                runtime: rt,
                script:  Some(script),
                path:    path.to_string(),
                stats:   Arc::new(RotoFilterStats::default()),
            })
        }

        pub fn reload(&mut self) -> bool {
            let source = match std::fs::read_to_string(&self.path) {
                Ok(s)  => s,
                Err(e) => { warn!(path = %self.path, %e, "Roto reload: read error"); return false; }
            };
            match Compiler::new(&self.runtime).compile(&source) {
                Ok(new_script) => {
                    self.script = Some(new_script);
                    info!(path = %self.path, "Roto filter hot-reloaded (JIT)");
                    true
                }
                Err(e) => {
                    warn!(path = %self.path, %e, "Roto reload failed — retaining current filter");
                    false
                }
            }
        }

        pub fn evaluate(&self, ctx: &RouteCtx) -> FilterVerdict {
            let script = match &self.script {
                Some(s) => s,
                None    => return FilterVerdict::Default,
            };
            let t0 = std::time::Instant::now();
            let verdict = match script.call::<RouteCtx, bool>("bgp_filter", ctx) {
                Ok(true)  => FilterVerdict::Accept,
                Ok(false) => FilterVerdict::Deny,
                Err(e)    => {
                    self.stats.error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    warn!(%e, "Roto eval error — default-accept");
                    FilterVerdict::Default
                }
            };
            self.stats.record(verdict, t0.elapsed());
            verdict
        }
    }
}

// ─── Public type (always available) ──────────────────────────────────────────

/// Public Roto filter engine.
///
/// When `roto-jit` feature is enabled: JIT-compiles a `.roto` script.
/// When feature absent: no-op engine that always returns `Default` (accept).
pub struct RotoFilterEngine {
    #[cfg(feature = "roto-jit")]
    inner: jit::RotoFilterEngineInner,
    #[cfg(not(feature = "roto-jit"))]
    #[allow(dead_code)]
    path:  String,
    pub stats: Arc<RotoFilterStats>,
}

impl RotoFilterEngine {
    /// Load and JIT-compile a `.roto` file.
    ///
    /// Returns `Ok(engine)` on success.  When the `roto-jit` feature is
    /// absent returns an engine that accepts all routes (no-op).
    pub fn load(path: &str) -> anyhow::Result<Self> {
        #[cfg(feature = "roto-jit")]
        {
            let inner = jit::RotoFilterEngineInner::load(path)?;
            let stats = Arc::clone(&inner.stats);
            Ok(Self { inner, stats })
        }
        #[cfg(not(feature = "roto-jit"))]
        {
            warn!(path, "roto-jit feature not enabled — Roto filter is a no-op (default-accept)");
            let stats = Arc::new(RotoFilterStats::default());
            Ok(Self { path: path.to_string(), stats })
        }
    }

    /// Hot-reload: recompile from the original file path.
    /// Returns true if reload succeeded, false if it failed (previous script kept).
    pub fn reload(&mut self) -> bool {
        #[cfg(feature = "roto-jit")]
        { self.inner.reload() }
        #[cfg(not(feature = "roto-jit"))]
        { false }
    }

    /// Evaluate the filter against a route context.
    pub fn evaluate(&self, _ctx: &RouteCtx) -> FilterVerdict {
        #[cfg(feature = "roto-jit")]
        { self.inner.evaluate(_ctx) }
        #[cfg(not(feature = "roto-jit"))]
        { FilterVerdict::Default }
    }

    /// Stats snapshot for the `/api/filters/stats` endpoint.
    pub fn stats_snapshot(&self) -> serde_json::Value {
        self.stats.snapshot()
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_default() {
        let stats = RotoFilterStats::default();
        stats.record(FilterVerdict::Accept, std::time::Duration::from_nanos(100));
        stats.record(FilterVerdict::Deny,   std::time::Duration::from_nanos(200));
        let snap = stats.snapshot();
        assert_eq!(snap["accept_count"], 1);
        assert_eq!(snap["deny_count"],   1);
    }

    // Compile-time smoke test: engine loads and default-accepts without roto-jit feature
    #[cfg(not(feature = "roto-jit"))]
    #[test]
    fn test_noop_engine_accepts() {
        let engine = RotoFilterEngine::load("config/filters.roto")
            .expect("noop load should succeed regardless of file existence");
        let ctx = RouteCtx::default();
        assert_eq!(engine.evaluate(&ctx), FilterVerdict::Default);
    }
}
