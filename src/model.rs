//! Plain data types shared between the collector, the store and the UI.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum AuthMethod {
    /// SSH agent first, then the default key files in `~/.ssh`.
    Auto,
    KeyFile { path: String },
    Password,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
}

impl ServerConfig {
    pub fn new_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{nanos:x}")
    }

    pub fn address(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.user, self.host)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuUsage {
    pub total: f32,
    pub user: f32,
    pub system: f32,
    pub iowait: f32,
    pub steal: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Disk {
    pub mount: String,
    pub fstype: String,
    pub total: u64,
    pub used: u64,
}

impl Disk {
    pub fn pct(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.used as f32 / self.total as f32 * 100.0
        }
    }
}

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub user: String,
    pub state: char,
    /// Percent of one core (100 = one full core).
    pub cpu: f32,
    pub mem_pct: f32,
    pub rss: u64,
    pub elapsed: u64,
    pub cmd: String,
}

#[derive(Clone, Debug, Default)]
pub struct HostInfo {
    pub kernel: String,
    pub os: String,
    pub cpu_model: String,
    pub reboot_required: bool,
    /// The host reports only total CPU usage (macOS without Python).
    pub no_per_core: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running, exited, restarting, paused, created, dead
    pub state: String,
    pub status: String,
    pub has_stats: bool,
    /// Percent of one core.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_limit: u64,
    pub net: String,
    pub block: String,
    pub pids: u32,
}

impl Container {
    pub fn unhealthy(&self) -> bool {
        self.status.contains("unhealthy")
    }

    /// Exited with a code other than 0 or 143 (SIGTERM, i.e. a normal `docker stop`).
    pub fn failed(&self) -> bool {
        let clean = self.status.starts_with("Exited (0)") || self.status.starts_with("Exited (143)");
        self.state == "dead" || (self.state == "exited" && !clean)
    }
}

#[derive(Clone, Debug, Default)]
pub enum DockerState {
    #[default]
    Unknown,
    NotInstalled,
    Error(String),
    Ok(Vec<Container>),
}

/// One full snapshot of a machine.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub ts: i64,
    pub cpu: CpuUsage,
    pub cores: Vec<f32>,
    pub freqs: Vec<f32>,
    pub load: [f32; 3],
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub disks: Vec<Disk>,
    pub temp: Option<f32>,
    pub uptime: u64,
    pub procs: Vec<Proc>,
    pub proc_count: usize,
    pub zombies: usize,
    pub info: HostInfo,
}

/// The part of a sample that is kept as history (in memory and in SQLite).
#[derive(Clone, Debug, Default)]
pub struct HistPoint {
    pub ts: i64,
    pub cpu: CpuUsage,
    pub load: [f32; 3],
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub temp: Option<f32>,
    pub cores: Vec<f32>,
}

impl HistPoint {
    pub fn from_sample(s: &Sample) -> Self {
        Self {
            ts: s.ts,
            cpu: s.cpu,
            load: s.load,
            mem_used: s.mem_used,
            mem_total: s.mem_total,
            swap_used: s.swap_used,
            rx_bps: s.rx_bps,
            tx_bps: s.tx_bps,
            temp: s.temp,
            cores: s.cores.clone(),
        }
    }
}

/// (timestamp, cpu percent, memory bytes)
pub type ContainerPoint = (i64, f32, u64);

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ConnState {
    #[default]
    Connecting,
    Online,
    /// A password or key passphrase is needed before connecting.
    NeedsSecret(String),
    Error(String),
}

#[derive(Default)]
pub struct ServerState {
    pub conn: ConnState,
    pub latest: Option<Sample>,
    pub docker: DockerState,
    pub history: VecDeque<HistPoint>,
    pub container_history: HashMap<String, VecDeque<ContainerPoint>>,
}

impl ServerState {
    /// Append a fresh sample and drop everything older than `keep_secs`.
    pub fn push(&mut self, sample: Sample, docker: DockerState, keep_secs: i64) {
        let ts = sample.ts;
        self.history.push_back(HistPoint::from_sample(&sample));
        if let DockerState::Ok(list) = &docker {
            for c in list.iter().filter(|c| c.has_stats) {
                self.container_history
                    .entry(c.name.clone())
                    .or_default()
                    .push_back((ts, c.cpu, c.mem_used));
            }
        }
        let cutoff = ts - keep_secs;
        while self.history.front().is_some_and(|p| p.ts < cutoff) {
            self.history.pop_front();
        }
        for series in self.container_history.values_mut() {
            while series.front().is_some_and(|p| p.0 < cutoff) {
                series.pop_front();
            }
        }
        self.container_history.retain(|_, s| !s.is_empty());
        self.latest = Some(sample);
        self.docker = docker;
    }
}

pub type Shared = Arc<Mutex<ServerState>>;

pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn fmt_rate(bytes_per_sec: f64) -> String {
    let bits = bytes_per_sec.max(0.0) * 8.0;
    if bits >= 1e9 {
        format!("{:.2} Gbit/s", bits / 1e9)
    } else if bits >= 1e6 {
        format!("{:.1} Mbit/s", bits / 1e6)
    } else if bits >= 1e3 {
        format!("{:.0} kbit/s", bits / 1e3)
    } else {
        format!("{bits:.0} bit/s")
    }
}

pub fn fmt_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {}s", secs % 60)
    } else {
        format!("{secs}s")
    }
}
