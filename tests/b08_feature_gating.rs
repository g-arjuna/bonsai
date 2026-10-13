use bonsai::ha_coordinator::{EtcdConfig, HACoordinator, HAMode, LeaderState};
use bonsai::remediation::rollback::{RollbackRegistry, RollbackState};

#[tokio::test]
async fn b08_cluster_mode_without_etcd_does_not_fallback_to_leader() {
    let ha = HACoordinator::new(HAMode::Cluster {
        node_id: "node-1".to_string(),
    });
    ha.start_election().await;
    assert!(
        !ha.is_leader().await,
        "cluster mode without etcd config must not silently assume leadership"
    );
    assert!(!ha.is_etcd_connected());
    assert_eq!(ha.state().await, LeaderState::Electing);
}

#[tokio::test]
async fn b08_cluster_mode_with_unreachable_etcd_does_not_fallback_to_leader() {
    let ha = HACoordinator::new(HAMode::Cluster {
        node_id: "node-1".to_string(),
    })
    .with_etcd_config(EtcdConfig {
        endpoints: vec!["http://127.0.0.1:1".to_string()],
        election_ttl_secs: 5,
        config_prefix: "/bonsai/config".to_string(),
    });
    ha.start_election().await;
    assert!(
        !ha.is_leader().await,
        "cluster mode with unreachable etcd must not silently assume leadership"
    );
    assert!(!ha.is_etcd_connected());
    assert_eq!(ha.state().await, LeaderState::Electing);
}

#[test]
fn b08_rollback_registry_persists_active_windows_across_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut reg = RollbackRegistry::load(dir.path());
        reg.register(RollbackState {
            proposal_id: "prop-42".to_string(),
            remediation_id: "rem-42".to_string(),
            executed_at_ns: 100 * 1_000_000_000,
            window_secs: 60,
            snapshot_json:
                r#"[{"yang_path":"/interfaces","before_value":"up","after_value":"down"}]"#
                    .to_string(),
            rolled_back: false,
        });
    }

    let mut reloaded = RollbackRegistry::load(dir.path());
    let active = reloaded.active_windows(120 * 1_000_000_000);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].proposal_id, "prop-42");

    reloaded.mark_rolled_back("prop-42");
    let reloaded_again = RollbackRegistry::load(dir.path());
    assert!(
        reloaded_again
            .active_windows(120 * 1_000_000_000)
            .is_empty()
    );
}
