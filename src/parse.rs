//! Remote shell scripts and parsers for their output.
//!
//! Everything is read from `/proc`, `df`, `ps` and the docker CLI, so the
//! monitored machine needs no agent installed. The scripts contain no single
//! quotes because they are wrapped in `sh -c '...'` (the login shell of the
//! remote user may not be POSIX compatible).

use crate::model::*;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;

pub const SYSTEM_SCRIPT: &str = r#"
echo @@stat; cat /proc/stat;
echo @@loadavg; cat /proc/loadavg;
echo @@meminfo; cat /proc/meminfo;
echo @@uptime; cat /proc/uptime;
echo @@net; cat /proc/net/dev;
echo @@df; df -PT -k 2>/dev/null;
echo @@freq; grep -i "cpu MHz" /proc/cpuinfo;
echo @@temp; cat /sys/class/thermal/thermal_zone*/temp 2>/dev/null;
echo @@pstat; cat /proc/[0-9]*/stat 2>/dev/null;
echo @@ps; ps -eo pid,user:16,rss,pmem,etimes,args 2>/dev/null | cut -c1-220;
echo @@info;
echo "kernel=$(uname -sr)";
echo "tck=$(getconf CLK_TCK 2>/dev/null)";
echo "model=$(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2)";
echo "os=$(. /etc/os-release 2>/dev/null; echo $PRETTY_NAME)";
test -f /var/run/reboot-required && echo reboot=1;
echo @@end
"#;

pub const DOCKER_SCRIPT: &str = r#"
command -v docker >/dev/null 2>&1 || { echo @@nodocker; exit 0; };
echo @@ps;
docker ps -a --no-trunc --format "{{.ID}}|{{.Names}}|{{.Image}}|{{.State}}|{{.Status}}" 2>&1 || echo @@error;
echo @@stats;
docker stats --no-stream --no-trunc --format "{{.ID}}|{{.CPUPerc}}|{{.MemUsage}}|{{.NetIO}}|{{.BlockIO}}|{{.PIDs}}" 2>/dev/null;
echo @@end
"#;

/// Wrap a script so it always runs under a POSIX shell.
pub fn wrap(script: &str) -> String {
    debug_assert!(!script.contains('\''));
    format!("sh -c '{}'", script.replace('\n', " "))
}

fn sections(text: &str) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current = "";
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("@@") {
            current = name.trim();
            map.entry(current).or_default();
        } else if !current.is_empty() {
            map.entry(current).or_default().push(line);
        }
    }
    map
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CpuTimes {
    user: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    steal: u64,
}

impl CpuTimes {
    fn parse(line: &str) -> Option<Self> {
        let v: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .map(|x| x.parse().unwrap_or(0))
            .collect();
        if v.len() < 4 {
            return None;
        }
        let g = |i: usize| v.get(i).copied().unwrap_or(0);
        Some(Self {
            user: g(0) + g(1),
            system: g(2) + g(5) + g(6),
            idle: g(3),
            iowait: g(4),
            steal: g(7),
        })
    }

    fn total(&self) -> u64 {
        self.user + self.system + self.idle + self.iowait + self.steal
    }

    pub fn usage_since(&self, prev: &CpuTimes) -> CpuUsage {
        let total = self.total().saturating_sub(prev.total());
        if total == 0 {
            return CpuUsage::default();
        }
        let pct = |a: u64, b: u64| a.saturating_sub(b) as f32 / total as f32 * 100.0;
        let idle = pct(self.idle, prev.idle);
        let iowait = pct(self.iowait, prev.iowait);
        CpuUsage {
            total: (100.0 - idle - iowait).clamp(0.0, 100.0),
            user: pct(self.user, prev.user),
            system: pct(self.system, prev.system),
            iowait,
            steal: pct(self.steal, prev.steal),
        }
    }
}

#[derive(Clone, Debug)]
struct PsRow {
    pid: u32,
    user: String,
    rss: u64,
    mem_pct: f32,
    elapsed: u64,
    cmd: String,
}

/// Raw counters of one poll. Rates are derived from two consecutive polls.
#[derive(Clone, Debug, Default)]
pub struct RawSystem {
    cpu: CpuTimes,
    cores: Vec<CpuTimes>,
    load: [f32; 3],
    mem_total: u64,
    mem_used: u64,
    swap_total: u64,
    swap_used: u64,
    uptime: u64,
    net_rx: u64,
    net_tx: u64,
    disks: Vec<Disk>,
    freqs: Vec<f32>,
    temp: Option<f32>,
    /// pid -> (cpu ticks, state)
    proc_ticks: HashMap<u32, (u64, char)>,
    ps: Vec<PsRow>,
    info: HostInfo,
    tck: f64,
}

pub fn parse_system(text: &str) -> Result<RawSystem> {
    let sec = sections(text);
    if !sec.contains_key("end") {
        bail!("incomplete output from remote host (is it a Linux machine?)");
    }
    let get = |name: &str| sec.get(name).map(Vec::as_slice).unwrap_or(&[]);
    let mut raw = RawSystem {
        tck: 100.0,
        ..Default::default()
    };

    for line in get("stat") {
        if line.starts_with("cpu ") {
            raw.cpu = CpuTimes::parse(line).context("bad /proc/stat")?;
        } else if line.starts_with("cpu") {
            raw.cores.extend(CpuTimes::parse(line));
        }
    }
    if raw.cores.is_empty() {
        bail!("no CPU data in /proc/stat");
    }

    if let Some(line) = get("loadavg").first() {
        for (i, v) in line.split_whitespace().take(3).enumerate() {
            raw.load[i] = v.parse().unwrap_or(0.0);
        }
    }

    let mut mem: HashMap<&str, u64> = HashMap::new();
    for line in get("meminfo") {
        if let Some((key, rest)) = line.split_once(':') {
            let kb = rest.split_whitespace().next().and_then(|v| v.parse().ok());
            mem.insert(key, kb.unwrap_or(0) * 1024);
        }
    }
    let m = |k: &str| mem.get(k).copied().unwrap_or(0);
    raw.mem_total = m("MemTotal");
    let available = if mem.contains_key("MemAvailable") {
        m("MemAvailable")
    } else {
        m("MemFree") + m("Buffers") + m("Cached")
    };
    raw.mem_used = raw.mem_total.saturating_sub(available);
    raw.swap_total = m("SwapTotal");
    raw.swap_used = raw.swap_total.saturating_sub(m("SwapFree"));

    raw.uptime = get("uptime")
        .first()
        .and_then(|l| l.split_whitespace().next())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;

    for line in get("net") {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let iface = iface.trim();
        // Skip loopback and container plumbing so traffic is not counted twice.
        if iface == "lo" || ["veth", "docker", "br-", "virbr"].iter().any(|p| iface.starts_with(p)) {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().map(|x| x.parse().unwrap_or(0)).collect();
        if f.len() >= 9 {
            raw.net_rx += f[0];
            raw.net_tx += f[8];
        }
    }

    const PSEUDO_FS: [&str; 8] = [
        "tmpfs", "devtmpfs", "overlay", "squashfs", "efivarfs", "proc", "sysfs", "nsfs",
    ];
    for line in get("df").iter().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 7 || PSEUDO_FS.contains(&f[1]) {
            continue;
        }
        let total = f[2].parse::<u64>().unwrap_or(0) * 1024;
        let mount = f[6..].join(" ");
        if total == 0 || raw.disks.iter().any(|d| d.mount == mount) {
            continue;
        }
        raw.disks.push(Disk {
            mount,
            fstype: f[1].to_string(),
            total,
            used: f[3].parse::<u64>().unwrap_or(0) * 1024,
        });
    }

    raw.freqs = get("freq")
        .iter()
        .filter_map(|l| l.split_once(':')?.1.trim().parse().ok())
        .collect();

    raw.temp = get("temp")
        .iter()
        .filter_map(|l| l.trim().parse::<f32>().ok())
        .map(|v| v / 1000.0)
        .filter(|v| (1.0..150.0).contains(v))
        .reduce(f32::max);

    for line in get("pstat") {
        // "pid (comm) state ppid ... utime stime ..." - comm may contain spaces.
        let (Some(open), Some(close)) = (line.find('('), line.rfind(')')) else {
            continue;
        };
        let Ok(pid) = line[..open].trim().parse::<u32>() else {
            continue;
        };
        let f: Vec<&str> = line[close + 1..].split_whitespace().collect();
        if f.len() > 12 {
            let ticks = f[11].parse::<u64>().unwrap_or(0) + f[12].parse::<u64>().unwrap_or(0);
            raw.proc_ticks
                .insert(pid, (ticks, f[0].chars().next().unwrap_or('?')));
        }
    }

    for line in get("ps") {
        let mut it = line.split_whitespace();
        let Some(Ok(pid)) = it.next().map(str::parse::<u32>) else {
            continue; // header line
        };
        let user = it.next().unwrap_or("").to_string();
        let rss = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
        let mem_pct = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let elapsed = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let cmd = it.collect::<Vec<_>>().join(" ");
        raw.ps.push(PsRow {
            pid,
            user,
            rss,
            mem_pct,
            elapsed,
            cmd,
        });
    }

    for line in get("info") {
        match line.split_once('=') {
            Some(("kernel", v)) => raw.info.kernel = v.trim().to_string(),
            Some(("model", v)) => raw.info.cpu_model = v.trim().to_string(),
            Some(("os", v)) => raw.info.os = v.trim().to_string(),
            Some(("tck", v)) => raw.tck = v.trim().parse().ok().filter(|t| *t > 0.0).unwrap_or(100.0),
            Some(("reboot", _)) => raw.info.reboot_required = true,
            _ => {}
        }
    }
    Ok(raw)
}

/// Turn two consecutive polls, `dt` seconds apart, into a sample.
pub fn build_sample(prev: &RawSystem, cur: &RawSystem, dt: f64, ts: i64) -> Sample {
    let dt = dt.max(0.001);
    let rate = |now: u64, before: u64| now.saturating_sub(before) as f64 / dt;

    let mut procs: Vec<Proc> = cur
        .ps
        .iter()
        .map(|row| {
            let (ticks, state) = cur.proc_ticks.get(&row.pid).copied().unwrap_or((0, '?'));
            let cpu = match prev.proc_ticks.get(&row.pid) {
                Some((before, _)) => (ticks.saturating_sub(*before) as f64 / cur.tck / dt * 100.0) as f32,
                None => 0.0,
            };
            Proc {
                pid: row.pid,
                user: row.user.clone(),
                state,
                cpu,
                mem_pct: row.mem_pct,
                rss: row.rss,
                elapsed: row.elapsed,
                cmd: row.cmd.clone(),
            }
        })
        .collect();
    let proc_count = procs.len();
    let zombies = procs.iter().filter(|p| p.state == 'Z').count();
    // Keep the interesting ones: busiest by CPU plus biggest by memory.
    procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss.cmp(&a.rss)));
    let mut rest = procs.split_off(procs.len().min(120));
    rest.sort_by(|a, b| b.rss.cmp(&a.rss));
    procs.extend(rest.into_iter().take(40));

    Sample {
        ts,
        cpu: cur.cpu.usage_since(&prev.cpu),
        cores: cur
            .cores
            .iter()
            .zip(&prev.cores)
            .map(|(c, p)| c.usage_since(p).total)
            .collect(),
        freqs: cur.freqs.clone(),
        load: cur.load,
        mem_total: cur.mem_total,
        mem_used: cur.mem_used,
        swap_total: cur.swap_total,
        swap_used: cur.swap_used,
        rx_bps: rate(cur.net_rx, prev.net_rx),
        tx_bps: rate(cur.net_tx, prev.net_tx),
        disks: cur.disks.clone(),
        temp: cur.temp,
        uptime: cur.uptime,
        procs,
        proc_count,
        zombies,
        info: cur.info.clone(),
    }
}

/// Parse sizes as printed by docker: `12.5MiB`, `1.2GB`, `648kB`, `0B`.
pub fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let value: f64 = s[..split].parse().unwrap_or(0.0);
    let mult = match s[split..].trim() {
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    (value * mult) as u64
}

pub fn parse_docker(text: &str) -> DockerState {
    let sec = sections(text);
    if sec.contains_key("nodocker") {
        return DockerState::NotInstalled;
    }
    let Some(ps) = sec.get("ps") else {
        return DockerState::Error("no output from docker".into());
    };
    if sec.contains_key("error") {
        let msg = sec["error"].first().or(ps.first()).copied().unwrap_or("docker ps failed");
        return DockerState::Error(msg.trim().to_string());
    }
    let mut list: Vec<Container> = ps
        .iter()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('|').collect();
            (f.len() >= 5).then(|| Container {
                id: f[0].to_string(),
                name: f[1].to_string(),
                image: f[2].to_string(),
                state: f[3].to_string(),
                status: f[4..].join("|"),
                ..Default::default()
            })
        })
        .collect();
    for line in sec.get("stats").map(Vec::as_slice).unwrap_or(&[]) {
        let f: Vec<&str> = line.split('|').collect();
        if f.len() < 6 {
            continue;
        }
        if let Some(c) = list.iter_mut().find(|c| c.id == f[0]) {
            let (used, limit) = f[2].split_once('/').unwrap_or((f[2], "0"));
            c.has_stats = true;
            c.cpu = f[1].trim_end_matches('%').parse().unwrap_or(0.0);
            c.mem_used = parse_size(used);
            c.mem_limit = parse_size(limit);
            c.net = f[3].to_string();
            c.block = f[4].to_string();
            c.pids = f[5].trim().parse().unwrap_or(0);
        }
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    DockerState::Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system_output(cpu: &str, cpu0: &str, rx: u64, ticks: u64) -> String {
        format!(
            "@@stat\n{cpu}\n{cpu0}\nintr 1 2 3\n@@loadavg\n0.52 0.40 0.30 1/200 999\n\
             @@meminfo\nMemTotal:       8000000 kB\nMemFree:        1000000 kB\nMemAvailable:   6000000 kB\n\
             SwapTotal:      2000000 kB\nSwapFree:       1500000 kB\n@@uptime\n86400.55 1000.0\n\
             @@net\nInter-|   Receive\n face |bytes\n    lo: 500 1 0 0 0 0 0 0 500 1 0 0 0 0 0 0\n\
             \x20 eth0: {rx} 10 0 0 0 0 0 0 2000 10 0 0 0 0 0 0\n veth1a: 9999 1 0 0 0 0 0 0 9999 1 0 0 0 0 0 0\n\
             @@df\nFilesystem Type 1024-blocks Used Available Capacity Mounted on\n\
             /dev/sda1 ext4 1000000 900000 100000 90% /\ntmpfs tmpfs 5000 0 5000 0% /run\n\
             /dev/sdb1 xfs 2000000 500000 1500000 25% /mnt/my data\n\
             @@freq\ncpu MHz\t\t: 3400.000\n@@temp\n45000\n61500\n\
             @@pstat\n42 (my (odd) proc) R 1 42 42 0 -1 4194304 100 0 0 0 {ticks} 50 0 0 20 0 1 0 100 1000 200\n\
             7 (defunct) Z 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 100 0 0\n\
             @@ps\n    PID USER               RSS %MEM ELAPSED COMMAND\n     42 www-data        204800  2.5    3600 /usr/bin/app --serve all\n\
             \x20     7 root                 0  0.0      10 [defunct]\n\
             @@info\nkernel=Linux 6.8.0\ntck=100\nmodel= Test CPU\nos=Ubuntu 24.04\nreboot=1\n@@end\n"
        )
    }

    #[test]
    fn parses_and_derives_rates() {
        let a = parse_system(&system_output("cpu  100 0 100 800 0 0 0 0", "cpu0 100 0 100 800 0 0 0 0", 1000, 100)).unwrap();
        let b = parse_system(&system_output("cpu  150 0 120 1020 10 0 0 0", "cpu0 150 0 120 1020 10 0 0 0", 6000, 350)).unwrap();
        let s = build_sample(&a, &b, 5.0, 1);

        // delta: user 50, system 20, idle 220, iowait 10 => total 300
        assert!((s.cpu.user - 16.67).abs() < 0.1);
        assert!((s.cpu.iowait - 3.33).abs() < 0.1);
        assert!((s.cpu.total - 23.33).abs() < 0.1);
        assert_eq!(s.cores.len(), 1);
        assert_eq!(s.load, [0.52, 0.40, 0.30]);
        assert_eq!(s.mem_total, 8_000_000 * 1024);
        assert_eq!(s.mem_used, 2_000_000 * 1024);
        assert_eq!(s.swap_used, 500_000 * 1024);
        assert_eq!(s.uptime, 86400);
        assert_eq!(s.rx_bps, 1000.0); // lo and veth are ignored
        assert_eq!(s.temp, Some(61.5));
        assert_eq!(s.freqs, vec![3400.0]);
        assert!(s.info.reboot_required);
        assert_eq!(s.info.cpu_model, "Test CPU");

        assert_eq!(s.disks.len(), 2);
        assert_eq!(s.disks[1].mount, "/mnt/my data");
        assert_eq!(s.disks[0].pct().round(), 90.0);

        assert_eq!(s.proc_count, 2);
        assert_eq!(s.zombies, 1);
        let p = &s.procs[0];
        // 250 ticks at 100 Hz over 5 s = half a core
        assert_eq!((p.pid, p.cpu.round(), p.state), (42, 50.0, 'R'));
        assert_eq!(p.cmd, "/usr/bin/app --serve all");
        assert_eq!(p.user, "www-data");
    }

    #[test]
    fn rejects_truncated_output() {
        assert!(parse_system("@@stat\ncpu  1 2 3 4\n").is_err());
    }

    #[test]
    fn parses_docker() {
        let text = "@@ps\nabc|web|nginx:1.27|running|Up 3 days (healthy)\n\
                    def|job|alpine|exited|Exited (1) 2 hours ago\n@@stats\n\
                    abc|12.50%|256MiB / 1GiB|1.2MB / 3.4MB|0B / 12kB|9\n@@end\n";
        let DockerState::Ok(list) = parse_docker(text) else {
            panic!("expected containers");
        };
        assert_eq!(list.len(), 2);
        let web = list.iter().find(|c| c.name == "web").unwrap();
        assert!(web.has_stats);
        assert_eq!(web.cpu, 12.5);
        assert_eq!(web.mem_used, 256 * 1024 * 1024);
        assert_eq!(web.mem_limit, 1024 * 1024 * 1024);
        assert_eq!(web.pids, 9);
        assert!(!list.iter().find(|c| c.name == "job").unwrap().has_stats);

        assert!(matches!(parse_docker("@@nodocker\n"), DockerState::NotInstalled));
        assert!(matches!(
            parse_docker("@@ps\npermission denied while trying to connect\n@@error\n@@stats\n@@end\n"),
            DockerState::Error(m) if m.contains("permission denied")
        ));
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size("648kB"), 648_000);
        assert_eq!(parse_size("1.5GiB "), 1_610_612_736);
    }

    #[test]
    fn scripts_are_safe_to_wrap() {
        assert!(!SYSTEM_SCRIPT.contains('\''));
        assert!(!DOCKER_SCRIPT.contains('\''));
    }
}
