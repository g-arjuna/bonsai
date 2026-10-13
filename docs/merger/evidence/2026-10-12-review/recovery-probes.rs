use bonsai::routing_service::{RoutingServiceState, RoutingServiceConfig, sample_bmp_session_frames};
use rbmp_core::bmp::types::{BmpMessage, BmpPayload};
use rbmp_core::bmp::parser::{parse_bmp_message, DEFAULT_MAX_FRAME};
use rbmp_core::bgp::types::Prefix;
use std::net::{IpAddr, Ipv4Addr};
use std::io::Write;
fn config(dir: &std::path::Path) -> RoutingServiceConfig {
    RoutingServiceConfig { bmp_listen_addr: "127.0.0.1:0".into(), http_listen_addr: "127.0.0.1:0".into(), bmp_journal_path: Some(dir.join("bmp.jsonl")), event_journal_path: Some(dir.join("events.jsonl")), max_frame_bytes: DEFAULT_MAX_FRAME, internal_service_token: None }
}
fn frames() -> (Vec<u8>, Vec<u8>, Vec<u8>) { sample_bmp_session_frames(Ipv4Addr::new(10,0,0,2),65001,[10,10,1],100) }
fn speaker() -> IpAddr { "192.0.2.1".parse().unwrap() }
#[tokio::test]
async fn accepted_raw_record_must_recover_event_history() {
 let dir=tempfile::tempdir().unwrap(); let cfg=config(dir.path()); let state=RoutingServiceState::new(cfg.clone()).await.unwrap(); let (init,_,_)=frames();
 let msg=BmpMessage { id: uuid::Uuid::new_v4(), received_at: chrono::Utc::now(), speaker_addr: speaker(), payload: parse_bmp_message(&init,speaker(),DEFAULT_MAX_FRAME).unwrap() };
 state.bmp_journal.append(&msg).await.unwrap(); drop(state);
 let recovered=RoutingServiceState::new(cfg).await.unwrap();
 assert_eq!(recovered.rib.read().await.speakers().len(),1);
 assert!(recovered.event_journal.total_events().await > 0,"Accepted raw observation rebuilt current state but lost event history");
}
#[tokio::test]
async fn recovered_truncated_event_tail_must_allow_future_restart() {
 let dir=tempfile::tempdir().unwrap(); let cfg=config(dir.path()); let (init,_,_)=frames();
 let state=RoutingServiceState::new(cfg.clone()).await.unwrap(); state.ingest_bmp_bytes(speaker(),Some(1),&init).await.unwrap(); drop(state);
 let mut f=std::fs::OpenOptions::new().append(true).open(dir.path().join("events.jsonl")).unwrap(); f.write_all(b"{\"torn\":").unwrap(); drop(f);
 let resumed=RoutingServiceState::new(cfg.clone()).await.unwrap(); resumed.ingest_bmp_bytes(speaker(),Some(2),&init).await.unwrap(); drop(resumed);
 assert!(RoutingServiceState::new(cfg).await.is_ok(),"Skipped torn tail was left on disk and merged with the next record");
}
#[tokio::test]
async fn transport_gap_must_survive_restart() {
 let dir=tempfile::tempdir().unwrap(); let cfg=config(dir.path()); let (init,peer,route)=frames(); let state=RoutingServiceState::new(cfg.clone()).await.unwrap();
 for frame in [&init,&peer,&route] { state.ingest_bmp_bytes(speaker(),Some(1),frame).await.unwrap(); }
 state.record_transport_disconnect(speaker(),"review disconnect").await; drop(state);
 let recovered=RoutingServiceState::new(cfg).await.unwrap();
 assert!(recovered.transport_gaps.read().await.contains_key(&speaker()),"Monitoring gap disappeared across restart");
}
#[tokio::test]
async fn large_valid_update_must_preserve_all_route_events() {
 let dir=tempfile::tempdir().unwrap(); let state=RoutingServiceState::new(config(dir.path())).await.unwrap(); let (init,peer,route)=frames();
 for frame in [&init,&peer] { state.ingest_bmp_bytes(speaker(),Some(1),frame).await.unwrap(); }
 let mut payload=parse_bmp_message(&route,speaker(),DEFAULT_MAX_FRAME).unwrap();
 if let BmpPayload::RouteMonitoring { update, .. } = &mut payload {
  update.announced=(0..5000u32).map(|i| Prefix::V4(ipnet::Ipv4Net::new(Ipv4Addr::new(10,(i>>8) as u8,i as u8,0),24).unwrap())).collect(); update.announced_path_ids=vec![None;5000];
 } else { panic!("Expected update"); }
 let before=state.event_journal.total_events().await;
 state.ingest_bmp_message(BmpMessage {id:uuid::Uuid::new_v4(), received_at:chrono::Utc::now(),speaker_addr:speaker(),payload},Some(1)).await.unwrap();
 let (_,routes)=bonsai::routing_service::build_routes_json(&state,&Default::default()).await;
 assert_eq!(routes["total_matching"],5000);
 assert_eq!(state.event_journal.total_events().await-before,5000,"Broadcast lag silently dropped emitted history events");
}
