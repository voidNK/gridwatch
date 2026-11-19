//! Plain data types shared between the collector, the store and the UI.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum AuthMethod {
    /// SSH agent first, then the default key files in `~/.ssh`.
    Auto,
    KeyFile {
        path: String,
    },
    Password,
}

/// How a machine is reached.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServerKind {
    #[default]
    Ssh,
    /// The computer gridwatch runs on.
    Local,
    /// The default WSL distribution of this Windows computer.
    Wsl,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub kind: ServerKind,
}

impl ServerConfig {
    pub fn new_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{nanos:x}")
    }

    /// The entry for this computer or its WSL distribution; there is at most one of each.
    pub fn builtin(kind: ServerKind) -> Self {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let host = env("COMPUTERNAME")
            .or_else(|| env("HOSTNAME"))
            .or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|h| h.trim().to_string())
                    .filter(|h| !h.is_empty())
            })
            .unwrap_or_else(|| "This computer".to_string());
        let wsl = kind == ServerKind::Wsl;
        Self {
            id: if wsl { "wsl" } else { "local" }.to_string(),
            name: if wsl { format!("{host} (WSL)") } else { host.clone() },
            host,
            port: 0,
            user: env("USERNAME").or_else(|| env("USER")).unwrap_or_default(),
            auth: AuthMethod::Auto,
            kind,
        }
    }

    pub fn address(&self) -> String {
        match self.kind {
            ServerKind::Local => return "this computer".to_string(),
            ServerKind::Wsl => return "Linux on this computer".to_string(),
            ServerKind::Ssh => {}
        }
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

/// Traffic of one network interface between two polls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetIface {
    pub name: String,
    /// Bytes per second.
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// Bytes since the interface came up.
    pub rx_total: u64,
    pub tx_total: u64,
    /// A real port. Only these count towards the totals of the machine; bridges,
    /// tunnels and container interfaces carry the same traffic a second time.
    pub physical: bool,
}

/// Transfer rates of one physical disk between two polls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiskIo {
    pub name: String,
    pub read_bps: f64,
    pub write_bps: f64,
    /// Read and write operations per second.
    pub iops: f64,
    /// Share of time the disk was busy, in percent. Not reported by macOS.
    pub util: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorKind {
    Cpu,
    Gpu,
    Disk,
    /// Mainboard, network card and anything else; shown but never alerted on.
    Other,
}

impl SensorKind {
    pub fn label(self) -> &'static str {
        match self {
            SensorKind::Cpu => "CPU",
            SensorKind::Gpu => "GPU",
            SensorKind::Disk => "Disk",
            SensorKind::Other => "Other",
        }
    }
}

/// A graphics card, as reported by nvidia-smi, the amdgpu driver or macmon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gpu {
    pub name: String,
    /// Percent of time the GPU was busy.
    pub util: f32,
    pub mem_used: u64,
    /// 0 when the GPU has no memory of its own (Apple Silicon, NVIDIA GB10 and other unified designs).
    pub mem_total: u64,
    pub temp: Option<f32>,
    /// Watts
    pub power: Option<f32>,
}

/// One temperature reading.
#[derive(Clone, Debug, PartialEq)]
pub struct Sensor {
    /// Driver that reports it, e.g. `k10temp`, `nvme`, `nvme #2`.
    pub chip: String,
    /// Name of the reading on that chip, e.g. `Tctl`, `Composite`; may be empty.
    pub label: String,
    pub temp: f32,
}

impl Sensor {
    pub fn kind(&self) -> SensorKind {
        let chip = self.chip.to_lowercase();
        let any = |names: &[&str]| names.iter().any(|n| chip.starts_with(n));
        if any(&["k10temp", "coretemp", "zenpower", "cpu", "x86_pkg_temp", "soc"]) {
            SensorKind::Cpu
        } else if any(&["amdgpu", "radeon", "nouveau", "nvidia", "gpu"]) {
            SensorKind::Gpu
        } else if any(&["nvme", "drivetemp"]) {
            SensorKind::Disk
        } else {
            SensorKind::Other
        }
    }

    /// Temperatures from which the reading counts as warm and as too hot.
    /// Spinning and SATA disks tolerate far less than chips do.
    pub fn limits(&self) -> (f32, f32) {
        match self.kind() {
            SensorKind::Disk if self.chip.starts_with("drivetemp") => (50.0, 60.0),
            SensorKind::Disk => (70.0, 80.0),
            SensorKind::Gpu => (85.0, 95.0),
            _ => (80.0, 90.0),
        }
    }
}

/// The hottest reading of every chip, in the order the chips were reported.
pub fn chip_temps(sensors: &[Sensor]) -> Vec<(String, f32)> {
    let mut out: Vec<(String, f32)> = Vec::new();
    for s in sensors {
        match out.iter_mut().find(|(chip, _)| *chip == s.chip) {
            Some((_, temp)) => *temp = temp.max(s.temp),
            None => out.push((s.chip.clone(), s.temp)),
        }
    }
    out
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
    pub is_mac: bool,
    /// This computer, read through the Windows API instead of a shell.
    pub is_local_windows: bool,
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
    /// Held by programs and the kernel; what is left is available.
    pub mem_used: u64,
    /// File cache and buffers: in use, but given back as soon as programs need it.
    pub mem_cached: u64,
    /// Not used for anything.
    pub mem_free: u64,
    /// Finer breakdown as the operating system reports it; the entries overlap.
    pub mem_detail: Vec<(String, u64)>,
    pub swap_total: u64,
    pub swap_used: u64,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub net: Vec<NetIface>,
    pub disks: Vec<Disk>,
    pub disk_io: Vec<DiskIo>,
    /// Hottest CPU sensor, or the hottest sensor that is not a disk if the CPU has none.
    pub temp: Option<f32>,
    pub sensors: Vec<Sensor>,
    pub gpus: Vec<Gpu>,
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
    pub mem_cached: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub temp: Option<f32>,
    pub cores: Vec<f32>,
    /// Sum over all physical disks, bytes per second.
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    /// Utilisation of the busiest disk in percent.
    pub disk_util: Option<f32>,
    /// Hottest reading per chip.
    pub temps: Vec<(String, f32)>,
    /// (utilisation percent, memory used) per GPU.
    pub gpus: Vec<(f32, u64)>,
    /// (interface, received, sent) in bytes per second.
    pub nets: Vec<(String, f64, f64)>,
}

/// How many points a chart shows at most; a longer range puts several samples into one.
pub const HISTORY_POINTS: i64 = 720;

impl HistPoint {
    /// Fold another sample into this point, which then stands for `count` samples:
    /// numbers become the running mean, lists take the newest values.
    pub fn merge(&mut self, new: &HistPoint, count: u32) {
        let n = count.max(1) as f64;
        let mean = |old: f64, new: f64| old + (new - old) / n;
        let mean32 = |old: f32, new: f32| mean(old as f64, new as f64) as f32;
        self.ts = new.ts;
        self.cpu = CpuUsage {
            total: mean32(self.cpu.total, new.cpu.total),
            user: mean32(self.cpu.user, new.cpu.user),
            system: mean32(self.cpu.system, new.cpu.system),
            iowait: mean32(self.cpu.iowait, new.cpu.iowait),
            steal: mean32(self.cpu.steal, new.cpu.steal),
        };
        for i in 0..3 {
            self.load[i] = mean32(self.load[i], new.load[i]);
        }
        self.mem_used = mean(self.mem_used as f64, new.mem_used as f64) as u64;
        self.mem_cached = mean(self.mem_cached as f64, new.mem_cached as f64) as u64;
        self.mem_total = new.mem_total;
        self.swap_used = mean(self.swap_used as f64, new.swap_used as f64) as u64;
        self.rx_bps = mean(self.rx_bps, new.rx_bps);
        self.tx_bps = mean(self.tx_bps, new.tx_bps);
        self.disk_read_bps = mean(self.disk_read_bps, new.disk_read_bps);
        self.disk_write_bps = mean(self.disk_write_bps, new.disk_write_bps);
        self.temp = match (self.temp, new.temp) {
            (Some(old), Some(new)) => Some(mean32(old, new)),
            (old, new) => new.or(old),
        };
        self.disk_util = match (self.disk_util, new.disk_util) {
            (Some(old), Some(new)) => Some(mean32(old, new)),
            (old, new) => new.or(old),
        };
        if self.cores.len() == new.cores.len() {
            for (old, new) in self.cores.iter_mut().zip(&new.cores) {
                *old = mean32(*old, *new);
            }
        } else {
            self.cores = new.cores.clone();
        }
        self.temps = new.temps.clone();
        self.gpus = new.gpus.clone();
        self.nets = new.nets.clone();
    }

    pub fn from_sample(s: &Sample) -> Self {
        Self {
            ts: s.ts,
            cpu: s.cpu,
            load: s.load,
            mem_used: s.mem_used,
            mem_total: s.mem_total,
            swap_used: s.swap_used,
            mem_cached: s.mem_cached,
            rx_bps: s.rx_bps,
            tx_bps: s.tx_bps,
            temp: s.temp,
            cores: s.cores.clone(),
            disk_read_bps: s.disk_io.iter().map(|d| d.read_bps).sum(),
            disk_write_bps: s.disk_io.iter().map(|d| d.write_bps).sum(),
            disk_util: s.disk_io.iter().filter_map(|d| d.util).reduce(f32::max),
            temps: chip_temps(&s.sensors),
            gpus: s.gpus.iter().map(|g| (g.util, g.mem_used)).collect(),
            // Container interfaces come and go under random names; their history would only be noise.
            nets: s
                .net
                .iter()
                .filter(|n| !n.name.starts_with("veth"))
                .take(16)
                .map(|n| (n.name.clone(), n.rx_bps, n.tx_bps))
                .collect(),
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
    /// What is built into the machine; read once after connecting.
    pub hardware: Option<crate::hardware::Hardware>,
    /// What the user asked to be done on this machine; the collector takes and runs them.
    pub commands: Vec<crate::actions::Command>,
    /// What became of the last command.
    pub feedback: Option<crate::actions::Feedback>,
    /// Ends the collector's wait between two polls, so a command runs at once.
    pub wake: Arc<tokio::sync::Notify>,
    /// The suggestions for the latest sample, worked out once when it arrives.
    pub findings: Vec<crate::suggest::Suggestion>,
    /// How many samples the newest point of `history` stands for.
    pub merged: u32,
}

impl ServerState {
    /// Take in a fresh sample and drop everything older than `keep_secs`.
    ///
    /// A chart has room for about [`HISTORY_POINTS`] points. Over a long range several
    /// samples therefore share one point, exactly as the stored history is read back,
    /// so the amount of data per server stays the same however long gridwatch runs.
    pub fn push(&mut self, sample: Sample, docker: DockerState, keep_secs: i64) {
        let ts = sample.ts;
        let bucket = (keep_secs / HISTORY_POINTS).max(1);
        let point = HistPoint::from_sample(&sample);
        match self.history.back_mut() {
            Some(last) if bucket > 1 && last.ts / bucket == ts / bucket => {
                self.merged += 1;
                last.merge(&point, self.merged);
            }
            _ => {
                self.merged = 1;
                self.history.push_back(point);
            }
        }
        if let DockerState::Ok(list) = &docker {
            for c in list.iter().filter(|c| c.has_stats) {
                let series = self.container_history.entry(c.name.clone()).or_default();
                match series.back_mut() {
                    // The newest value stands for the point.
                    Some(last) if bucket > 1 && last.0 / bucket == ts / bucket => *last = (ts, c.cpu, c.mem_used),
                    _ => series.push_back((ts, c.cpu, c.mem_used)),
                }
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
        self.findings = crate::suggest::analyze(self);
    }
}

pub type Shared = Arc<Mutex<ServerState>>;

/// Bytes per second, for disks (network rates are shown in bits).
pub fn fmt_bytes_rate(bps: f64) -> String {
    format!("{}/s", fmt_bytes(bps.max(0.0) as u64))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, cpu: f32) -> Sample {
        Sample {
            ts,
            cpu: CpuUsage {
                total: cpu,
                ..Default::default()
            },
            cores: vec![cpu, 0.0],
            mem_total: 100,
            ..Default::default()
        }
    }

    #[test]
    fn a_long_range_keeps_a_fixed_number_of_points() {
        // One sample every 2 seconds for a day, shown over 24 hours: 120 seconds per point.
        let mut state = ServerState::default();
        let day = 86_400;
        for i in 0..(day / 2) {
            state.push(sample(i * 2, (i % 60) as f32), DockerState::Unknown, day);
        }
        assert_eq!(state.history.len() as i64, HISTORY_POINTS);
        // Each point is the mean of its 60 samples: 0, 1, ... 59.
        let point = &state.history[10];
        assert!((point.cpu.total - 29.5).abs() < 0.01, "{}", point.cpu.total);
        assert!((point.cores[0] - 29.5).abs() < 0.01);
        assert_eq!(point.ts % 120, 118);
        assert!(state.latest.is_some());
    }

    #[test]
    fn a_short_range_keeps_every_sample() {
        let mut state = ServerState::default();
        for i in 0..100 {
            state.push(sample(i * 2, i as f32), DockerState::Unknown, 300);
        }
        assert_eq!(state.history.len(), 100);
        assert_eq!(state.history[7].cpu.total, 7.0);
        // Older than the range: dropped.
        state.push(sample(1000, 1.0), DockerState::Unknown, 300);
        assert!(state.history.front().unwrap().ts >= 700);
    }
}
