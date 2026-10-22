use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::{Result, ensure};
use claviron::orbit::link::{
    Access, GroupAccess, InboxGeometry, LinkSpec, StreamSpec,
};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub source: String,
    pub target: String,
    pub quincy_config: PathBuf,
    pub max_sessions: usize,
    pub max_name_bytes: usize,
    pub link: Link,
    pub dnat: Option<DnatCapability>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub fleet: String,
    pub capacity: u16,
    pub inbox_kind: u8,
    pub inbox_frames: usize,
    pub frame_bytes: usize,
    pub stream_kind: u8,
    pub stream_slots: usize,
    pub stream_bytes: usize,
    pub access: LinkAccess,
    pub invoke: InvokeLimits,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvokeLimits {
    pub pending_calls: usize,
    pub accepted_calls: usize,
    pub operation_bytes: usize,
    pub request_bytes: usize,
    pub response_bytes: usize,
}

impl InvokeLimits {
    pub fn limits(&self) -> claviron::orbit::link::invoke::Limits {
        claviron::orbit::link::invoke::Limits {
            pending_calls: self.pending_calls,
            accepted_calls: self.accepted_calls,
            operation_bytes: self.operation_bytes,
            request_bytes: self.request_bytes,
            response_bytes: self.response_bytes,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnatCapability {
    pub renew_interval_ms: u64,
    pub batch_limit: usize,
    pub duplicate_session: DuplicateSession,
    pub principals: BTreeMap<String, DnatTarget>,
}

/// User policy stays local; only IPs, mode and an opaque lease key cross Link.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnatTarget {
    pub resource: scutum_management::Identity,
    pub dnat_ip: IpAddr,
    pub mode: scutum_management::leases::Mode,
    /// Higher values take precedence. Equal-priority shared targets form a pool.
    pub priority: i32,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateSession {
    KeepExisting,
    ReplaceExisting,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkAccess {
    OwnerOnly,
    Group { owner_uid: u32, gid: u32, directory: PathBuf },
}

impl Link {
    pub fn spec(&self) -> LinkSpec {
        LinkSpec::new(
            self.capacity,
            self.inbox_kind,
            InboxGeometry::new(self.inbox_frames, self.frame_bytes),
            StreamSpec::new(
                self.stream_kind,
                self.stream_slots,
                self.stream_bytes,
            ),
        )
    }
    pub fn access(&self) -> Result<Access> {
        Ok(match &self.access {
            LinkAccess::OwnerOnly => Access::OwnerOnly,
            LinkAccess::Group { owner_uid, gid, directory } => {
                Access::Group(GroupAccess::new(*owner_uid, *gid, directory)?)
            }
        })
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        for name in [&self.source, &self.target] {
            ensure!(
                !name.is_empty()
                    && name.len() <= claviron::orbitive::link::LANE_NAME_MAX
                    && !name.chars().any(char::is_control),
                "invalid service identity"
            );
        }
        ensure!(
            self.source != self.target,
            "VPN and firewall identities must differ"
        );
        ensure!(
            self.max_sessions > 0 && self.max_name_bytes > 0,
            "positive VPN observation limits required"
        );
        ensure!(
            !self.quincy_config.as_os_str().is_empty(),
            "Quincy config path required"
        );
        ensure!(
            self.link.inbox_kind != self.link.stream_kind,
            "Link kinds must differ"
        );
        claviron::orbitive::link::check_name(
            &self.link.fleet,
            self.link.spec(),
        )?;
        self.link.invoke.limits().validate(self.link.frame_bytes)?;
        if let Some(dnat) = &self.dnat {
            ensure!(
                self.link.invoke.operation_bytes
                    >= scutum_management::leases::OPERATION.len(),
                "invoke operation bound cannot hold lease operation"
            );
            ensure!(
                dnat.renew_interval_ms > 0 && dnat.batch_limit > 0,
                "DNAT renewal interval and batch limit required"
            );
            ensure!(
                self.max_sessions <= dnat.batch_limit,
                "batch must hold configured maximum sessions"
            );
            ensure!(
                dnat.principals.len() <= self.max_sessions,
                "DNAT principal count exceeds session capacity"
            );
            ensure!(
                dnat.principals
                    .keys()
                    .all(|p| !p.is_empty() && p.len() <= self.max_name_bytes),
                "invalid DNAT principal"
            );
            for target in dnat.principals.values() {
                ensure!(target.dnat_ip.is_ipv4(), "DNAT requires IPv4");
                for other in dnat.principals.values() {
                    if other.resource == target.resource {
                        ensure!(
                            other.dnat_ip == target.dnat_ip,
                            "resource has inconsistent DNAT IPs"
                        );
                        ensure!(
                            other.priority != target.priority
                                || other.mode == target.mode,
                            "equal-priority resource policies must use the same mode"
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
