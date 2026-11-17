//! Background tasks: one per server, polling over SSH, on this computer, or generating demo data.

use crate::model::*;
use crate::notify::Notifier;
use crate::parse::{self, RawSystem};
use crate::ssh::{ConnectError, Session};
use crate::store::Store;
use crate::{demo, hardware, now_ts};
use anyhow::{Context, Result};
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
    pub notifier: Notifier,
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

    /// Tell the user if this server has new trouble.
    fn alert(&self, cfg: &ServerConfig, shared: &Shared) {
        self.notifier.check(&cfg.id, &cfg.name, &shared.lock().unwrap());
    }
}

fn set_conn(shared: &Shared, env: &Env, conn: ConnState) {
    shared.lock().unwrap().conn = conn;
    env.repaint.request_repaint();
}

/// Run a program on this computer without a console window flashing up, feed it `input`
/// and return what it prints.
pub fn run_local(program: &str, args: &[&str], input: &str) -> Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().with_context(|| format!("cannot start {program}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A program that does not read its input closes the pipe; that is not an error.
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    Ok(text)
}

/// Where the collection scripts run.
enum Source {
    Ssh(Session),
    /// A shell on this computer that reads the script from its standard input,
    /// which avoids every quoting problem (`sh`, or `wsl.exe -e sh`).
    Shell {
        program: &'static str,
        args: &'static [&'static str],
    },
}

impl Source {
    async fn run(&self, script: &str, timeout: Duration) -> Result<String> {
        match self {
            Source::Ssh(session) => session.exec(&parse::wrap(script), timeout).await,
            Source::Shell { program, args } => {
                let (program, args, script) = (*program, *args, format!("{script}\n"));
                let job = tokio::task::spawn_blocking(move || run_local(program, args, &script));
                tokio::time::timeout(timeout, job).await.context("timed out")??
            }
        }
    }
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
                    env.alert(&cfg, &shared);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };
            let source = Source::Ssh(session);
            let err = poll_loop(&source, &cfg, &shared, &env).await.unwrap_err();
            if let Source::Ssh(session) = &source {
                session.close().await;
            }
            set_conn(&shared, &env, ConnState::Error(format!("{err:#}")));
            env.alert(&cfg, &shared);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

/// This computer, or the WSL distribution running on it: no connection, no login.
pub fn spawn_local(rt: &tokio::runtime::Handle, cfg: ServerConfig, shared: Shared, env: Arc<Env>) -> JoinHandle<()> {
    rt.spawn(async move {
        loop {
            set_conn(&shared, &env, ConnState::Connecting);
            let err = match cfg.kind {
                ServerKind::Wsl => {
                    let source = Source::Shell {
                        program: "wsl.exe",
                        args: &["-e", "sh"],
                    };
                    poll_loop(&source, &cfg, &shared, &env).await.unwrap_err()
                }
                _ => local_loop(&cfg, &shared, &env).await.unwrap_err(),
            };
            set_conn(&shared, &env, ConnState::Error(format!("{err:#}")));
            env.alert(&cfg, &shared);
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    })
}

/// Linux and macOS answer the same scripts locally that they answer over SSH.
#[cfg(not(windows))]
async fn local_loop(cfg: &ServerConfig, shared: &Shared, env: &Env) -> Result<()> {
    let source = Source::Shell {
        program: "sh",
        args: &[],
    };
    poll_loop(&source, cfg, shared, env).await
}

/// Windows has no shell to ask, so the values come from the Windows API.
#[cfg(windows)]
async fn local_loop(cfg: &ServerConfig, shared: &Shared, env: &Env) -> Result<()> {
    use crate::local_windows::Local;
    use std::sync::Mutex;
    let local = Arc::new(Mutex::new(Local::new()?));
    // The inventory takes PowerShell a few seconds; it must not hold up the first numbers.
    let (inventory, repaint) = (shared.clone(), env.repaint.clone());
    tokio::task::spawn_blocking(move || {
        let args = ["-NoProfile", "-NonInteractive", "-Command", "-"];
        if let Ok(out) = run_local("powershell", &args, hardware::WINDOWS_SCRIPT) {
            inventory.lock().unwrap().hardware = Some(hardware::parse(&out));
            repaint.request_repaint();
        }
    });
    // CPU usage is measured between two readings.
    tokio::time::sleep(Duration::from_secs(1)).await;
    loop {
        let started = Instant::now();
        let reader = local.clone();
        let system = tokio::task::spawn_blocking(move || reader.lock().unwrap().sample(now_ts()));
        let docker = tokio::task::spawn_blocking(crate::local_windows::docker);
        let (sample, docker) = tokio::join!(system, docker);
        env.publish(&cfg.id, shared, sample?, docker?);
        env.alert(cfg, shared);
        tokio::time::sleep(env.interval().saturating_sub(started.elapsed())).await;
    }
}

async fn poll_system(source: &Source) -> Result<(RawSystem, Instant)> {
    let out = source.run(&parse::system_script(), Duration::from_secs(20)).await?;
    Ok((parse::parse_system(&out)?, Instant::now()))
}

async fn poll_docker(source: &Source) -> DockerState {
    match source.run(parse::DOCKER_SCRIPT, Duration::from_secs(30)).await {
        Ok(out) => parse::parse_docker(&out),
        Err(e) => DockerState::Error(format!("{e:#}")),
    }
}

/// Runs until the connection breaks; only ever returns an error.
async fn poll_loop(source: &Source, cfg: &ServerConfig, shared: &Shared, env: &Env) -> Result<()> {
    // CPU and network values are rates, so a first baseline poll is needed.
    let (mut prev, mut prev_at) = poll_system(source).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut inventory_read = false;
    loop {
        let started = Instant::now();
        let (system, docker) = tokio::join!(poll_system(source), poll_docker(source));
        let (raw, at) = system?;
        let dt = at.duration_since(prev_at).as_secs_f64();
        let sample = parse::build_sample(&prev, &raw, dt, now_ts());
        env.publish(&cfg.id, shared, sample, docker);
        env.alert(cfg, shared);
        (prev, prev_at) = (raw, at);
        // The inventory does not change; read it once, after the first numbers are on screen.
        if !inventory_read {
            inventory_read = true;
            if let Ok(out) = source.run(&hardware::script(), Duration::from_secs(60)).await {
                shared.lock().unwrap().hardware = Some(hardware::parse(&out));
                env.repaint.request_repaint();
            }
        }
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
    shared.lock().unwrap().hardware = Some(demo::hardware(profile));
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
            env.alert(&cfg, &shared);
            tokio::time::sleep(env.interval()).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poll a local shell twice, the way this computer and WSL are watched:
    /// `cargo test local_shell -- --ignored --nocapture` (uses WSL on Windows).
    #[test]
    #[ignore]
    fn local_shell() {
        let source = if cfg!(windows) {
            Source::Shell {
                program: "wsl.exe",
                args: &["-e", "sh"],
            }
        } else {
            Source::Shell {
                program: "sh",
                args: &[],
            }
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (a, _) = poll_system(&source).await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
            let (b, _) = poll_system(&source).await.unwrap();
            let mut s = parse::build_sample(&a, &b, 2.0, 0);
            s.procs.truncate(3);
            println!("{s:#?}\n{:?}", poll_docker(&source).await);
            assert!(!s.cores.is_empty() && s.mem_total > 0 && s.proc_count > 0);
        });
    }
}
