use super::*;

fn config(duplicate_session: DuplicateSession) -> DnatCapability {
    DnatCapability {
        renew_interval_ms: 1000,
        batch_limit: 2,
        duplicate_session,
        principals: [(
            "alice".into(),
            DnatTarget {
                resource: "example-resource".to_owned().try_into().unwrap(),
                dnat_ip: "198.51.100.1".parse().unwrap(),
                mode: Mode::Dedicated,
                priority: 0,
            },
        )]
        .into(),
    }
}
fn session(sequence: u64) -> Session {
    Session {
        id: format!("session-{sequence}").try_into().unwrap(),
        sequence,
        principal: "alice".into(),
        address: "192.0.2.2".parse().unwrap(),
    }
}

#[test]
fn duplicate_policy_is_a_meatus_decision_not_implicit_load_balancing() {
    let first = select(
        vec![session(2), session(1)],
        &config(DuplicateSession::KeepExisting),
    );
    assert_eq!(first.values().next().unwrap().session.sequence, 1);
    let last = select(
        vec![session(1), session(2)],
        &config(DuplicateSession::ReplaceExisting),
    );
    assert_eq!(last.values().next().unwrap().session.sequence, 2);
}

#[test]
fn shared_pool_is_preempted_by_higher_priority_dedicated_user_and_returns_on_departure()
 {
    let mut cfg = config(DuplicateSession::KeepExisting);
    cfg.principals.get_mut("alice").unwrap().mode = Mode::Shared;
    let mut privileged = cfg.principals["alice"].clone();
    privileged.mode = Mode::Dedicated;
    privileged.priority = 10;
    cfg.principals.insert("bob".into(), privileged);
    let shared = vec![session(1), session(2)];
    assert_eq!(select(shared.clone(), &cfg).len(), 2);
    let mut dedicated = session(3);
    dedicated.principal = "bob".into();
    let mut all = shared.clone();
    all.push(dedicated);
    let selected = select(all, &cfg);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected.values().next().unwrap().session.principal, "bob");
    assert_eq!(selected.values().next().unwrap().target.mode, Mode::Dedicated);
    assert_eq!(select(shared, &cfg).len(), 2);
    let mut unknown = session(4);
    unknown.principal = "ungranted".into();
    assert!(select(vec![unknown], &cfg).is_empty());
}

#[tokio::test(start_paused = true)]
async fn shared_members_are_acquired_independently_renewed_together_and_released_by_id()
 {
    use quincy_server::server::lifecycle::{
        Observer, Session as QuincySession,
    };
    let sessions = Arc::new(Sessions::new(7, 2, 64));
    for ip in ["192.0.2.2/24", "192.0.2.3/24"] {
        sessions.connected(QuincySession {
            username: "alice".into(),
            address: ip.parse().unwrap(),
        });
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut client = Client::new();
    client
        .register(
            "example-fw",
            Arc::new(SlowRoute {
                commands: tx,
                gate: Arc::new(tokio::sync::Notify::new()),
                first: false.into(),
            }),
        )
        .unwrap();
    let stop = CancellationToken::new();
    let running = {
        let stop = stop.clone();
        let sessions = sessions.clone();
        tokio::spawn(async move {
            let mut cfg = config(DuplicateSession::KeepExisting);
            cfg.principals.get_mut("alice").unwrap().mode = Mode::Shared;
            synchronize(
                &client,
                "example-fw",
                sessions,
                &cfg,
                stop,
                CancellationToken::new(),
            )
            .await
        })
    };
    let mut ips = BTreeSet::new();
    for _ in 0..2 {
        let Command::Acquire { vpn_ip, dnat_ip, mode, .. } =
            rx.recv().await.unwrap()
        else {
            panic!()
        };
        assert_eq!(mode, Mode::Shared);
        assert_eq!(
            dnat_ip,
            "198.51.100.1".parse::<std::net::IpAddr>().unwrap()
        );
        ips.insert(vpn_ip);
    }
    assert_eq!(ips.len(), 2);
    let Command::Renew { leases } = rx.recv().await.unwrap() else { panic!() };
    assert_eq!(leases.len(), 2);
    sessions.clear();
    let mut released = BTreeSet::new();
    for _ in 0..2 {
        let Command::Release { lease } = rx.recv().await.unwrap() else {
            panic!()
        };
        released.insert(lease);
    }
    assert_eq!(released, leases.into_iter().collect());
    stop.cancel();
    running.await.unwrap().unwrap();
}

struct ConflictingRoute {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    conflict: std::sync::atomic::AtomicBool,
}

#[tokio::test(start_paused = true)]
async fn priority_change_releases_shared_leases_before_dedicated_and_restores_pool()
 {
    use quincy_server::server::lifecycle::{
        Observer, Session as QuincySession,
    };
    let sessions = Arc::new(Sessions::new(7, 3, 64));
    let connection = |name: &str, ip: &str| QuincySession {
        username: name.into(),
        address: ip.parse().unwrap(),
    };
    sessions.connected(connection("alice", "192.0.2.2/24"));
    sessions.connected(connection("alice", "192.0.2.3/24"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut client = Client::new();
    client
        .register(
            "example-fw",
            Arc::new(SlowRoute {
                commands: tx,
                gate: Arc::new(tokio::sync::Notify::new()),
                first: false.into(),
            }),
        )
        .unwrap();
    let stop = CancellationToken::new();
    let running = {
        let stop = stop.clone();
        let sessions = sessions.clone();
        tokio::spawn(async move {
            let mut cfg = config(DuplicateSession::KeepExisting);
            cfg.batch_limit = 3;
            cfg.principals.get_mut("alice").unwrap().mode = Mode::Shared;
            let mut privileged = cfg.principals["alice"].clone();
            privileged.mode = Mode::Dedicated;
            privileged.priority = 10;
            cfg.principals.insert("bob".into(), privileged);
            synchronize(
                &client,
                "example-fw",
                sessions,
                &cfg,
                stop,
                CancellationToken::new(),
            )
            .await
        })
    };
    for _ in 0..2 {
        assert!(matches!(
            rx.recv().await.unwrap(),
            Command::Acquire { mode: Mode::Shared, .. }
        ));
    }
    sessions.connected(connection("bob", "192.0.2.4/24"));
    for _ in 0..2 {
        assert!(matches!(rx.recv().await.unwrap(), Command::Release { .. }));
    }
    assert!(
        matches!(rx.recv().await.unwrap(), Command::Acquire { mode: Mode::Dedicated, vpn_ip, .. } if vpn_ip == "192.0.2.4".parse::<std::net::IpAddr>().unwrap())
    );
    sessions.disconnected(connection("bob", "192.0.2.4/24"));
    assert!(matches!(rx.recv().await.unwrap(), Command::Release { .. }));
    for _ in 0..2 {
        assert!(matches!(
            rx.recv().await.unwrap(),
            Command::Acquire { mode: Mode::Shared, .. }
        ));
    }
    stop.cancel();
    for _ in 0..2 {
        assert!(matches!(rx.recv().await.unwrap(), Command::Release { .. }));
    }
    running.await.unwrap().unwrap();
}

#[async_trait::async_trait]
impl claviron::orbit::invoke::Route for ConflictingRoute {
    async fn call(
        &self,
        request: claviron::orbit::invoke::Request,
    ) -> claviron::orbit::invoke::CallResult<Vec<u8>> {
        let request: Request =
            serde_json::from_slice(&request.payload).unwrap();
        self.commands.send(request.command.clone()).unwrap();
        let result = match request.command {
            Command::Acquire { .. }
                if self
                    .conflict
                    .swap(false, std::sync::atomic::Ordering::SeqCst) =>
            {
                let mut failure = scutum_management::Failure::new(
                    scutum_management::ErrorCode::Conflict,
                    "occupied",
                );
                failure.lease_conflict =
                    Some(scutum_management::leases::Conflict {
                        vpn_ip: "192.0.2.99".parse().unwrap(),
                        mode: Mode::Shared,
                        owned_lease: Some(
                            "obsolete-lease".to_owned().try_into().unwrap(),
                        ),
                    });
                Err(failure)
            }
            Command::Acquire { .. } => Ok(Reply::Acquired {
                lease: "new-lease".to_owned().try_into().unwrap(),
                ttl_ms: 10000,
            }),
            Command::Release { .. } => Ok(Reply::Released { removed: true }),
            Command::Renew { leases } => Ok(Reply::Renewed {
                leases: leases
                    .into_iter()
                    .map(|lease| scutum_management::leases::Renewal {
                        lease,
                        result: Ok(10000),
                    })
                    .collect(),
            }),
        };
        Ok(serde_json::to_vec(&Response { id: request.id, result }).unwrap())
    }
}

#[tokio::test(start_paused = true)]
async fn conflict_ip_is_preserved_and_obsolete_owned_lease_is_released_before_reacquisition()
 {
    use quincy_server::server::lifecycle::{
        Observer, Session as QuincySession,
    };
    let sessions = Arc::new(Sessions::new(7, 2, 64));
    sessions.connected(QuincySession {
        username: "alice".into(),
        address: "192.0.2.2/24".parse().unwrap(),
    });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut client = Client::new();
    client
        .register(
            "example-fw",
            Arc::new(ConflictingRoute { commands: tx, conflict: true.into() }),
        )
        .unwrap();
    let stop = CancellationToken::new();
    let running = {
        let stop = stop.clone();
        tokio::spawn(async move {
            synchronize(
                &client,
                "example-fw",
                sessions,
                &config(DuplicateSession::KeepExisting),
                stop,
                CancellationToken::new(),
            )
            .await
        })
    };
    assert!(matches!(rx.recv().await.unwrap(), Command::Acquire { .. }));
    assert!(
        matches!(rx.recv().await.unwrap(), Command::Release { lease } if lease.as_str() == "obsolete-lease")
    );
    assert!(matches!(rx.recv().await.unwrap(), Command::Acquire { .. }));
    stop.cancel();
    // A pending renewal can precede shutdown; the eventual release must be acknowledged.
    loop {
        if let Command::Release { lease } = rx.recv().await.unwrap() {
            assert_eq!(lease.as_str(), "new-lease");
            break;
        }
    }
    running.await.unwrap().unwrap();
}

struct SlowRoute {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    gate: Arc<tokio::sync::Notify>,
    first: std::sync::atomic::AtomicBool,
}

struct ExpiringRoute(tokio::sync::mpsc::UnboundedSender<Command>);

#[async_trait::async_trait]
impl claviron::orbit::invoke::Route for ExpiringRoute {
    async fn call(
        &self,
        request: claviron::orbit::invoke::Request,
    ) -> claviron::orbit::invoke::CallResult<Vec<u8>> {
        let request: Request =
            serde_json::from_slice(&request.payload).unwrap();
        self.0.send(request.command.clone()).unwrap();
        let result = match request.command {
            Command::Acquire { .. } => Reply::Acquired {
                lease: "example-binding".to_owned().try_into().unwrap(),
                ttl_ms: 10000,
            },
            Command::Renew { leases } => Reply::Renewed {
                leases: leases
                    .into_iter()
                    .map(|lease| scutum_management::leases::Renewal {
                        lease,
                        result: Err(scutum_management::Failure::new(
                            scutum_management::ErrorCode::NotFound,
                            "expired",
                        )),
                    })
                    .collect(),
            },
            Command::Release { .. } => Reply::Released { removed: false },
        };
        Ok(serde_json::to_vec(&Response { id: request.id, result: Ok(result) })
            .unwrap())
    }
}

#[tokio::test(start_paused = true)]
async fn expired_id_is_not_reacquired_by_the_renewal_loop() {
    use quincy_server::server::lifecycle::{
        Observer, Session as QuincySession,
    };
    let sessions = Arc::new(Sessions::new(7, 2, 64));
    sessions.connected(QuincySession {
        username: "alice".into(),
        address: "192.0.2.2/24".parse().unwrap(),
    });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut client = Client::new();
    client.register("example-fw", Arc::new(ExpiringRoute(tx))).unwrap();
    let stop = CancellationToken::new();
    let running = {
        let stop = stop.clone();
        let sessions = sessions.clone();
        tokio::spawn(async move {
            synchronize(
                &client,
                "example-fw",
                sessions,
                &config(DuplicateSession::KeepExisting),
                stop,
                CancellationToken::new(),
            )
            .await
        })
    };
    assert!(matches!(rx.recv().await.unwrap(), Command::Acquire { .. }));
    assert!(matches!(rx.recv().await.unwrap(), Command::Renew { .. }));
    tokio::time::advance(Duration::from_millis(3000)).await;
    tokio::task::yield_now().await;
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    stop.cancel();
    running.await.unwrap().unwrap();
}

#[async_trait::async_trait]
impl claviron::orbit::invoke::Route for SlowRoute {
    async fn call(
        &self,
        request: claviron::orbit::invoke::Request,
    ) -> claviron::orbit::invoke::CallResult<Vec<u8>> {
        let request: Request =
            serde_json::from_slice(&request.payload).unwrap();
        self.commands.send(request.command.clone()).unwrap();
        let reply = match request.command {
            Command::Acquire { session, .. } => {
                if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    self.gate.notified().await;
                }
                Reply::Acquired {
                    lease: format!("lease-{session}").try_into().unwrap(),
                    ttl_ms: 10000,
                }
            }
            Command::Renew { leases } => Reply::Renewed {
                leases: leases
                    .into_iter()
                    .map(|lease| scutum_management::leases::Renewal {
                        lease,
                        result: Ok(10000),
                    })
                    .collect(),
            },
            Command::Release { .. } => Reply::Released { removed: true },
        };
        Ok(serde_json::to_vec(&Response { id: request.id, result: Ok(reply) })
            .unwrap())
    }
}

#[tokio::test(start_paused = true)]
async fn slow_acquisition_does_not_block_hooks_and_release_and_batch_renewal_use_ids()
 {
    use quincy_server::server::lifecycle::{
        Observer, Session as QuincySession,
    };
    let sessions = Arc::new(Sessions::new(7, 2, 64));
    let connection = |ip: &str| QuincySession {
        username: "alice".into(),
        address: ip.parse().unwrap(),
    };
    sessions.connected(connection("192.0.2.2/24"));
    let gate = Arc::new(tokio::sync::Notify::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut client = Client::new();
    client
        .register(
            "example-fw",
            Arc::new(SlowRoute {
                commands: tx,
                gate: gate.clone(),
                first: true.into(),
            }),
        )
        .unwrap();
    let stop = CancellationToken::new();
    let running = {
        let sessions = sessions.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            synchronize(
                &client,
                "example-fw",
                sessions,
                &config(DuplicateSession::ReplaceExisting),
                stop,
                CancellationToken::new(),
            )
            .await
        })
    };
    assert!(matches!(rx.recv().await.unwrap(), Command::Acquire { .. }));
    // No reply exists yet. These lifecycle observations still finish directly.
    sessions.connected(connection("192.0.2.3/24"));
    sessions.disconnected(connection("192.0.2.2/24"));
    assert_eq!(sessions.snapshot().len(), 1);
    gate.notify_one();
    let old = match rx.recv().await.unwrap() {
        Command::Release { lease } => lease,
        other => panic!("{other:?}"),
    };
    assert!(old.as_str().starts_with("lease-s"));
    assert!(matches!(rx.recv().await.unwrap(), Command::Acquire { .. }));
    let current = match rx.recv().await.unwrap() {
        Command::Renew { leases } => {
            assert_eq!(leases.len(), 1);
            leases[0].clone()
        }
        other => panic!("{other:?}"),
    };
    assert_ne!(old, current);
    sessions.disconnected(connection("192.0.2.3/24"));
    assert!(
        matches!(rx.recv().await.unwrap(), Command::Release { lease } if lease == current)
    );
    stop.cancel();
    running.await.unwrap().unwrap();
}
