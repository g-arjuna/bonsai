//! BN-015-U1 browser acceptance: the NetBox onboarding flow must select
//! candidates by stable external identity (never by IP), import them as
//! inventory-only assets through the real authenticated router path, and
//! surface per-object results — including independent conflicts for two
//! NetBox devices that share one IP address.
//!
//! A mocked paginated NetBox instance serves two pages of devices; the
//! real Bonsai SPA+API router is exercised by Playwright/Chromium.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::Query;
use axum::routing::get;
use bonsai::config::TargetConfig;
use bonsai::graph::GraphStore;
use bonsai::http_server::{AppState, router_from_state};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn nb_device(id: u64, name: &str, ip: Option<&str>) -> Value {
    let primary_ip = ip.map(|a| json!({ "address": a }));
    json!({
        "id": id,
        "name": name,
        "primary_ip": primary_ip,
        "site": { "name": "Site A", "slug": "site-a" },
        "role": { "slug": "leaf", "name": "Leaf" },
        "device_type": { "model": "S9300", "manufacturer": { "name": "Nokia" } },
        "platform": { "slug": "srlinux", "name": "SR Linux" },
        "status": { "value": "active" },
        "tenant": { "name": "Tenant-1" },
    })
}

use axum::Json;

async fn mock_netbox() -> Result<(String, tokio::task::JoinHandle<()>)> {
    // Page 0 (offset=0): exactly 200 rows so the client fetches page 1.
    // Page 1 (offset=200): the interesting candidates (<200 → stop).
    let mut page0: Vec<Value> = (0..199)
        .map(|i| {
            nb_device(
                1000 + i as u64,
                &format!("filler-{i}"),
                Some(&format!("10.99.{}.{}", i / 250, (i % 250) + 1)),
            )
        })
        .collect();
    page0.push(nb_device(8, "edge-r1", Some("10.30.5.1/32")));

    let page1 = vec![
        nb_device(9, "noip-device", None),
        nb_device(10, "dup-a", Some("10.30.5.2/24")),
        nb_device(11, "dup-b", Some("10.30.5.2/24")),
        nb_device(12, "v6-edge", Some("2001:db8::10/64")),
    ];

    let page0 = Arc::new(page0);
    let page1 = Arc::new(page1);
    let total = page0.len() + page1.len();

    let version = get(|| async { Json(json!({ "netbox-version": "4.2.1" })) });
    let devices = {
        let page0 = Arc::clone(&page0);
        let page1 = Arc::clone(&page1);
        get(move |Query(params): Query<HashMap<String, String>>| {
            let page0 = Arc::clone(&page0);
            let page1 = Arc::clone(&page1);
            async move {
                let offset: usize = params
                    .get("offset")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let page = if offset == 0 { &page0 } else { &page1 };
                Json(json!({ "count": total, "results": &**page }))
            }
        })
    };

    let app = axum::Router::new()
        .route("/api/", version)
        .route("/api/dcim/devices/", devices);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((format!("http://{addr}"), handle))
}

#[tokio::test]
async fn netbox_onboarding_browser_acceptance() -> Result<()> {
    // Dev-mode auth bypass so the SPA/API are reachable without login —
    // the same canonical pattern as bn016_u1_topology_browser. RBAC on
    // the import route itself is covered by the authenticated-router
    // unit test in managed_devices.
    // SAFETY: test-only process env, set before any server task starts.
    unsafe {
        std::env::set_var("BONSAI_ALLOW_INSECURE_NO_AUTH", "1");
        std::env::set_var("BONSAI_VAULT_PASSPHRASE", "bn015-u1-inventory-browser");
    }

    let (netbox_url, _nb) = mock_netbox().await?;

    let tmp = TempDir::new().context("temp runtime dir")?;
    let graph_path = tmp.path().join("graph").to_string_lossy().into_owned();
    let store = Arc::new(GraphStore::open(&graph_path, 256 * 1024 * 1024)?);
    let state = AppState::new_minimal_with_runtime_dir(
        Arc::clone(&store),
        tmp.path().to_string_lossy().as_ref(),
    );
    // One ordinary configured device keeps is_first_run=false so the SPA
    // renders the device workspaces rather than the first-run wizard.
    state
        .registry
        .add_device(TargetConfig {
            address: "10.0.0.1".to_string(),
            enabled: true,
            hostname: Some("seed".to_string()),
            vendor: Some("nokia_srl".to_string()),
            ..Default::default()
        })
        .context("seed device")?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let bonsai_url = format!("http://127.0.0.1:{port}");
    let router = router_from_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let client = reqwest::Client::new();
    for _ in 0..50 {
        if client
            .get(format!("{bonsai_url}/api/onboarding/devices"))
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
    println!("Mock NetBox: {netbox_url}");
    let mut pw = tokio::process::Command::new("npx");
    pw.args(&[
        "--prefix",
        "tests/ui_driver",
        "playwright",
        "test",
        "--config",
        "tests/ui_driver/playwright.config.js",
        "tests/ui_driver/netbox_onboarding.spec.js",
        "--reporter=list",
    ])
    .current_dir(".")
    .env("BONSAI_URL", &bonsai_url)
    .env("NETBOX_MOCK_URL", &netbox_url);
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
    assert!(status.success(), "playwright netbox spec failed: {status}");
    Ok(())
}
