use super::*;
use quincy_server::server::lifecycle::Observer;

fn session(ip: &str) -> QuincySession {
    QuincySession { username: "alice".into(), address: ip.parse().unwrap() }
}

#[test]
fn sessions_are_distinct_and_release_removes_only_its_connection() {
    let state = Sessions::new(7, 2, 64);
    state.connected(session("192.0.2.2/24"));
    state.connected(session("192.0.2.3/24"));
    let before = state.snapshot();
    assert_eq!(before.len(), 2);
    assert_ne!(before[0].id, before[1].id);
    state.disconnected(session("192.0.2.2/24"));
    assert_eq!(
        state.snapshot()[0].address,
        "192.0.2.3".parse::<IpAddr>().unwrap()
    );
}

#[test]
fn capacity_degrades_only_capability_and_recovers_after_sessions_release() {
    let state = Sessions::new(7, 1, 64);
    state.connected(session("192.0.2.2/24"));
    state.connected(session("192.0.2.3/24"));
    assert!(state.snapshot().is_empty());
    state.disconnected(session("192.0.2.2/24"));
    state.disconnected(session("192.0.2.3/24"));
    state.connected(session("192.0.2.4/24"));
    assert_eq!(state.snapshot().len(), 1);
}
