//! Simulated servers for `--demo` mode (and for the README screenshots).
//!
//! Values are a pure function of the profile and the timestamp, so history and
//! live data line up without keeping any state.

use crate::model::*;
use std::f64::consts::TAU;

struct Profile {
    name: &'static str,
    host: &'static str,
    cores: usize,
    mem_gb: u64,
    cpu_base: f64,
    cpu_swing: f64,
    iowait: f64,
    mem_pct: f64,
    temp: f32,
    zombies: usize,
    disks: &'static [(&'static str, &'static str, u64, f64)],
    procs: &'static [(&'static str, &'static str, f64, f64)],
    /// name, image, state, status, cpu share, memory MiB, limit MiB (0 = none)
    containers: &'static [(&'static str, &'static str, &'static str, &'static str, f64, u64, u64)],
}

const PROFILES: [Profile; 3] = [
    Profile {
        name: "web-01",
        host: "web-01.example.net",
        cores: 8,
        mem_gb: 16,
        cpu_base: 24.0,
        cpu_swing: 14.0,
        iowait: 1.0,
        mem_pct: 46.0,
        temp: 52.0,
        zombies: 0,
        disks: &[("/", "ext4", 80, 52.0), ("/var/lib/docker", "xfs", 200, 61.0)],
        procs: &[
            ("www-data", "nginx: worker process", 0.22, 0.6),
            ("node", "node /srv/api/dist/server.js", 0.34, 4.1),
            ("postgres", "postgres: api app 10.0.0.12(51234) idle", 0.18, 6.5),
            ("redis", "redis-server *:6379", 0.06, 1.2),
            ("root", "/usr/bin/dockerd -H fd://", 0.05, 1.0),
            ("root", "/usr/bin/containerd", 0.03, 0.6),
            ("prometheus", "/usr/local/bin/node_exporter", 0.02, 0.2),
            ("root", "sshd: deploy [priv]", 0.01, 0.1),
            ("root", "/lib/systemd/systemd-journald", 0.01, 0.3),
        ],
        containers: &[
            ("proxy", "nginx:1.27-alpine", "running", "Up 3 weeks (healthy)", 0.20, 96, 256),
            ("api", "registry.example.net/shop/api:4.18.2", "running", "Up 6 days (healthy)", 0.36, 742, 1024),
            ("postgres", "postgres:17", "running", "Up 3 weeks (healthy)", 0.20, 2210, 4096),
            ("redis", "redis:7-alpine", "running", "Up 3 weeks", 0.07, 188, 512),
            ("worker", "registry.example.net/shop/worker:4.18.2", "running", "Up 6 days", 0.12, 410, 1024),
            ("migrate", "registry.example.net/shop/api:4.18.2", "exited", "Exited (0) 6 days ago", 0.0, 0, 0),
        ],
    },
    Profile {
        name: "db-01",
        host: "db-01.example.net",
        cores: 16,
        mem_gb: 64,
        cpu_base: 38.0,
        cpu_swing: 10.0,
        iowait: 19.0,
        mem_pct: 92.5,
        temp: 61.0,
        zombies: 0,
        disks: &[("/", "ext4", 100, 38.0), ("/var/lib/postgresql", "xfs", 1800, 88.0)],
        procs: &[
            ("postgres", "postgres: checkpointer", 0.12, 20.5),
            ("postgres", "postgres: analytics reports 10.0.0.31(40112) SELECT", 0.41, 28.0),
            ("postgres", "postgres: walwriter", 0.08, 3.2),
            ("postgres", "postgres: autovacuum worker orders", 0.21, 9.1),
            ("root", "/usr/sbin/pgbouncer /etc/pgbouncer/pgbouncer.ini", 0.05, 0.4),
            ("backup", "pg_basebackup -D /backup/base -Ft -z", 0.09, 1.3),
            ("root", "/lib/systemd/systemd-journald", 0.01, 0.2),
        ],
        containers: &[],
    },
    Profile {
        name: "build-runner",
        host: "ci-runner-3.example.net",
        cores: 4,
        mem_gb: 8,
        cpu_base: 91.0,
        cpu_swing: 6.0,
        iowait: 3.0,
        mem_pct: 71.0,
        temp: 84.0,
        zombies: 3,
        disks: &[("/", "ext4", 120, 96.0)],
        procs: &[
            ("runner", "rustc --crate-name app --edition=2024 src/main.rs -C opt-level=3", 0.38, 14.0),
            ("runner", "cc1plus -quiet -O2 kernel/sched/core.c", 0.27, 6.0),
            ("runner", "node /opt/ci/node_modules/.bin/webpack --mode production", 0.21, 11.5),
            ("root", "/usr/bin/dockerd -H fd://", 0.07, 2.0),
            ("runner", "/opt/ci/agent run --once", 0.04, 1.1),
            ("runner", "[sh] <defunct>", 0.0, 0.0),
        ],
        containers: &[
            ("ci-agent", "registry.example.net/ci/agent:2.9", "restarting", "Restarting (1) 4 seconds ago", 0.0, 0, 0),
            ("build-7412", "rust:1.98", "running", "Up 11 minutes", 0.55, 2890, 3072),
            ("build-7409", "node:24", "exited", "Exited (137) 40 minutes ago", 0.0, 0, 0),
            ("registry-cache", "registry:3", "running", "Up 2 days", 0.04, 120, 0),
        ],
    },
];

pub fn servers() -> Vec<ServerConfig> {
    PROFILES
        .iter()
        .enumerate()
        .map(|(i, p)| ServerConfig {
            id: format!("demo-{i}"),
            name: p.name.to_string(),
            host: p.host.to_string(),
            port: 22,
            user: "monitor".to_string(),
            auth: AuthMethod::Auto,
        })
        .collect()
}

fn wave(t: f64, period: f64, phase: f64) -> f64 {
    ((t / period + phase) * TAU).sin()
}

/// Deterministic noise in -1..1.
fn noise(seed: u64, ts: i64) -> f64 {
    let mut x = (ts as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    x ^= x >> 32;
    x = x.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    x ^= x >> 32;
    (x % 20_001) as f64 / 10_000.0 - 1.0
}

pub fn generate(profile: usize, ts: i64) -> (Sample, DockerState) {
    let p = &PROFILES[profile];
    let seed = profile as u64 * 100;
    let t = ts as f64;
    // Slow drift, a faster ripple and a little jitter.
    let activity = 0.6 * wave(t, 1500.0, profile as f64 * 0.3) + 0.3 * wave(t, 340.0, 0.1) + 0.1 * noise(seed, ts);
    let total = (p.cpu_base + p.cpu_swing * activity).clamp(1.0, 99.5);
    let iowait = (p.iowait * (1.0 + 0.4 * wave(t, 420.0, 0.5)) + 0.5 * noise(seed + 1, ts)).max(0.0);
    let system = total * 0.22;

    let cores: Vec<f32> = (0..p.cores)
        .map(|i| {
            let offset = 18.0 * wave(t, 200.0 + 37.0 * i as f64, i as f64 * 0.37) + 6.0 * noise(seed + 10 + i as u64, ts);
            (total + offset).clamp(0.5, 100.0) as f32
        })
        .collect();
    let mem_total = p.mem_gb << 30;
    let mem_pct = p.mem_pct + 1.5 * wave(t, 2400.0, 0.2) + 0.2 * noise(seed + 2, ts);
    let load1 = total / 100.0 * p.cores as f64 * (1.15 + iowait / 40.0);
    let traffic = (1.0 + 0.5 * activity).max(0.1);

    let procs: Vec<Proc> = p
        .procs
        .iter()
        .enumerate()
        .map(|(i, (user, cmd, share, mem))| {
            let cpu = share * total * p.cores as f64 * (1.0 + 0.15 * noise(seed + 50 + i as u64, ts));
            Proc {
                pid: 800 + i as u32 * 137 + profile as u32 * 1000,
                user: user.to_string(),
                state: if cmd.contains("defunct") { 'Z' } else if cpu > 5.0 { 'R' } else { 'S' },
                cpu: cpu.max(0.0) as f32,
                mem_pct: *mem as f32,
                rss: (mem_total as f64 * mem / 100.0) as u64,
                elapsed: 3600 * (5 + i as u64 * 31) + (ts % 3600) as u64,
                cmd: cmd.to_string(),
            }
        })
        .collect();

    let sample = Sample {
        ts,
        cpu: CpuUsage {
            total: total as f32,
            user: (total - system) as f32,
            system: system as f32,
            iowait: iowait as f32,
            steal: 0.0,
        },
        freqs: cores.iter().map(|c| 1800.0 + c * 22.0).collect(),
        cores,
        load: [
            load1 as f32,
            (load1 * (1.0 - 0.1 * wave(t, 900.0, 0.0))) as f32,
            (p.cpu_base / 100.0 * p.cores as f64 * 1.15) as f32,
        ],
        mem_total,
        mem_used: (mem_total as f64 * mem_pct / 100.0) as u64,
        swap_total: 4 << 30,
        swap_used: if p.mem_pct > 90.0 { 2600 << 20 } else { 64 << 20 },
        rx_bps: 2.4e6 * traffic * (1.0 + 0.2 * noise(seed + 3, ts)),
        tx_bps: 6.1e6 * traffic * (1.0 + 0.2 * noise(seed + 4, ts)),
        disks: p
            .disks
            .iter()
            .map(|(mount, fstype, gb, pct)| Disk {
                mount: mount.to_string(),
                fstype: fstype.to_string(),
                total: gb << 30,
                used: ((gb << 30) as f64 * pct / 100.0) as u64,
            })
            .collect(),
        temp: Some(p.temp + (total as f32 - p.cpu_base as f32) * 0.25),
        uptime: 40 * 86400 + (ts % 86400) as u64,
        proc_count: 140 + profile * 35,
        zombies: p.zombies,
        procs,
        info: HostInfo {
            kernel: "Linux 6.8.0-106-generic".into(),
            os: "Ubuntu 24.04.3 LTS".into(),
            cpu_model: "AMD EPYC 7543P 32-Core Processor".into(),
            reboot_required: profile == 2,
        },
    };

    let docker = if p.containers.is_empty() {
        DockerState::NotInstalled
    } else {
        DockerState::Ok(
            p.containers
                .iter()
                .enumerate()
                .map(|(i, (name, image, state, status, share, mem, limit))| {
                    let running = *state == "running";
                    let wobble = 1.0 + 0.2 * wave(t, 260.0 + 45.0 * i as f64, i as f64 * 0.21) + 0.08 * noise(seed + 70 + i as u64, ts);
                    Container {
                        id: format!("{:012x}", (i as u64 + 1) * 0x9E37_79B9 + seed),
                        name: name.to_string(),
                        image: image.to_string(),
                        state: state.to_string(),
                        status: status.to_string(),
                        has_stats: running,
                        cpu: (share * total * p.cores as f64 * wobble).max(0.0) as f32,
                        mem_used: ((mem << 20) as f64 * (1.0 + 0.04 * wave(t, 1300.0, i as f64 * 0.4))) as u64,
                        mem_limit: if *limit == 0 { mem_total } else { limit << 20 },
                        net: if running { format!("{:.1}GB / {:.1}GB", 1.2 + i as f64, 3.4 + i as f64 * 2.0) } else { String::new() },
                        block: if running { format!("{}MB / {}MB", 120 * (i + 1), 48 * (i + 1)) } else { String::new() },
                        pids: if running { 4 + i as u32 * 7 } else { 0 },
                    }
                })
                .collect(),
        )
    };
    (sample, docker)
}
