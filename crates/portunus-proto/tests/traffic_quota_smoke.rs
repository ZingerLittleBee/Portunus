//! 013-traffic-quotas: smoke test that the new v1.4 proto types compile,
//! round-trip through prost encode/decode, and are reachable via the
//! `ServerMessage.payload` oneof variant.

use portunus_proto::v1::{
    server_message, ServerMessage, TrafficQuotaAction, TrafficQuotaState, TrafficQuotaUpdate,
};
use prost::Message;

#[test]
fn traffic_quota_update_set_roundtrips() {
    let state = TrafficQuotaState {
        monthly_bytes: 1_000_000_000,
        budget_remaining_bytes: 999_000_000,
        period_started_at_unix_sec: 1_704_067_200,
        period_ends_at_unix_sec: 1_706_745_600,
        exhausted: false,
    };
    let update = TrafficQuotaUpdate {
        request_id: "01HXXX".into(),
        user_id: "alice".into(),
        client_name: "edge-01".into(),
        action: TrafficQuotaAction::Set as i32,
        state: Some(state.clone()),
    };
    let msg = ServerMessage {
        payload: Some(server_message::Payload::TrafficQuotaUpdate(update.clone())),
    };
    let encoded = msg.encode_to_vec();
    let decoded = ServerMessage::decode(&encoded[..]).unwrap();
    let Some(server_message::Payload::TrafficQuotaUpdate(got)) = decoded.payload else {
        panic!("wrong variant after decode");
    };
    assert_eq!(got.user_id, "alice");
    assert_eq!(got.client_name, "edge-01");
    assert_eq!(got.action, TrafficQuotaAction::Set as i32);
    assert_eq!(got.state.unwrap(), state);
}

#[test]
fn traffic_quota_update_remove_has_no_state() {
    let update = TrafficQuotaUpdate {
        request_id: "01HXXY".into(),
        user_id: "alice".into(),
        client_name: "edge-01".into(),
        action: TrafficQuotaAction::Remove as i32,
        state: None,
    };
    let encoded = update.encode_to_vec();
    let got = TrafficQuotaUpdate::decode(&encoded[..]).unwrap();
    assert_eq!(got.action, TrafficQuotaAction::Remove as i32);
    assert!(got.state.is_none());
}

#[test]
fn budget_remaining_can_be_negative() {
    // Sanity: int64 wire type round-trips negative values for exhausted state.
    let state = TrafficQuotaState {
        monthly_bytes: 100,
        budget_remaining_bytes: -42,
        period_started_at_unix_sec: 1_704_067_200,
        period_ends_at_unix_sec: 1_706_745_600,
        exhausted: true,
    };
    let encoded = state.encode_to_vec();
    let got = TrafficQuotaState::decode(&encoded[..]).unwrap();
    assert_eq!(got.budget_remaining_bytes, -42);
    assert!(got.exhausted);
}
