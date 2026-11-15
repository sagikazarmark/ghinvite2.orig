//! Starting and stopping an emulator instance.

use crate::api::{self, Api};
use crate::auth::Auth;
use crate::calls::Call;
use crate::fidelity::{Manifest, ManifestError, Mismatch};
use crate::seed::{Seed, SeedError};
use crate::state::{Shared, SharedState};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Why an emulator did not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The seed failed validation.
    #[error(transparent)]
    Seed(#[from] SeedError),
    /// The compiled-in fidelity manifest is malformed.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A registered route or behaviour has no manifest entry, or the reverse.
    #[error(transparent)]
    Registry(#[from] Mismatch),
    /// The address could not be bound.
    #[error("octoemu could not listen: {0}")]
    Bind(#[from] io::Error),
}

/// Options for [`Emulator::builder`].
#[derive(Debug)]
#[must_use]
pub struct Builder {
    seed: Seed,
    bind: SocketAddr,
    idle_timeout: Option<Duration>,
    refuse_assumed: bool,
}

impl Builder {
    /// Listens on `addr` instead of `127.0.0.1:0` (an ephemeral loopback
    /// port).
    pub fn bind(mut self, addr: SocketAddr) -> Self {
        self.bind = addr;
        self
    }

    /// Stops the emulator once no request has arrived for `after` (and none
    /// is in flight).
    pub fn idle_timeout(mut self, after: Duration) -> Self {
        self.idle_timeout = Some(after);
        self
    }

    /// Refuses behaviour the fidelity manifest labels `assumed`, like the
    /// seed's `settings.refuse_assumed`.
    pub fn refuse_assumed(mut self) -> Self {
        self.refuse_assumed = true;
        self
    }

    /// Validates the seed, binds and starts serving.
    pub async fn start(self) -> Result<Emulator, StartError> {
        let manifest = Manifest::embedded()?;
        let api = api::github();
        manifest.check(api.route_names(), api::BEHAVIOURS.iter().copied())?;
        self.start_with(api, Arc::new(manifest.clone())).await
    }

    pub(crate) async fn start_with(
        mut self,
        api: Api,
        manifest: Arc<Manifest>,
    ) -> Result<Emulator, StartError> {
        self.seed.settings.refuse_assumed |= self.refuse_assumed;
        self.seed.validate()?;
        let listener = TcpListener::bind(self.bind).await?;
        let addr = listener.local_addr()?;
        let registered: BTreeMap<String, Auth> = api.routes().iter().cloned().collect();
        let host = match addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            ip => ip,
        };
        let root = format!("http://{}", SocketAddr::new(host, addr.port()));
        let shared = Arc::new(Shared::new(
            self.seed,
            manifest,
            registered,
            root.clone(),
            addr.port(),
        ));
        let app = crate::server::router(shared.clone(), api);

        let (stop, mut stop_requested) = watch::channel(false);
        let (stopped_tx, stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stop_requested.wait_for(|stop| *stop).await;
                })
                .await;
            let _ = stopped_tx.send(true);
            result
        });
        if let Some(after) = self.idle_timeout {
            tokio::spawn(idle_watch(shared.clone(), after, stop.clone()));
        }

        Ok(Emulator {
            shared,
            addr,
            api_base: root.clone(),
            web_base: root.clone(),
            avatar_base: format!("{root}/avatars"),
            admin_base: format!("{root}/__octoemu"),
            stop,
            stopped,
            task: Some(task),
        })
    }
}

/// Requests a stop once the emulator has been idle for `after`.
async fn idle_watch(shared: SharedState, after: Duration, stop: watch::Sender<bool>) {
    let mut stop_requested = stop.subscribe();
    loop {
        let wait = match shared.activity.idle_for() {
            Some(idle) if idle >= after => {
                stop.send_replace(true);
                return;
            }
            Some(idle) => after - idle,
            None => after.min(Duration::from_millis(250)),
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = stop_requested.wait_for(|stop| *stop) => return,
        }
    }
}

/// A running emulator. Stops when dropped (or on [`Emulator::shutdown`]).
///
/// One port serves every surface, routed by path:
///
/// | Surface | Base | Stands in for |
/// |---|---|---|
/// | REST API | [`api_base`](Self::api_base) (`/`) | `https://api.github.com` |
/// | Web pages (`/login/oauth/*`) | [`web_base`](Self::web_base) | `https://github.com` |
/// | Avatars (`/avatars/u/{id}`) | [`avatar_base`](Self::avatar_base) | `https://avatars.githubusercontent.com` |
/// | Admin API | [`admin_base`](Self::admin_base) (`/__octoemu`) | — |
#[derive(Debug)]
pub struct Emulator {
    shared: SharedState,
    addr: SocketAddr,
    api_base: String,
    web_base: String,
    avatar_base: String,
    admin_base: String,
    stop: watch::Sender<bool>,
    stopped: watch::Receiver<bool>,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl Emulator {
    /// Starts an emulator on an ephemeral loopback port.
    pub async fn start(seed: Seed) -> Result<Self, StartError> {
        Self::builder(seed).start().await
    }

    /// Options beyond the seed: bind address, idle timeout, refuse-assumed.
    pub fn builder(seed: Seed) -> Builder {
        Builder {
            seed,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            idle_timeout: None,
            refuse_assumed: false,
        }
    }

    /// The REST API base URL (stands in for `https://api.github.com`), no
    /// trailing slash.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// The web base URL (stands in for `https://github.com`), no trailing
    /// slash.
    pub fn web_base(&self) -> &str {
        &self.web_base
    }

    /// The avatar base URL (stands in for
    /// `https://avatars.githubusercontent.com`), no trailing slash.
    pub fn avatar_base(&self) -> &str {
        &self.avatar_base
    }

    /// The admin API base URL, no trailing slash.
    pub fn admin_base(&self) -> &str {
        &self.admin_base
    }

    /// The bound address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The virtual clock's time.
    pub fn now(&self) -> DateTime<Utc> {
        self.shared.now()
    }

    /// Moves the virtual clock forward (whole seconds; at most a century).
    pub fn advance_clock(&self, by: Duration) {
        self.shared.state().clock.advance(by.as_secs());
    }

    /// Adds `more` to the world, validated as a whole; on error nothing
    /// changes.
    pub fn seed(&self, more: Seed) -> Result<(), SeedError> {
        self.shared.merge(more)
    }

    /// The call log, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.shared.state().calls.calls()
    }

    /// Suspends installation `id`, as `PUT /app/installations/{id}/suspended`
    /// does: token creation is refused and existing tokens stop working.
    /// `false` when there is no such installation.
    pub fn suspend_installation(&self, id: u64) -> bool {
        crate::apps::set_suspended(&self.shared, id, None, true)
    }

    /// Lifts a suspension. `false` when there is no such installation.
    pub fn unsuspend_installation(&self, id: u64) -> bool {
        crate::apps::set_suspended(&self.shared, id, None, false)
    }

    /// Uninstalls installation `id`: it disappears and its tokens stop
    /// working. `false` when there is no such installation.
    pub fn uninstall(&self, id: u64) -> bool {
        crate::apps::uninstall(&self.shared, id)
    }

    /// Empties the call log.
    pub fn clear_calls(&self) {
        self.shared.state().calls.clear();
    }

    /// The `GET /__octoemu/state` dump.
    pub fn state(&self) -> serde_json::Value {
        self.shared.dump()
    }

    /// Resolves once the emulator has stopped (idle timeout or shutdown).
    pub async fn stopped(&self) {
        let mut stopped = self.stopped.clone();
        let _ = stopped.wait_for(|stopped| *stopped).await;
    }

    /// Stops serving and waits for in-flight requests to finish.
    pub async fn shutdown(mut self) -> io::Result<()> {
        self.stop.send_replace(true);
        match self.task.take() {
            Some(task) => task.await.map_err(io::Error::other)?,
            None => Ok(()),
        }
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
