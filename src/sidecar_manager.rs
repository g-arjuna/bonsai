//! Managed sidecar process lifecycle — spawn, stop, restart, status.
//!
//! `SidecarProcessManager` is an `Arc`-shareable handle that owns a single
//! supervised Python child process (the collector-engine sidecar). Bonsai
//! core can:
//!   - auto-start the sidecar at boot (`auto_start = true` in config)
//!   - restart it on crash with exponential back-off
//!   - start / stop / restart via HTTP endpoints
//!   - expose live process status (`pid`, `state`, `restart_count`) via the API
//!
//! The sidecar's gRPC heartbeat to the `SidecarRegistry` remains the health
//! signal; this module only handles OS-level process management.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};
use tracing::{error, info, warn};

use crate::config::ManagedSidecarConfig;

// ── Public state types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SidecarProcessState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SidecarProcessStatus {
    pub state: SidecarProcessState,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub last_exit_code: Option<i32>,
    pub uptime_secs: Option<u64>,
}

// ── Inner mutable state ───────────────────────────────────────────────────────

struct Inner {
    state: SidecarProcessState,
    child: Option<Child>,
    pid: Option<u32>,
    restart_count: u32,
    last_exit_code: Option<i32>,
    started_at: Option<Instant>,
}

impl Inner {
    fn new() -> Self {
        Self {
            state: SidecarProcessState::Stopped,
            child: None,
            pid: None,
            restart_count: 0,
            last_exit_code: None,
            started_at: None,
        }
    }
}

// ── SidecarProcessManager ─────────────────────────────────────────────────────

pub struct SidecarProcessManager {
    cfg: ManagedSidecarConfig,
    inner: Arc<Mutex<Inner>>,
    /// Sending `true` requests the supervisor loop to stop.
    shutdown_tx: watch::Sender<bool>,
}

impl SidecarProcessManager {
    pub fn new(cfg: ManagedSidecarConfig) -> Arc<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        Arc::new(Self {
            cfg,
            inner: Arc::new(Mutex::new(Inner::new())),
            shutdown_tx,
        })
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Spawn the sidecar process and attach a persistent supervisor loop.
    /// Safe to call from HTTP handlers or server_startup — supervisor uses
    /// the manager's own `shutdown_tx` so its lifetime matches the manager.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        let mut guard = self.inner.lock().await;
        if guard.state == SidecarProcessState::Running
            || guard.state == SidecarProcessState::Starting
        {
            return Err(anyhow::anyhow!("sidecar is already running (pid {:?})", guard.pid));
        }
        guard.state = SidecarProcessState::Starting;
        drop(guard);
        self.spawn_child().await?;

        // Subscribe to the persistent shutdown channel — the sender lives as
        // long as the Arc<SidecarProcessManager>, so the receiver will never
        // see a spurious close.
        let shutdown_rx = self.shutdown_tx.subscribe();
        Arc::clone(self).run_supervised(shutdown_rx);
        Ok(())
    }

    /// Send SIGTERM to the running child.
    /// If the supervisor has already taken the child handle out of the lock
    /// (which it does before awaiting `wait()`), fall back to signalling via PID.
    pub async fn stop(&self) -> Result<()> {
        let mut guard = self.inner.lock().await;
        match guard.state {
            SidecarProcessState::Running | SidecarProcessState::Starting => {
                guard.state = SidecarProcessState::Stopping;
                if let Some(child) = guard.child.as_mut() {
                    child.start_kill().context("SIGTERM to sidecar")?;
                    info!("sent SIGTERM to sidecar (pid {:?})", guard.pid);
                } else if let Some(pid) = guard.pid {
                    // Supervisor has taken the child handle — send SIGTERM via PID.
                    let status = Command::new("kill")
                        .arg("-15")
                        .arg(pid.to_string())
                        .status()
                        .await;
                    match status {
                        Ok(s) if s.success() => info!(pid, "sent SIGTERM to sidecar via PID"),
                        Ok(s) => warn!(pid, exit_status = ?s, "kill -15 returned non-zero"),
                        Err(e) => warn!(pid, error = %e, "failed to execute kill -15"),
                    }
                }
                Ok(())
            }
            _ => Err(anyhow::anyhow!("sidecar is not running")),
        }
    }

    /// Stop the running sidecar, wait for it to reach Stopped, then start again.
    pub async fn restart(self: &Arc<Self>) -> Result<()> {
        let _ = self.stop().await;
        // Poll up to 5 s for the supervisor to mark state as Stopped.
        for _ in 0..50 {
            {
                let guard = self.inner.lock().await;
                if guard.state == SidecarProcessState::Stopped {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.start().await
    }

    /// Current process status snapshot.
    pub async fn status(&self) -> SidecarProcessStatus {
        let guard = self.inner.lock().await;
        SidecarProcessStatus {
            state: guard.state.clone(),
            pid: guard.pid,
            restart_count: guard.restart_count,
            last_exit_code: guard.last_exit_code,
            uptime_secs: guard.started_at.map(|t| t.elapsed().as_secs()),
        }
    }

    // ── Supervisor background task ────────────────────────────────────────────

    /// Spawns a long-running supervisor Tokio task. Should be called once at
    /// startup when `auto_start = true`, or by the HTTP start handler.
    pub fn run_supervised(self: Arc<Self>, mut shutdown_rx: watch::Receiver<bool>) {
        let mgr = Arc::clone(&self);
        tokio::spawn(async move {
            let mut delay = mgr.cfg.restart_delay_secs;
            loop {
                // Take the child out of the lock so we can await wait() without
                // holding the mutex across the await boundary.
                let mut child = {
                    let mut guard = mgr.inner.lock().await;
                    if guard.state == SidecarProcessState::Stopped {
                        break;
                    }
                    guard.child.take()
                };

                let exit_code: i32 = if let Some(ref mut c) = child {
                    tokio::select! {
                        status = c.wait() => {
                            status.ok().and_then(|s| s.code()).unwrap_or(-1)
                        }
                        _ = shutdown_rx.changed() => {
                            // Shutdown requested — kill child and exit supervisor.
                            let _ = c.start_kill();
                            let _ = c.wait().await;
                            return;
                        }
                    }
                } else {
                    // No child — should not happen; exit loop.
                    break;
                };

                // Update state after child exit.
                let should_restart = {
                    let mut guard = mgr.inner.lock().await;
                    guard.child = None;
                    guard.pid = None;
                    guard.last_exit_code = Some(exit_code);

                    if guard.state == SidecarProcessState::Stopping {
                        guard.state = SidecarProcessState::Stopped;
                        info!("sidecar stopped (requested)");
                        false
                    } else {
                        guard.state = SidecarProcessState::Crashed;
                        guard.restart_count += 1;
                        warn!(
                            exit_code,
                            restart_count = guard.restart_count,
                            delay_secs = delay,
                            "sidecar exited unexpectedly — restarting"
                        );
                        true
                    }
                };

                if !should_restart {
                    break;
                }

                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(mgr.cfg.max_restart_delay_secs);

                if let Err(e) = mgr.spawn_child().await {
                    error!(error = %e, "failed to respawn sidecar");
                }
            }
        });
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn spawn_child(&self) -> Result<()> {
        let python = &self.cfg.python;
        let script = &self.cfg.script;

        let mut cmd = Command::new(python);
        cmd.arg(script);
        cmd.kill_on_drop(true);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if !self.cfg.working_dir.is_empty() {
            cmd.current_dir(PathBuf::from(&self.cfg.working_dir));
        }

        for (k, v) in &self.cfg.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().with_context(|| {
            format!("failed to spawn sidecar: {} {}", python, script)
        })?;

        let pid = child.id();
        info!(pid, python = %python, script = %script, "sidecar process spawned");

        // Pipe stdout/stderr into tracing at info/warn level.
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    info!(target: "sidecar", "{}", line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    warn!(target: "sidecar", "{}", line);
                }
            });
        }

        let mut guard = self.inner.lock().await;
        guard.pid = pid;
        guard.child = Some(child);
        guard.state = SidecarProcessState::Running;
        guard.started_at = Some(Instant::now());
        Ok(())
    }
}

/// Convenience type alias used throughout the HTTP layer.
pub type SharedSidecarManager = Arc<SidecarProcessManager>;
