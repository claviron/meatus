//! One independent executor coalesces session state and renews bounded batches.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, ensure};
use claviron::orbit::invoke::Client;
use scutum_management::Identity;
use scutum_management::leases::{
    Command, Mode, OPERATION, Reply, Request, Response,
};
use tokio_util::sync::CancellationToken;

use crate::config::{DnatCapability, DnatTarget, DuplicateSession};
use crate::state::{Session, Sessions};

pub struct Selected {
    pub session: Session,
    pub target: DnatTarget,
}

pub fn select(
    sessions: Vec<Session>,
    config: &DnatCapability,
) -> BTreeMap<Identity, Selected> {
    let mut groups: BTreeMap<Identity, Vec<Selected>> = BTreeMap::new();
    for session in sessions {
        let Some(target) = config.principals.get(&session.principal) else {
            continue;
        };
        groups
            .entry(target.resource.clone())
            .or_default()
            .push(Selected { session, target: target.clone() });
    }
    let mut selected = BTreeMap::new();
    for mut group in groups.into_values() {
        let priority = group
            .iter()
            .map(|s| s.target.priority)
            .max()
            .expect("nonempty group");
        group.retain(|s| s.target.priority == priority);
        if group[0].target.mode == Mode::Dedicated {
            group.sort_by_key(|s| s.session.sequence);
            let winner = match config.duplicate_session {
                DuplicateSession::KeepExisting => group.remove(0),
                DuplicateSession::ReplaceExisting => {
                    group.pop().expect("nonempty group")
                }
            };
            selected.insert(winner.session.id.clone(), winner);
        } else {
            for item in group {
                selected.insert(item.session.id.clone(), item);
            }
        }
    }
    selected
}

struct Held {
    session: Identity,
    resource: Identity,
    lease: Identity,
}

pub async fn synchronize(
    client: &Client,
    target: &str,
    sessions: Arc<Sessions>,
    config: &DnatCapability,
    stop: CancellationToken,
    broken: CancellationToken,
) -> Result<()> {
    let mut held: Vec<Held> = Vec::new();
    let mut expired = BTreeSet::new();
    let mut sequence = 0u64;
    let interval = Duration::from_millis(config.renew_interval_ms);
    let mut renewal = tokio::time::interval_at(
        tokio::time::Instant::now() + interval,
        interval,
    );
    renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    sessions.changed.notify_one();
    loop {
        let renew = tokio::select! {
            biased;
            () = broken.cancelled() => return Err(anyhow!("Link driver unavailable; existing leases expire in Scutum")),
            () = stop.cancelled() => false,
            () = sessions.changed.notified() => false,
            _ = renewal.tick() => true,
        };
        let closing = stop.is_cancelled();
        let desired = if closing {
            BTreeMap::new()
        } else {
            select(sessions.snapshot(), config)
        };
        // Release before replacement acquisition. Failed release leaves the old
        // binding tracked; Scutum also refuses a conflicting exclusive resource.
        let mut index = 0;
        while index < held.len() {
            let current = &held[index];
            if desired.contains_key(&current.session) {
                index += 1;
                continue;
            }
            match call(
                client,
                target,
                &mut sequence,
                Command::Release { lease: current.lease.clone() },
            )
            .await
            {
                Ok(Reply::Released { .. }) => {
                    held.remove(index);
                }
                result => {
                    tracing::warn!(
                        ?result,
                        "DNAT release not confirmed; TTL remains authoritative"
                    );
                    index += 1;
                }
            }
        }
        if closing {
            return Ok(());
        }
        let live_ids: BTreeSet<_> = desired.keys().cloned().collect();
        expired.retain(|id| live_ids.contains(id));
        if renew {
            let ids: Vec<_> = held
                .iter()
                .filter(|h| live_ids.contains(&h.session))
                .map(|h| h.lease.clone())
                .collect();
            if !ids.is_empty() {
                match call(
                    client,
                    target,
                    &mut sequence,
                    Command::Renew { leases: ids },
                )
                .await
                {
                    Ok(Reply::Renewed { leases }) => {
                        for result in leases {
                            if let Err(failure) = result.result {
                                if matches!(
                                    failure.code,
                                    scutum_management::ErrorCode::NotFound
                                ) {
                                    if let Some(h) = held
                                        .iter()
                                        .find(|h| h.lease == result.lease)
                                    {
                                        expired.insert(h.session.clone());
                                    }
                                    held.retain(|h| h.lease != result.lease);
                                }
                                tracing::warn!(code = ?failure.code, "DNAT renewal rejected");
                            }
                        }
                    }
                    result => tracing::warn!(
                        ?result,
                        "DNAT renewal not confirmed; VPN remains active"
                    ),
                }
            }
        }
        let desired_endpoints: BTreeSet<_> = desired
            .values()
            .map(|s| {
                (s.target.resource.clone(), s.session.address, s.target.mode)
            })
            .collect();
        for selected in desired.into_values() {
            let session = selected.session;
            let policy = selected.target;
            if expired.contains(&session.id)
                || held.iter().any(|h| {
                    h.session == session.id
                        || (policy.mode == Mode::Dedicated
                            && h.resource == policy.resource)
                })
            {
                continue;
            }
            match call(
                client,
                target,
                &mut sequence,
                Command::Acquire {
                    session: session.id.clone(),
                    resource: policy.resource.clone(),
                    vpn_ip: session.address,
                    dnat_ip: policy.dnat_ip,
                    mode: policy.mode,
                },
            )
            .await
            {
                Ok(Reply::Acquired { lease, ttl_ms }) => {
                    // A misconfigured cadence never renews an already expired ID.
                    if config.renew_interval_ms >= ttl_ms {
                        tracing::error!(
                            "DNAT renewal interval must be shorter than granted TTL"
                        );
                        let _ = call(
                            client,
                            target,
                            &mut sequence,
                            Command::Release { lease },
                        )
                        .await;
                        expired.insert(session.id);
                    } else {
                        held.push(Held {
                            session: session.id,
                            resource: policy.resource,
                            lease,
                        });
                    }
                }
                Err(error) => {
                    if let Some(conflict) = error
                        .downcast_ref::<scutum_management::Failure>()
                        .and_then(|e| e.lease_conflict.as_ref())
                    {
                        tracing::warn!(vpn_ip = %conflict.vpn_ip, mode = ?conflict.mode,
                            "DNAT conflicts with an active lease");
                        // Only our own lease can be released. The local user
                        // policy, not the firewall, decides whether it is obsolete.
                        if let Some(lease) = &conflict.owned_lease
                            && !desired_endpoints.contains(&(
                                policy.resource.clone(),
                                conflict.vpn_ip,
                                conflict.mode,
                            ))
                        {
                            match call(
                                client,
                                target,
                                &mut sequence,
                                Command::Release { lease: lease.clone() },
                            )
                            .await
                            {
                                Ok(Reply::Released { removed: true }) => {
                                    held.retain(|h| h.lease != *lease);
                                    sessions.changed.notify_one();
                                }
                                result => tracing::warn!(
                                    ?result,
                                    "conflicting lease release not confirmed"
                                ),
                            }
                        }
                    } else {
                        tracing::warn!(%error, "DNAT acquisition not confirmed; VPN remains active");
                    }
                }
                result => tracing::warn!(
                    ?result,
                    "unexpected DNAT acquisition response"
                ),
            }
        }
    }
}

async fn call(
    client: &Client,
    target: &str,
    sequence: &mut u64,
    command: Command,
) -> Result<Reply> {
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| anyhow!("request sequence exhausted"))?;
    let id: Identity = format!("r{sequence}").try_into()?;
    let payload = serde_json::to_vec(&Request { id: id.clone(), command })?;
    let bytes = client.call(target, OPERATION, payload).await?;
    let response: Response = serde_json::from_slice(&bytes)?;
    ensure!(response.id == id, "lease response correlation mismatch");
    response.result.map_err(anyhow::Error::new)
}

#[cfg(test)]
#[path = "../tests/leases.rs"]
mod tests;
