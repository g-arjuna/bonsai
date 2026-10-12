/// HTTP API smoke tests against an in-process server (RV4-9 T2).
///
/// Spins up a full AppState with an in-memory DuckDB store, binds
/// on a random port, and verifies each endpoint responds correctly.
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use rbmp_store::{RouteStore, writer::StoreWriter};
    use rbmp_store::query::QueryEngine;
    use rbmp_rib::RibManager;
    use rbmp_enrichment::EnrichmentEngine;
    use rbmp_server::api::{AppState, build_router};
    use rbmp_server::config::{AuthConfig, SpeakerRegistry};
    use metrics_exporter_prometheus::PrometheusBuilder;
    use std::net::SocketAddr;

    async fn spawn_test_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let store = Arc::new(std::sync::Mutex::new(
            RouteStore::in_memory().expect("in-memory store")
        ));
        let queries  = Arc::new(QueryEngine::new(Arc::clone(&store)));
        let rib      = Arc::new(tokio::sync::RwLock::new(RibManager::new(16384)));
        let (tx, _)  = broadcast::channel(1024);
        let enrichment = Arc::new(EnrichmentEngine::default());
        let registry = Arc::new(SpeakerRegistry::default());
        let auth_cfg = Arc::new(AuthConfig::default());
        let prom     = PrometheusBuilder::new().build_recorder().handle();

        let state = AppState { rib, store, queries, events: tx, enrichment, registry, prom, auth_cfg };
        let router = build_router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await.expect("bind test server");
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (addr, handle) = spawn_test_server().await;
        let url = format!("http://{addr}/health");
        let res = reqwest::get(&url).await.expect("GET /health");
        assert_eq!(res.status(), 200);
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["status"], "ok");
        handle.abort();
    }

    #[tokio::test]
    async fn peers_returns_empty_list() {
        let (addr, handle) = spawn_test_server().await;
        let url = format!("http://{addr}/api/peers");
        let res = reqwest::get(&url).await.expect("GET /api/peers");
        assert_eq!(res.status(), 200);
        let body: serde_json::Value = res.json().await.unwrap();
        assert!(body.is_array(), "peers must be an array");
        handle.abort();
    }

    #[tokio::test]
    async fn routes_returns_empty_list() {
        let (addr, handle) = spawn_test_server().await;
        let url = format!("http://{addr}/api/routes");
        let res = reqwest::get(&url).await.expect("GET /api/routes");
        assert_eq!(res.status(), 200);
        handle.abort();
    }

    #[tokio::test]
    async fn bgpls_graph_returns_empty_graph() {
        let (addr, handle) = spawn_test_server().await;
        let url = format!("http://{addr}/api/bgpls/graph");
        let res = reqwest::get(&url).await.expect("GET /api/bgpls/graph");
        assert_eq!(res.status(), 200);
        let body: serde_json::Value = res.json().await.unwrap();
        assert!(body["nodes"].is_array());
        assert!(body["links"].is_array());
        handle.abort();
    }

    #[tokio::test]
    async fn auth_rejects_invalid_key() {
        use rbmp_server::config::AuthConfig;
        // Spawn with auth enabled + specific key
        let store = Arc::new(std::sync::Mutex::new(RouteStore::in_memory().unwrap()));
        let queries  = Arc::new(QueryEngine::new(Arc::clone(&store)));
        let rib      = Arc::new(tokio::sync::RwLock::new(RibManager::new(16384)));
        let (tx, _)  = broadcast::channel(1024);
        let enrichment = Arc::new(EnrichmentEngine::default());
        let registry = Arc::new(SpeakerRegistry::default());
        let auth_cfg = Arc::new(AuthConfig {
            enabled:                 true,
            jwt_secret:              "test-secret-32-bytes-minimum!!!!".into(),
            token_ttl_secs:          3600,
            api_keys:                vec!["valid-key".into()],
            rate_limit_msgs_per_sec: 0,
        });
        let prom = PrometheusBuilder::new().build_recorder().handle();
        let state  = AppState { rib, store, queries, events: tx, enrichment, registry, prom, auth_cfg };
        let router = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });

        // No token → 401
        let res = reqwest::get(format!("http://{addr}/api/peers")).await.unwrap();
        assert_eq!(res.status(), 401, "/api/peers without token should be 401");

        // Wrong api_key → 401 from /auth
        let client = reqwest::Client::new();
        let res = client.post(format!("http://{addr}/auth"))
            .json(&serde_json::json!({ "api_key": "wrong-key" }))
            .send().await.unwrap();
        assert_eq!(res.status(), 401, "wrong api_key should return 401");

        handle.abort();
    }
}
