use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Mutex;

use quincy_server::server::lifecycle::{Observer, Session as QuincySession};
use scutum_management::Identity;
use tokio::sync::Notify;

#[derive(Clone, Debug)]
pub struct Session {
    pub id: Identity,
    pub sequence: u64,
    pub principal: String,
    pub address: IpAddr,
}

struct State {
    next: u64,
    count: usize,
    degraded: bool,
    sessions: BTreeMap<IpAddr, Session>,
}

/// Local bounded bookkeeping only. Hooks never perform IPC or await a reply.
pub struct Sessions {
    incarnation: u64,
    capacity: usize,
    name_bytes: usize,
    state: Mutex<State>,
    pub changed: Notify,
}

impl Sessions {
    pub fn new(
        incarnation: u64,
        capacity: usize,
        name_bytes: usize,
    ) -> Self {
        Self {
            incarnation,
            capacity,
            name_bytes,
            state: Mutex::new(State {
                next: 0,
                count: 0,
                degraded: false,
                sessions: BTreeMap::new(),
            }),
            changed: Notify::new(),
        }
    }
    pub fn snapshot(&self) -> Vec<Session> {
        let state = self.state.lock().expect("session state poisoned");
        if state.degraded {
            Vec::new()
        } else {
            state.sessions.values().cloned().collect()
        }
    }
    pub fn clear(&self) {
        let mut state = self.state.lock().expect("session state poisoned");
        state.sessions.clear();
        state.count = 0;
        state.degraded = false;
        drop(state);
        self.changed.notify_one();
    }
}

impl Observer for Sessions {
    fn connected(
        &self,
        session: QuincySession,
    ) {
        let mut state = self.state.lock().expect("session state poisoned");
        state.count = state.count.saturating_add(1);
        if state.degraded {
            return;
        }
        let next = state.next.checked_add(1).filter(|_| {
            state.sessions.len() < self.capacity
                && session.username.len() <= self.name_bytes
                && !session.username.is_empty()
                && !state.sessions.contains_key(&session.address.addr())
        });
        if let Some(next) = next {
            state.next = next;
            let sequence = state.next;
            let id = format!("s{:016x}-{:016x}", self.incarnation, sequence)
                .try_into()
                .expect("bounded session ID");
            state.sessions.insert(
                session.address.addr(),
                Session {
                    id,
                    sequence,
                    principal: session.username,
                    address: session.address.addr(),
                },
            );
        } else {
            state.degraded = true;
            tracing::error!(
                "DNAT observation capacity unavailable; VPN continues without DNAT admission"
            );
        }
        drop(state);
        self.changed.notify_one();
    }
    fn disconnected(
        &self,
        session: QuincySession,
    ) {
        let mut state = self.state.lock().expect("session state poisoned");
        state.count = state.count.saturating_sub(1);
        state.sessions.remove(&session.address.addr());
        if state.count == 0 {
            state.degraded = false;
        }
        drop(state);
        self.changed.notify_one();
    }
}

#[cfg(test)]
#[path = "../tests/state.rs"]
mod tests;
