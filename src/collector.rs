//! Background tasks: one per server, polling over SSH (or generating demo data).

use crate::model::*;
use crate::parse::{self, RawSystem};
use crate::ssh::{ConnectError, Session};
use crate::store::Store;
use crate::{demo, now_ts};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// Things every collector task needs.
pub struct Env {
    pub store: Arc<Store>,
    pub known_hosts: PathBuf,
    pub repaint: eframe::egui::Context,
    /// Poll interval in seconds; can be changed while running.
    pub interval: AtomicU64,
    /// How much history the UI currently shows, in seconds.
    pub keep_secs: AtomicI64,
}

impl Env {
    fn interval(&self) -> Duration {
        Duration::from_secs(self.interval.load(Ordering::Relaxed).max(1))
    }

    fn publish(&self, server_id: &str, shared: &Shared, sample: Sample, docker: DockerState) {
        let point = HistPoint::from_sample(&sample);
        if let Err(e) = self.store.insert(server_id, &point, &docker) {
            eprintln!("gridwatch: cannot store sample: {e:#}");
        }
        let mut state = shared.lock().unwrap();
        state.conn = ConnState::Online;
        state.push(sample, docker, self.keep_secs.load(Ordering::Relaxed));
        drop(state);
        self.repaint.request_repaint();
    }
}

fn set_conn(shared: &Shared, env: &Env, conn: ConnState) {
    shared.lock().unwrap().conn = conn;
    env.repaint.request_repaint();
}

pub fn spawn(
    rt: &tokio::runtime::Handle,
    cfg: ServerConfig,
    secret: Option<String>,
    shared: Shared,
    env: Arc<Env>,
) -> JoinHandle<()> {
    rt.spawn(async move {
        loop {
            set_conn(&shared, &env, ConnState::Connecting);
            let session = match Session::connect(&cfg, secret.as_deref(), &env.known_hosts).await {
                Ok(session) => session,
                Err(ConnectError::NeedsSecret(prompt)) => {
                    // The UI restarts this task once the user has typed the secret.
                    set_conn(&shared, &env, ConnState::NeedsSecret(prompt));
                    return;
                }
                Err(ConnectError::Failed(msg)) => {
                    set_conn(&shared, &env, ConnState::Error(msg));
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };
            let err = poll_loop(&session, &cfg, &shared, &env).await.unwrap_err();
            session.close().await;
            set_conn(&shared, &env, ConnState::Error(format!("{err:#}")));
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

async fn poll_system(session: &Session) -> Result<(RawSystem, Instant)> {
    let out = session.exec(&parse::wrap(parse::SYSTEM_SCRIPT), Duration::from_secs(20)).await?;
    Ok((parse::parse_system(&out)?, Instant::now()))
}

async fn poll_docker(session: &Session) -> DockerState {
    match session.exec(&parse::wrap(parse::DOCKER_SCRIPT), Duration::from_secs(30)).await {
        Ok(out) => parse::parse_docker(&out),
        Err(e) => DockerState::Error(format!("{e:#}")),
    }
}

/// Runs until the connection breaks; only ever returns an error.
async fn poll_loop(session: &Session, cfg: &ServerConfig, shared: &Shared, env: &Env) -> Result<()> {
    // CPU and network values are rates, so a first baseline poll is needed.
    let (mut prev, mut prev_at) = poll_system(session).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    loop {
        let started = Instant::now();
        let (system, docker) = tokio::join!(poll_system(session), poll_docker(session));
        let (raw, at) = system?;
        let dt = at.duration_since(prev_at).as_secs_f64();
        let sample = parse::build_sample(&prev, &raw, dt, now_ts());
        env.publish(&cfg.id, shared, sample, docker);
        (prev, prev_at) = (raw, at);
        tokio::time::sleep(env.interval().saturating_sub(started.elapsed())).await;
    }
}

/// Simulated machine for `--demo`: an hour of history, then live updates.
pub fn spawn_demo(
    rt: &tokio::runtime::Handle,
    cfg: ServerConfig,
    profile: usize,
    shared: Shared,
    env: Arc<Env>,
) -> JoinHandle<()> {
    let step = env.interval().as_secs() as i64;
    let now = now_ts();
    let mut ts = now - 3600;
    while ts < now {
        let (sample, docker) = demo::generate(profile, ts);
        env.publish(&cfg.id, &shared, sample, docker);
        ts += step;
    }
    rt.spawn(async move {
        loop {
            let (sample, docker) = demo::generate(profile, now_ts());
            env.publish(&cfg.id, &shared, sample, docker);
            tokio::time::sleep(env.interval()).await;
        }
    })
}
