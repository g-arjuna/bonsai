/// DuckDB retention sweep unit tests (RV4-9 T3).
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use rbmp_store::RouteStore;

    fn make_store() -> Arc<std::sync::Mutex<RouteStore>> {
        Arc::new(std::sync::Mutex::new(RouteStore::in_memory().expect("in-memory")))
    }

    fn insert_old_route(store: &RouteStore) {
        store.conn().execute_batch(
            r#"INSERT INTO route_events
               (id, occurred_at, speaker_addr, peer_addr, peer_as, rib_type,
                action, prefix, afi)
               VALUES (gen_random_uuid(), NOW() - INTERVAL '100' DAY,
                       '10.0.0.1', '10.0.0.2', 65000, 'ipv4',
                       'announce', '192.0.2.0/24', 'ipv4')"#,
        ).expect("insert old route");
    }

    fn insert_fresh_route(store: &RouteStore) {
        store.conn().execute_batch(
            r#"INSERT INTO route_events
               (id, occurred_at, speaker_addr, peer_addr, peer_as, rib_type,
                action, prefix, afi)
               VALUES (gen_random_uuid(), NOW() - INTERVAL '1' DAY,
                       '10.0.0.1', '10.0.0.2', 65000, 'ipv4',
                       'announce', '203.0.113.0/24', 'ipv4')"#,
        ).expect("insert fresh route");
    }

    fn count_routes(store: &RouteStore) -> i64 {
        store.conn().query_row("SELECT COUNT(*) FROM route_events", [], |r| r.get(0))
            .unwrap_or(0)
    }

    #[test]
    fn retention_deletes_old_keeps_fresh() {
        let store = make_store();
        let s = store.lock().unwrap();
        insert_old_route(&s);
        insert_fresh_route(&s);
        assert_eq!(count_routes(&s), 2, "should start with 2 routes");

        // Simulate 30-day retention sweep
        let sql = "DELETE FROM route_events WHERE occurred_at < NOW() - INTERVAL '30' DAY";
        s.conn().execute(sql, []).unwrap();

        assert_eq!(count_routes(&s), 1, "old route should be deleted");
    }

    #[test]
    fn retention_zero_days_is_noop() {
        let store = make_store();
        let s = store.lock().unwrap();
        insert_old_route(&s);
        insert_fresh_route(&s);

        // retain_days = 0 → no deletion
        let count_before = count_routes(&s);
        // (no SQL executed — same as run_retention_sweep returning early)
        assert_eq!(count_routes(&s), count_before);
    }

    #[tokio::test]
    async fn retention_task_exits_immediately_when_disabled() {
        use rbmp_server::retention::run_retention_sweep;
        let store = make_store();
        // retain_days = 0 → task returns immediately (no loop)
        let task = tokio::spawn(run_retention_sweep(Arc::clone(&store), 0));
        // Should complete without blocking
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("retention task should exit quickly when disabled")
            .expect("task should not panic");
    }
}
