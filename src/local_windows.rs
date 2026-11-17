//! This computer when it runs Windows: values from the Windows API (through `sysinfo`)
//! instead of a shell script. GPUs and containers come from `nvidia-smi` and `docker`
//! if they are installed.

use crate::collector::run_local;
use crate::model::*;
use crate::parse;
use anyhow::Result;
use std::collections::HashMap;
use std::time::Instant;
use sysinfo::{Components, Disks, Networks, ProcessesToUpdate, System, Users};

#[link(name = "kernel32")]
unsafe extern "system" {
    /// Three FILETIME values in 100 ns units; a FILETIME has the layout of a little-endian u64.
    fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
}

/// (idle, kernel including idle, user) CPU time since boot.
fn system_times() -> Option<(u64, u64, u64)> {
    let (mut idle, mut kernel, mut user) = (0u64, 0u64, 0u64);
    // SAFETY: the three pointers are valid for writes of 8 bytes for the duration of the call.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    (ok != 0).then_some((idle, kernel, user))
}

pub struct Local {
    sys: System,
    networks: Networks,
    disks: Disks,
    users: Users,
    components: Option<Components>,
    times: Option<(u64, u64, u64)>,
    net: (u64, u64),
    /// interface -> (bytes received, bytes sent)
    net_bytes: HashMap<String, (u64, u64)>,
    /// mount point -> (bytes read, bytes written)
    disk_bytes: HashMap<String, (u64, u64)>,
    at: Instant,
}

/// Adapters that only exist in software would count the same traffic twice.
fn physical(interface: &str) -> bool {
    let name = interface.to_lowercase();
    !["loopback", "vethernet", "wsl", "virtual", "vmware", "bluetooth", "tailscale", "wintun"].iter().any(|v| name.contains(v))
}

impl Local {
    pub fn new() -> Result<Self> {
        let mut local = Self {
            sys: System::new_all()?,
            networks: Networks::new_with_refreshed_list()?,
            disks: Disks::new_with_refreshed_list()?,
            users: Users::new_with_refreshed_list()?,
            components: Components::new_with_refreshed_list().ok(),
            times: system_times(),
            net: (0, 0),
            net_bytes: HashMap::new(),
            disk_bytes: HashMap::new(),
            at: Instant::now(),
        };
        local.net = local.net_totals();
        local.net_bytes = local.net_by_interface();
        local.disk_bytes = local.disk_totals();
        Ok(local)
    }

    fn net_totals(&self) -> (u64, u64) {
        self.networks
            .iter()
            .filter(|(name, _)| physical(&name.to_string_lossy()))
            .fold((0, 0), |sum, (_, n)| (sum.0 + n.total_received(), sum.1 + n.total_transmitted()))
    }

    fn net_by_interface(&self) -> HashMap<String, (u64, u64)> {
        self.networks.iter().map(|(name, n)| (name.to_string_lossy().into_owned(), (n.total_received(), n.total_transmitted()))).collect()
    }

    fn disk_totals(&self) -> HashMap<String, (u64, u64)> {
        self.disks
            .iter()
            .map(|d| {
                let usage = d.usage();
                (mount(d), (usage.total_read_bytes, usage.total_written_bytes))
            })
            .collect()
    }

    /// Everything since the previous call. Blocks for a moment (process list, nvidia-smi).
    pub fn sample(&mut self, ts: i64) -> Sample {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        self.networks.refresh(true);
        self.disks.refresh(true);
        let dt = self.at.elapsed().as_secs_f64().max(0.001);
        self.at = Instant::now();
        let rate = |now: u64, before: u64| now.saturating_sub(before) as f64 / dt;

        let cores: Vec<f32> = self.sys.cpus().iter().map(|c| c.usage()).collect();
        let mut cpu = CpuUsage { total: self.sys.global_cpu_usage(), ..Default::default() };
        cpu.user = cpu.total;
        let times = system_times();
        if let (Some(now), Some(before)) = (times, self.times) {
            let (idle, kernel, user) = (now.0 - before.0, now.1 - before.1, now.2 - before.2);
            let all = (kernel + user) as f32;
            if all > 0.0 {
                cpu.user = user as f32 / all * 100.0;
                cpu.system = kernel.saturating_sub(idle) as f32 / all * 100.0;
                cpu.total = (cpu.user + cpu.system).clamp(0.0, 100.0);
            }
        }
        self.times = times;

        let net = self.net_totals();
        let (rx_bps, tx_bps) = (rate(net.0, self.net.0), rate(net.1, self.net.1));
        self.net = net;
        let net_bytes = self.net_by_interface();
        let mut interfaces: Vec<NetIface> = net_bytes
            .iter()
            .filter(|(_, now)| now.0 + now.1 > 0)
            .map(|(name, now)| {
                let before = self.net_bytes.get(name);
                NetIface {
                    name: name.clone(),
                    rx_bps: before.map_or(0.0, |b| rate(now.0, b.0)),
                    tx_bps: before.map_or(0.0, |b| rate(now.1, b.1)),
                    rx_total: now.0,
                    tx_total: now.1,
                    physical: physical(name),
                }
            })
            .collect();
        interfaces.sort_by(|a, b| b.physical.cmp(&a.physical).then(a.name.cmp(&b.name)));
        self.net_bytes = net_bytes;

        let disk_bytes = self.disk_totals();
        let mut disks = Vec::new();
        let mut disk_io = Vec::new();
        for d in self.disks.iter() {
            let name = mount(d);
            if d.total_space() == 0 || disks.iter().any(|x: &Disk| x.mount == name) {
                continue;
            }
            if let (Some(now), Some(before)) = (disk_bytes.get(&name), self.disk_bytes.get(&name)) {
                disk_io.push(DiskIo {
                    name: name.clone(),
                    read_bps: rate(now.0, before.0),
                    write_bps: rate(now.1, before.1),
                    iops: 0.0,
                    util: None,
                });
            }
            disks.push(Disk {
                mount: name,
                fstype: d.file_system().to_string_lossy().into_owned(),
                total: d.total_space(),
                used: d.total_space().saturating_sub(d.available_space()),
            });
        }
        self.disk_bytes = disk_bytes;

        let mut sensors = Vec::new();
        if let Some(components) = &mut self.components {
            components.refresh(true);
            for c in components.iter() {
                if let Some(temp) = c.temperature().filter(|t| (1.0..150.0).contains(t)) {
                    sensors.push(Sensor { chip: c.label().to_string(), label: String::new(), temp });
                }
            }
        }
        let gpus = gpus(&mut sensors);

        let mem_total = self.sys.total_memory();
        let mut procs: Vec<Proc> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let cmd = p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
                Proc {
                    pid: pid.as_u32(),
                    user: p.user_id().and_then(|id| self.users.get_user_by_id(id)).map(|u| u.name().to_string()).unwrap_or_default(),
                    state: if p.cpu_usage() > 0.5 { 'R' } else { 'S' },
                    cpu: p.cpu_usage(),
                    mem_pct: p.memory() as f32 / mem_total.max(1) as f32 * 100.0,
                    rss: p.memory(),
                    elapsed: p.run_time(),
                    // The command line of system processes is not readable; the name always is.
                    cmd: if cmd.is_empty() { p.name().to_string_lossy().into_owned() } else { cmd },
                }
            })
            .collect();
        let proc_count = procs.len();
        // Keep the interesting ones: busiest by CPU plus biggest by memory.
        procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss.cmp(&a.rss)));
        let mut rest = procs.split_off(procs.len().min(120));
        rest.sort_by(|a, b| b.rss.cmp(&a.rss));
        procs.extend(rest.into_iter().take(40));

        Sample {
            ts,
            cpu,
            freqs: self.sys.cpus().iter().map(|c| c.frequency() as f32).collect(),
            cores,
            load: [0.0; 3],
            mem_total,
            mem_used: self.sys.used_memory(),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            rx_bps,
            tx_bps,
            net: interfaces,
            disks,
            disk_io,
            temp: parse::main_temp(&sensors),
            sensors,
            gpus,
            uptime: System::uptime().unwrap_or(0),
            procs,
            proc_count,
            zombies: 0,
            info: HostInfo {
                kernel: System::kernel_version().map(|v| format!("build {v}")).unwrap_or_default(),
                os: System::long_os_version().unwrap_or_else(|_| "Windows".into()),
                cpu_model: self.sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
                is_local_windows: true,
                ..Default::default()
            },
        }
    }
}

/// `C:\` -> `C:`
fn mount(d: &sysinfo::Disk) -> String {
    d.mount_point().to_string_lossy().trim_end_matches('\\').to_string()
}

fn gpus(sensors: &mut Vec<Sensor>) -> Vec<Gpu> {
    let query = |what: &str| {
        let out = run_local("nvidia-smi", &[what, "--format=csv,noheader,nounits"], "").unwrap_or_default();
        // An error message is not a list of GPUs.
        if out.contains("NVIDIA-SMI") { String::new() } else { out }
    };
    let list = query("--query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw");
    if list.trim().is_empty() {
        return Vec::new();
    }
    let apps = query("--query-compute-apps=used_memory");
    parse::parse_gpus(&list.lines().collect::<Vec<_>>(), &apps.lines().collect::<Vec<_>>(), &[], sensors)
}

/// Containers of the docker installation on this computer (Docker Desktop).
pub fn docker() -> DockerState {
    const PS: &str = "{{.ID}}|{{.Names}}|{{.Image}}|{{.State}}|{{.Status}}";
    const STATS: &str = "{{.ID}}|{{.CPUPerc}}|{{.MemUsage}}|{{.NetIO}}|{{.BlockIO}}|{{.PIDs}}";
    let Ok(ps) = run_local("docker", &["ps", "-a", "--no-trunc", "--format", PS], "") else {
        return DockerState::NotInstalled;
    };
    // Rows have four separators; anything else is an error message from the CLI.
    let failed = ps.lines().any(|l| !l.trim().is_empty() && l.matches('|').count() < 4);
    let stats = if failed { String::new() } else { run_local("docker", &["stats", "--no-stream", "--no-trunc", "--format", STATS], "").unwrap_or_default() };
    let error = if failed { "@@error\n" } else { "" };
    parse::parse_docker(&format!("@@ps\n{ps}\n{error}@@stats\n{stats}\n@@end\n"))
}
