//! BN-016-U1 focused browser acceptance: the topology view must render
//! ONLY BGP sessions the backend deterministically resolved. Seeded via
//! the production `GraphStore::write_batch` path, served through the
//! canonical `router_from_state` SPA+API router, exercised by Playwright.

use std::sync::Arc;

use anyhow::{Context, Result};
use bonsai::config::TargetConfig;
use bonsai::graph::GraphStore;
use bonsai::http_server::{AppState, router_from_state};
use bonsai::telemetry::TelemetryUpdate;
use tempfile::TempDir;
use tokio::net::TcpListener;

fn gnmi_bgp(target: &str, peer: &str, ni: &str, state: &str, peer_as: i64) -> TelemetryUpdate {
    TelemetryUpdate {
        target: target.to_string(),
        vendor: "nokia_srl".to_string(),
        hostname: target.to_string(),
        role: "leaf".to_string(),
        site: "dc-a".to_string(),
        timestamp_ns: 1_000,
        path: format!(
            "network-instance[name={ni}]/protocols/bgp/neighbor[peer-address={peer}]"
        ),
        value: serde_json::json!({ "session-state": state, "peer-as": peer_as }),
    }
}

fn iface_update(target: &str) -> TelemetryUpdate {
    TelemetryUpdate {
        target: target.to_string(),
        vendor: "nokia_srl".to_string(),
        hostname: target.to_string(),
        role: "leaf".to_string(),
        site: "dc-a".to_string(),
        timestamp_ns: 1_000,
        path: "interfaces/interface[name=eth0]/state/counters".to_string(),
        value: serde_json::json!({ "in-octets": 1, "out-octets": 1 }),
    }
}

fn registry_target(address: &str, hostname: &str) -> TargetConfig {
    TargetConfig {
        address: address.to_string(),
        enabled: true,
        hostname: Some(hostname.to_string()),
        vendor: Some("nokia_srl".to_string()),
        role: Some("leaf".to_string()),
        site: Some("dc-a".to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn topology_browser_renders_only_backend_resolved_sessions() -> Result<()> {
    // Dev-mode auth bypass so the SPA/API are reachable without login.
    // SAFETY: test-only process env, set before any server task starts.
    unsafe {
        std::env::set_var("BONSAI_ALLOW_INSECURE_NO_AUTH", "1");
        std::env::set_var("BONSAI_VAULT_PASSPHRASE", "bn016-u1-browser-acceptance");
    }

    let tmp = TempDir::new().context("temp runtime dir")?;
    let db_path = tmp.path().join("bonsai.db").to_string_lossy().into_owned();
    let store = Arc::new(GraphStore::open(&db_path, 256 * 1024 * 1024)?);

    // Seed through the PRODUCTION batch path: one router reporting four
    // sessions — two network-instances to the same peer (must stay two
    // sessions), one distinct peer, one unresolvable peer — plus the two
    // peer devices reporting their own view back.
    store
        .write_batch(vec![
            gnmi_bgp("10.0.0.1", "10.0.0.2", "default", "established", 65002),
            gnmi_bgp("10.0.0.1", "10.0.0.2", "vrf-blue", "established", 65002),
            gnmi_bgp("10.0.0.1", "10.0.0.3", "default", "down", 65003),
            gnmi_bgp("10.0.0.1", "10.9.9.9", "default", "established", 65009),
            gnmi_bgp("10.0.0.2", "10.0.0.1", "default", "established", 65001),
            gnmi_bgp("10.0.0.3", "10.0.0.1", "default", "down", 65001),
            iface_update("10.0.0.2"),
            iface_update("10.0.0.3"),
        ])
        .await
        .context("seed graph via write_batch")?;

    let state = AppState::new_minimal_with_runtime_dir(
        Arc::clone(&store),
        tmp.path().to_string_lossy().as_ref(),
    );
    for (addr, host) in [
        ("10.0.0.1", "r1"),
        ("10.0.0.2", "peer-a"),
        ("10.0.0.3", "peer-b"),
    ] {
        state
            .registry
            .add_device(registry_target(addr, host))
            .with_context(|| format!("register {addr}"))?;
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let bonsai_url = format!("http://127.0.0.1:{port}");
    let router = router_from_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    // Wait for the API to answer before handing the URL to the browser.
    let client = reqwest::Client::new();
    for _ in 0..50 {
        if client
            .get(format!("{bonsai_url}/api/topology"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    println!("Bonsai URL: {bonsai_url}");
    let mut pw = tokio::process::Command::new("npx");
    pw.args(&[
        "--prefix",
        "tests/ui_driver",
        "playwright",
        "test",
        "--config",
        "tests/ui_driver/playwright.config.js",
        "tests/ui_driver/topology_bgp.spec.js",
        "--reporter=list",
    ])
    .current_dir(".")
    .env("BONSAI_URL", &bonsai_url);
    if std::env::var("PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH").is_err() {
        for candidate in [
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
        ] {
            if std::path::Path::new(candidate).exists() {
                pw.env("PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH", candidate);
                break;
            }
        }
    }
    let status = pw.status().await.context("run playwright")?;
    assert!(status.success(), "playwright topology spec failed: {status}");
    Ok(())
}
