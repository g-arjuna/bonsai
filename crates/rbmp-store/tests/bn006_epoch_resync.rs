//! Regression extracted from an actual EOS reconnect, not generated BMP fixtures.
use rbmp_rib::RibManager;
use rbmp_rib::event::RibEventPayload;
use rbmp_store::BmpJournal;
use rbmp_store::RibEventJournal;

#[test]
fn eos_reconnect_retires_unrefreshed_local_path_ids_at_eor() {
    let (mut rib, _) = RibManager::new(128);
    BmpJournal::replay_into(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/bn006_eos_4_36/reconnect.journal.jsonl"
        ),
        &mut rib,
    )
    .unwrap();
    assert_eq!(
        rib.total_routes(),
        4,
        "two real VRFs x post-policy/Loc-RIB: old epoch path IDs must not survive a complete new snapshot"
    );
    let speaker = "172.31.217.11".parse().unwrap();
    let mut actual = std::collections::BTreeSet::new();
    for (peer, table) in rib.ribs_for_speaker(speaker) {
        for view in [
            rbmp_core::bmp::types::RibType::LocRib,
            rbmp_core::bmp::types::RibType::AdjRibInPostPolicy,
        ] {
            for route in table.iter_rib(view) {
                actual.insert((
                    peer.distinguisher_string(),
                    format!("{view:?}"),
                    route.prefix.to_string(),
                    route.path_id,
                ));
            }
        }
    }
    let expected = [
        ("0:0", "LocRib", "10.77.1.0/24", Some(2)),
        ("0:0", "AdjRibInPostPolicy", "10.77.1.0/24", None),
        ("65001:100", "LocRib", "10.77.2.0/24", Some(1)),
        ("65001:100", "AdjRibInPostPolicy", "10.77.2.0/24", None),
    ]
    .into_iter()
    .map(|(a, b, c, d)| (a.to_string(), b.to_string(), c.to_string(), d))
    .collect();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn legacy_events_recover_retirement_metadata_without_resequencing_or_rewriting() {
    let (mut rib, _) = RibManager::new(128);
    let outcome = BmpJournal::replay_into_with_state(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/bn006_eos_4_36/reconnect.journal.jsonl"
        ),
        &mut rib,
    )
    .unwrap();
    let retirement = outcome.emitted_events.iter().find(|e| matches!(&e.payload, RibEventPayload::EndOfRib { retired_routes, .. } if retired_routes.iter().any(|r| r.route_key.path_id == Some(3)))).expect("EOR must identify old path 3");
    let mut legacy = Vec::new();
    for event in &outcome.emitted_events {
        let mut value = serde_json::to_value(event).unwrap();
        value["payload"]
            .as_object_mut()
            .unwrap()
            .remove("retired_routes");
        serde_json::to_writer(&mut legacy, &value).unwrap();
        legacy.push(b'\n');
    }
    let path = std::env::temp_dir().join(format!("bn006-events-{}.jsonl", uuid::Uuid::new_v4()));
    std::fs::write(&path, &legacy).unwrap();
    for _ in 0..2 {
        let journal = RibEventJournal::open(Some(&path)).await.unwrap();
        journal
            .enrich_replayed_eors(&outcome.emitted_events)
            .await
            .unwrap();
        assert!(journal.events_covered_by(&outcome.emitted_events).await);
        let mut missing_observation = outcome.emitted_events.clone();
        missing_observation.remove(0);
        assert!(!journal.events_covered_by(&missing_observation).await);
        let mut conflicting = outcome.emitted_events.clone();
        let conflict = conflicting
            .iter_mut()
            .find(|event| event.id == retirement.id)
            .unwrap();
        conflict.id = uuid::Uuid::new_v4();
        assert!(journal.enrich_replayed_eors(&conflicting).await.is_err());
        let restored = journal.all_events().await;
        assert_eq!(restored.len(), outcome.emitted_events.len());
        assert!(
            restored
                .iter()
                .zip(&outcome.emitted_events)
                .all(|(a, b)| a.id == b.id && a.event_key == b.event_key)
        );
        let restored_eor = restored.iter().find(|e| e.id == retirement.id).unwrap();
        assert!(
            matches!(&restored_eor.payload, RibEventPayload::EndOfRib { retired_routes, .. } if retired_routes.iter().any(|r| r.route_key.path_id == Some(3)))
        );
        let history = journal.prefix_history("10.77.1.0/24", 100).await;
        assert!(
            history
                .iter()
                .any(|item| item.action == "retire" && item.path_id == Some(3))
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            legacy,
            "upgrade must not rewrite durable history"
        );
    }
    std::fs::remove_file(path).unwrap();
}
