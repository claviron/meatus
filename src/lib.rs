//! A native Claviron service. Quincy owns VPN IO; Scutum owns firewall leases.
pub mod config;
mod leases;
mod state;

use std::future::Future;
use std::sync::Arc;

use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use claviron::orbit::invoke::{Client, Failure};
use claviron::orbit::link::Connection;
use claviron::runtime::{Component, RunContext, Runnable, SharedState};
use quincy::config::{FromPath, ServerConfig};
use quincy::network::interface::quincy_tun::QuincyTunInterface;
use quincy_server::server::QuincyServer;
use tokio_util::sync::CancellationToken;

use config::Config;
use state::Sessions;

/// Construction validates configuration only; the runtime owns all IO.
pub struct MeatusService {
    config: Config,
}

impl MeatusService {
    pub fn new(config: Config) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    pub async fn run_until(
        &self,
        shutdown: impl Future<Output = ()> + Send,
    ) -> Result<()> {
        let mut random = [0; 8];
        getrandom::fill(&mut random)
            .map_err(|e| anyhow!("incarnation unavailable: {e}"))?;
        let incarnation = u64::from_ne_bytes(random);
        ensure!(incarnation != 0, "invalid zero incarnation");
        let sessions = Arc::new(Sessions::new(
            incarnation,
            self.config.max_sessions,
            self.config.max_name_bytes,
        ));
        let server = QuincyServer::new(ServerConfig::from_path(
            &self.config.quincy_config,
            "QUINCY_",
        )?)?;
        let Some(capability) = &self.config.dnat else {
            server
                .run_until::<QuincyTunInterface>(async {
                    shutdown.await;
                    Ok(())
                })
                .await?;
            return Ok(());
        };
        let server = server.with_observer(sessions.clone());
        let vpn_stop = CancellationToken::new();
        let sync_stop = CancellationToken::new();
        let vpn = async {
            let result = server
                .run_until::<QuincyTunInterface>(async {
                    vpn_stop.cancelled().await;
                    Ok(())
                })
                .await;
            sessions.clear();
            sync_stop.cancel();
            result
        };
        let transport = async {
            loop {
                if sync_stop.is_cancelled() {
                    return;
                }
                if let Err(error) = self
                    .dnat_connection(
                        sessions.clone(),
                        capability,
                        incarnation,
                        sync_stop.clone(),
                    )
                    .await
                {
                    tracing::warn!(%error, "DNAT transport unavailable; VPN remains active");
                }
                // The declared renewal cadence also bounds reconnect attempts.
                tokio::select! {
                    () = sync_stop.cancelled() => return,
                    () = tokio::time::sleep(std::time::Duration::from_millis(capability.renew_interval_ms)) => {},
                }
            }
        };
        tokio::pin!(vpn, transport, shutdown);
        let mut vpn_result = None;
        let mut stopping = false;
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown, if !stopping => { stopping = true; vpn_stop.cancel(); },
                result = &mut vpn, if vpn_result.is_none() => { vpn_result = Some(result); },
                () = &mut transport => {
                    return match vpn_result { Some(result) => result.map_err(Into::into), None => vpn.await.map_err(Into::into) };
                }
            }
        }
    }

    async fn dnat_connection(
        &self,
        sessions: Arc<Sessions>,
        capability: &config::DnatCapability,
        incarnation: u64,
        sync_stop: CancellationToken,
    ) -> Result<()> {
        let connection = Connection::open_with_access(
            &self.config.link.fleet,
            self.config.link.spec(),
            &self.config.source,
            "vpn",
            incarnation,
            &self.config.link.access()?,
        )?;
        let (handle, driver) =
            connection.invocations(self.config.link.invoke.limits())?;
        let mut client = Client::new();
        client.register(
            &self.config.target,
            handle.route(&self.config.target)?,
        )?;
        let driver_stop = CancellationToken::new();
        let link_broken = CancellationToken::new();
        let driving = async {
            let result = driver
                .run_until(
                    driver_stop.cancelled(),
                    |_| async { Err(Failure::Unsupported) },
                    |_| async {
                        sessions.changed.notify_one();
                        Ok(())
                    },
                )
                .await;
            link_broken.cancel();
            result
        };
        let syncing = async {
            let result = leases::synchronize(
                &client,
                &self.config.target,
                sessions.clone(),
                capability,
                sync_stop.clone(),
                link_broken.clone(),
            )
            .await;
            driver_stop.cancel();
            result
        };
        let (driven, synced) = tokio::join!(driving, syncing);
        driven?;
        synced
    }
}

impl<S: SharedState> Component<S> for MeatusService {
    type Deps = ();
    fn name(&self) -> &'static str {
        "meatus"
    }
    fn as_runnable(self: Arc<Self>) -> Option<Arc<dyn Runnable<S, ()>>> {
        Some(self)
    }
}

#[async_trait]
impl<S: SharedState> Runnable<S> for MeatusService {
    async fn run(
        self: Arc<Self>,
        (): (),
        _state: S,
        context: RunContext,
    ) -> claviron::runtime::Result<()> {
        self.run_until(context.cancelled()).await.map_err(|e| {
            claviron::runtime::Error::Other(e.into_boxed_dyn_error())
        })
    }
}
