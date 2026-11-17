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
echo @@diskstats; cat /proc/diskstats 2>/dev/null;
echo @@blocks; ls /sys/block 2>/dev/null;
echo @@hwmon; for d in /sys/class/hwmon/hwmon*; do n=$(cat $d/name 2>/dev/null); for f in $d/temp*_input; do test -r $f && echo "$d|$n|$(cat ${f%_input}_label 2>/dev/null)|$(cat $f 2>/dev/null)"; done; done;
echo @@zones; for z in /sys/class/thermal/thermal_zone*; do test -r $z/temp && echo "$(cat $z/type 2>/dev/null)|$(cat $z/temp 2>/dev/null)"; done;
echo @@nvidia; command -v nvidia-smi >/dev/null 2>&1 && { nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw --format=csv,noheader,nounits 2>/dev/null; echo @@nvapps; nvidia-smi --query-compute-apps=used_memory --format=csv,noheader,nounits 2>/dev/null; };
echo @@drm; for c in /sys/class/drm/card[0-9]*; do test -r $c/device/gpu_busy_percent && echo "$c|$(cat $c/device/gpu_busy_percent 2>/dev/null)|$(cat $c/device/mem_info_vram_used 2>/dev/null)|$(cat $c/device/mem_info_vram_total 2>/dev/null)"; done;
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

/// macOS has no `/proc`. Per-core counters come from the Mach kernel through the
/// system Python (only touched when the developer tools are installed, the bare
/// stub would open an install dialog); without Python `top` gives the total.
/// Temperatures are only available through `macmon` (Homebrew), which samples for
/// a second and therefore runs in the background while the rest is collected.
pub const MAC_SCRIPT: &str = r#"
export LC_ALL=C;
PATH=$PATH:/usr/local/bin:/opt/homebrew/bin;
echo @@darwin;
T=$(mktemp);
(command -v macmon >/dev/null 2>&1 && macmon pipe -s 1 -i 100 >$T 2>/dev/null) &
PY=; for p in /usr/local/bin/python3 /opt/homebrew/bin/python3; do test -x $p && PY=$p; done;
test -z "$PY" && xcode-select -p >/dev/null 2>&1 && PY=/usr/bin/python3;
C=; test -n "$PY" && C=$($PY -c "import ctypes as c; l=c.CDLL(None); n=c.c_uint(); p=c.POINTER(c.c_int)(); m=c.c_uint(); l.mach_host_self.restype=c.c_uint; l.host_processor_info(l.mach_host_self(),2,c.byref(n),c.byref(p),c.byref(m)); [print(i,p[i*4],p[i*4+3],p[i*4+1],p[i*4+2]) for i in range(n.value)]" 2>/dev/null);
if test -n "$C"; then echo @@cores; echo "$C"; else echo @@top; top -l 2 -s 1 -n 0 2>/dev/null | grep "CPU usage"; fi;
echo @@sysctl;
echo "ncpu=$(sysctl -n hw.ncpu)";
echo "memsize=$(sysctl -n hw.memsize)";
echo "pagesize=$(sysctl -n hw.pagesize)";
echo "boottime=$(sysctl -n kern.boottime)";
echo "swap=$(sysctl -n vm.swapusage)";
echo "load=$(sysctl -n vm.loadavg)";
echo "now=$(date +%s)";
echo @@vmstat; vm_stat;
echo @@net; netstat -ibn;
echo @@df; df -PkY 2>/dev/null || df -Pk;
echo @@ioreg; ioreg -c IOBlockStorageDriver -r -w0 2>/dev/null | grep Statistics;
echo @@ps; ps -axo pid,user,rss,pmem,etime,state,time,args | cut -c1-240;
echo @@info;
echo "kernel=$(uname -sr)";
echo "model=$(sysctl -n machdep.cpu.brand_string)";
echo "os=macOS $(sw_vers -productVersion)";
echo @@macmon; wait; cat $T; rm -f $T;
echo @@end
"#;

/// The script for a system poll: picks the part that fits the OS it runs on.
pub fn system_script() -> String {
    // Trimmed: a line that starts with ";" is a syntax error when the script is piped to a shell.
    let (mac, linux) = (MAC_SCRIPT.trim(), SYSTEM_SCRIPT.trim());
    format!("if test \"$(uname -s)\" = Darwin; then {mac}; else {linux}; fi")
}

pub const DOCKER_SCRIPT: &str = r#"
PATH=$PATH:/usr/local/bin:/opt/homebrew/bin;
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

/// Cumulative transfer counters of one physical disk.
#[derive(Clone, Debug, Default)]
struct DiskCounter {
    name: String,
    read_bytes: u64,
    write_bytes: u64,
    ops: u64,
    /// Milliseconds the disk had work queued; Linux only.
    busy_ms: Option<u64>,
}

/// Devices in `/sys/block` that are not disks, or that sit on top of disks already counted.
const VIRTUAL_DISKS: [&str; 8] = ["loop", "ram", "zram", "sr", "fd", "dm-", "md", "nbd"];

/// Build the sensor list from `dir|chip|label|millidegrees` (hwmon) and `type|millidegrees` (thermal zones).
fn parse_sensors(hwmon: &[&str], zones: &[&str]) -> Vec<Sensor> {
    let reading = |v: &str| v.trim().parse::<f32>().ok().map(|v| v / 1000.0).filter(|v| (1.0..150.0).contains(v));
    let mut out = Vec::new();
    // Two chips of the same driver (two NVMe disks) get a number.
    let mut dirs: Vec<(&str, &str)> = Vec::new();
    for line in hwmon {
        let f: Vec<&str> = line.split('|').collect();
        if f.len() != 4 || f[1].is_empty() {
            continue;
        }
        if !dirs.contains(&(f[0], f[1])) {
            dirs.push((f[0], f[1]));
        }
        let nth = dirs.iter().filter(|d| d.1 == f[1]).position(|d| d.0 == f[0]).unwrap_or(0);
        if let Some(temp) = reading(f[3]) {
            out.push(Sensor {
                chip: if nth == 0 { f[1].to_string() } else { format!("{} #{}", f[1], nth + 1) },
                label: f[2].trim().to_string(),
                temp,
            });
        }
    }
    // Thermal zones usually repeat what hwmon already reports.
    for (i, line) in zones.iter().enumerate() {
        let Some((kind, value)) = line.split_once('|') else { continue };
        if kind.is_empty() || dirs.iter().any(|d| d.1 == kind) {
            continue;
        }
        if let Some(temp) = reading(value) {
            out.push(Sensor {
                chip: kind.to_string(),
                label: format!("zone {i}"),
                temp,
            });
        }
    }
    out
}

/// GPUs from `nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw`
/// (csv, no units), the memory of its compute processes in MiB, and `card|busy percent|vram used|vram total` of amdgpu.
/// Each GPU with a temperature also becomes a sensor.
pub fn parse_gpus(nvidia: &[&str], apps: &[&str], drm: &[&str], sensors: &mut Vec<Sensor>) -> Vec<Gpu> {
    const MIB: f64 = 1024.0 * 1024.0;
    let mut out = Vec::new();
    for line in nvidia {
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        if f.len() < 7 {
            continue;
        }
        // Fields a GPU does not support read "[N/A]".
        let n = |i: usize| f[i].parse::<f64>().ok();
        out.push(Gpu {
            name: f[1].to_string(),
            util: n(2).unwrap_or(0.0) as f32,
            mem_used: (n(3).unwrap_or(0.0) * MIB) as u64,
            mem_total: (n(4).unwrap_or(0.0) * MIB) as u64,
            temp: n(5).map(|t| t as f32),
            power: n(6).map(|p| p as f32),
        });
    }
    // A GPU that shares the system memory reports no usage of its own; its processes do.
    if let [gpu] = out.as_mut_slice() {
        if gpu.mem_total == 0 {
            gpu.mem_used = (apps.iter().filter_map(|l| l.trim().parse::<f64>().ok()).sum::<f64>() * MIB) as u64;
        }
    }
    for line in drm {
        let f: Vec<&str> = line.split('|').collect();
        if f.len() == 4 {
            let card = f[0].rsplit('/').next().unwrap_or(f[0]);
            out.push(Gpu {
                name: format!("AMD GPU ({card})"),
                util: f[1].trim().parse().unwrap_or(0.0),
                mem_used: f[2].trim().parse().unwrap_or(0),
                mem_total: f[3].trim().parse().unwrap_or(0),
                ..Default::default()
            });
        }
    }
    for (i, gpu) in out.iter().enumerate() {
        if let Some(temp) = gpu.temp {
            sensors.push(Sensor {
                chip: format!("GPU {i}"),
                label: gpu.name.clone(),
                temp,
            });
        }
    }
    out
}

/// The one temperature that stands for the machine: the CPU if it has a sensor.
pub fn main_temp(sensors: &[Sensor]) -> Option<f32> {
    let hottest = |kinds: &[SensorKind]| sensors.iter().filter(|s| kinds.contains(&s.kind())).map(|s| s.temp).reduce(f32::max);
    hottest(&[SensorKind::Cpu]).or_else(|| hottest(&[SensorKind::Other]))
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
    /// (interface, bytes received, bytes sent, physical)
    net_ifaces: Vec<(String, u64, u64, bool)>,
    disks: Vec<Disk>,
    disk_counters: Vec<DiskCounter>,
    freqs: Vec<f32>,
    sensors: Vec<Sensor>,
    gpus: Vec<Gpu>,
    /// pid -> (cpu ticks, state)
    proc_ticks: HashMap<u32, (u64, char)>,
    ps: Vec<PsRow>,
    info: HostInfo,
    tck: f64,
    /// Set when the host only reports a total percentage instead of counters (macOS without Python).
    cpu_direct: Option<CpuUsage>,
    /// Number of cores when `cores` could not be filled.
    core_count: usize,
}

pub fn parse_system(text: &str) -> Result<RawSystem> {
    let sec = sections(text);
    if !sec.contains_key("end") {
        bail!("incomplete output from remote host (is it a Linux or macOS machine?)");
    }
    if sec.contains_key("darwin") {
        return parse_mac(&sec);
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
        // Loopback and container plumbing carry traffic that a real port already counted.
        const VIRTUAL: [&str; 12] = ["veth", "docker", "br-", "virbr", "tun", "tap", "wg", "tailscale", "cni", "flannel", "cali", "bond"];
        let physical = iface != "lo" && !VIRTUAL.iter().any(|p| iface.starts_with(p));
        let f: Vec<u64> = rest.split_whitespace().map(|x| x.parse().unwrap_or(0)).collect();
        if f.len() >= 9 {
            if physical {
                raw.net_rx += f[0];
                raw.net_tx += f[8];
            }
            raw.net_ifaces.push((iface.to_string(), f[0], f[8], physical));
        }
    }

    const PSEUDO_FS: [&str; 12] = [
        "9p", "drvfs", "iso9660", "erofs",
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

    raw.sensors = parse_sensors(get("hwmon"), get("zones"));
    raw.gpus = parse_gpus(get("nvidia"), get("nvapps"), get("drm"), &mut raw.sensors);

    // "major minor name reads merged sectors ms writes merged sectors ms in_flight ms_io ..."
    let blocks = get("blocks");
    for line in get("diskstats") {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 13 || !blocks.contains(&f[2]) || VIRTUAL_DISKS.iter().any(|p| f[2].starts_with(p)) {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        raw.disk_counters.push(DiskCounter {
            name: f[2].to_string(),
            read_bytes: n(5) * 512,
            write_bytes: n(9) * 512,
            ops: n(3) + n(7),
            busy_ms: Some(n(12)),
        });
    }

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

/// `[[dd-]hh:]mm:ss[.cc]` as printed by ps, in seconds.
fn parse_clock(s: &str) -> f64 {
    let (days, rest) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().unwrap_or(0.0), rest),
        None => (0.0, s),
    };
    rest.split(':').fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap_or(0.0)) + days * 86400.0
}

/// Rows of `df -P`, with or without a filesystem type column: (type, total, available, mount).
fn df_rows<'a>(lines: &[&'a str]) -> Vec<(&'a str, u64, u64, String)> {
    let typed = lines.first().is_some_and(|header| header.contains("Type"));
    let first = if typed { 2 } else { 1 };
    lines
        .iter()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < first + 5 {
                return None;
            }
            let kib = |i: usize| f[i].parse::<u64>().unwrap_or(0) * 1024;
            Some((if typed { f[1] } else { "" }, kib(first), kib(first + 2), f[first + 4..].join(" ")))
        })
        .collect()
}

fn parse_mac(sec: &HashMap<&str, Vec<&str>>) -> Result<RawSystem> {
    let get = |name: &str| sec.get(name).map(Vec::as_slice).unwrap_or(&[]);
    let mut raw = RawSystem {
        tck: 100.0,
        ..Default::default()
    };
    let sysctl: HashMap<&str, &str> = get("sysctl").iter().filter_map(|l| l.split_once('=')).collect();
    let number = |key: &str| sysctl.get(key).and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);

    // "index user nice system idle" in ticks, one line per core.
    for line in get("cores") {
        let v: Vec<u64> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if v.len() == 5 {
            raw.cores.push(CpuTimes {
                user: v[1] + v[2],
                system: v[3],
                idle: v[4],
                ..Default::default()
            });
        }
    }
    for core in &raw.cores {
        raw.cpu.user += core.user;
        raw.cpu.system += core.system;
        raw.cpu.idle += core.idle;
    }
    if raw.cores.is_empty() {
        // "CPU usage: 5.31% user, 3.77% sys, 90.90% idle" - the last line covers the last second.
        let line = get("top").last().context("no CPU data from the Mac (neither python3 nor top answered)")?;
        let pct = |label: &str| {
            line.split([':', ','])
                .find_map(|part| part.trim().strip_suffix(label)?.trim().trim_end_matches('%').parse::<f32>().ok())
                .unwrap_or(0.0)
        };
        raw.cpu_direct = Some(CpuUsage {
            total: (100.0 - pct("idle")).clamp(0.0, 100.0),
            user: pct("user"),
            system: pct("sys"),
            ..Default::default()
        });
        raw.core_count = number("ncpu").max(1) as usize;
        raw.info.no_per_core = true;
    }

    // "{ 2.20 2.16 2.10 }"
    let load = sysctl.get("load").copied().unwrap_or("");
    for (i, v) in load.split_whitespace().filter_map(|v| v.replace(',', ".").parse().ok()).take(3).enumerate() {
        raw.load[i] = v;
    }

    // Used memory the way Activity Monitor counts it: app memory + wired + compressed.
    let pages: HashMap<&str, u64> = get("vmstat")
        .iter()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().trim_matches('"'), v.trim().trim_end_matches('.').parse().unwrap_or(0)))
        .collect();
    let page = |k: &str| pages.get(k).copied().unwrap_or(0);
    raw.mem_total = number("memsize");
    let used = page("Anonymous pages").saturating_sub(page("Pages purgeable"))
        + page("Pages wired down")
        + page("Pages occupied by compressor");
    raw.mem_used = (used * number("pagesize")).min(raw.mem_total);

    // "total = 2048.00M  used = 1024.50M  free = 1023.50M  (encrypted)"
    let swap = sysctl.get("swap").copied().unwrap_or("");
    let swap_value = |label: &str| {
        let mut it = swap.split_whitespace().skip_while(|w| *w != label).skip(2);
        it.next().map(|v| parse_size(&format!("{v}iB"))).unwrap_or(0)
    };
    raw.swap_total = swap_value("total");
    raw.swap_used = swap_value("used");

    // "{ sec = 1794304814, usec = 153828 } Tue Nov 10 ..."
    let boot = sysctl
        .get("boottime")
        .and_then(|v| v.split("sec = ").nth(1)?.split(',').next()?.trim().parse::<u64>().ok());
    raw.uptime = boot.map(|b| number("now").saturating_sub(b)).unwrap_or(0);

    // One "<Link#n>" row per interface carries its byte counters; en* are the physical ports.
    for line in get("net") {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 9 && f[2].starts_with("<Link") {
            let name = f[0].trim_end_matches('*');
            let (rx, tx) = (f[f.len() - 5].parse::<u64>().unwrap_or(0), f[f.len() - 2].parse::<u64>().unwrap_or(0));
            let physical = name.starts_with("en");
            if physical {
                raw.net_rx += rx;
                raw.net_tx += tx;
            }
            raw.net_ifaces.push((name.to_string(), rx, tx, physical));
        }
    }

    // APFS volumes share one container: the data volume stands for the internal disk,
    // and what is not available counts as used.
    for (fstype, total, available, mount) in df_rows(get("df")) {
        let mount = if mount == "/System/Volumes/Data" { "/".to_string() } else { mount };
        if total == 0 || !(mount == "/" || mount.starts_with("/Volumes/")) || raw.disks.iter().any(|d| d.mount == mount) {
            continue;
        }
        raw.disks.push(Disk {
            mount,
            fstype: fstype.to_string(),
            total,
            used: total.saturating_sub(available),
        });
    }

    // One line per disk driver: {"Operations (Write)"=14730925,...,"Bytes (Read)"=747853242368,...}
    for line in get("ioreg") {
        let n = |key: &str| {
            let value = line.split(&format!("\"{key}\"=")).nth(1).and_then(|v| v.split([',', '}']).next());
            value.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0)
        };
        let (read_bytes, write_bytes) = (n("Bytes (Read)"), n("Bytes (Write)"));
        // Drivers without any traffic are card readers and other empty slots.
        if read_bytes + write_bytes > 0 {
            raw.disk_counters.push(DiskCounter {
                name: format!("disk{}", raw.disk_counters.len()),
                read_bytes,
                write_bytes,
                ops: n("Operations (Read)") + n("Operations (Write)"),
                busy_ms: None,
            });
        }
    }

    // macmon prints one JSON object: {"temp":{"cpu_temp_avg":34.9,"gpu_temp_avg":37.1},...}
    if let Some(v) = get("macmon").first().and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        // "gpu_usage":[frequency in MHz, share of time busy]
        if let Some(busy) = v["gpu_usage"][1].as_f64() {
            raw.gpus.push(Gpu {
                name: "Apple GPU".to_string(),
                util: (busy * 100.0) as f32,
                temp: v["temp"]["gpu_temp_avg"].as_f64().map(|t| t as f32),
                power: v["gpu_power"].as_f64().map(|p| p as f32),
                ..Default::default()
            });
        }
        for (chip, key) in [("CPU", "cpu_temp_avg"), ("GPU", "gpu_temp_avg")] {
            if let Some(temp) = v["temp"][key].as_f64().filter(|t| (1.0..150.0).contains(t)) {
                raw.sensors.push(Sensor {
                    chip: chip.to_string(),
                    label: "average".to_string(),
                    temp: temp as f32,
                });
            }
        }
    }
    raw.info.is_mac = true;

    // pid user rss %mem etime state time args
    for line in get("ps") {
        let mut it = line.split_whitespace();
        let Some(Ok(pid)) = it.next().map(str::parse::<u32>) else {
            continue; // header line
        };
        let user = it.next().unwrap_or("").to_string();
        let rss = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
        let mem_pct = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let elapsed = parse_clock(it.next().unwrap_or("")) as u64;
        let state = it.next().and_then(|v| v.chars().next()).unwrap_or('?');
        let ticks = (parse_clock(it.next().unwrap_or("")) * raw.tck).round() as u64;
        raw.proc_ticks.insert(pid, (ticks, state));
        raw.ps.push(PsRow {
            pid,
            user,
            rss,
            mem_pct,
            elapsed,
            cmd: it.collect::<Vec<_>>().join(" "),
        });
    }

    for line in get("info") {
        match line.split_once('=') {
            Some(("kernel", v)) => raw.info.kernel = v.trim().to_string(),
            Some(("model", v)) => raw.info.cpu_model = v.trim().to_string(),
            Some(("os", v)) => raw.info.os = v.trim().to_string(),
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

    // Interfaces that never carried anything (unused ports) are left out.
    let net = cur
        .net_ifaces
        .iter()
        .filter(|n| n.1 + n.2 > 0)
        .map(|(name, rx, tx, physical)| {
            let before = prev.net_ifaces.iter().find(|p| p.0 == *name);
            NetIface {
                name: name.clone(),
                rx_bps: before.map_or(0.0, |p| rate(*rx, p.1)),
                tx_bps: before.map_or(0.0, |p| rate(*tx, p.2)),
                rx_total: *rx,
                tx_total: *tx,
                physical: *physical,
            }
        })
        .collect();
    let disk_io = cur
        .disk_counters
        .iter()
        .filter_map(|c| {
            let p = prev.disk_counters.iter().find(|p| p.name == c.name)?;
            Some(DiskIo {
                name: c.name.clone(),
                read_bps: rate(c.read_bytes, p.read_bytes),
                write_bps: rate(c.write_bytes, p.write_bytes),
                iops: rate(c.ops, p.ops),
                util: c.busy_ms.zip(p.busy_ms).map(|(now, before)| (rate(now, before) / 10.0).clamp(0.0, 100.0) as f32),
            })
        })
        .collect();

    Sample {
        ts,
        cpu: cur.cpu_direct.unwrap_or_else(|| cur.cpu.usage_since(&prev.cpu)),
        cores: match cur.cpu_direct {
            // No per-core data: every core stands at the average, so core counts stay right.
            Some(cpu) => vec![cpu.total; cur.core_count],
            None => cur.cores.iter().zip(&prev.cores).map(|(c, p)| c.usage_since(p).total).collect(),
        },
        freqs: cur.freqs.clone(),
        load: cur.load,
        mem_total: cur.mem_total,
        mem_used: cur.mem_used,
        swap_total: cur.swap_total,
        swap_used: cur.swap_used,
        rx_bps: rate(cur.net_rx, prev.net_rx),
        tx_bps: rate(cur.net_tx, prev.net_tx),
        net,
        disks: cur.disks.clone(),
        disk_io,
        temp: main_temp(&cur.sensors),
        sensors: cur.sensors.clone(),
        gpus: cur.gpus.clone(),
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
        // The disk reads 1000 sectors and is busy for 2.5 s between the two polls.
        let (sectors, busy) = if rx > 1000 { (2000, 3000) } else { (1000, 500) };
        format!(
            "@@stat\n{cpu}\n{cpu0}\nintr 1 2 3\n@@loadavg\n0.52 0.40 0.30 1/200 999\n\
             @@meminfo\nMemTotal:       8000000 kB\nMemFree:        1000000 kB\nMemAvailable:   6000000 kB\n\
             SwapTotal:      2000000 kB\nSwapFree:       1500000 kB\n@@uptime\n86400.55 1000.0\n\
             @@net\nInter-|   Receive\n face |bytes\n    lo: 500 1 0 0 0 0 0 0 500 1 0 0 0 0 0 0\n\
             \x20 eth0: {rx} 10 0 0 0 0 0 0 2000 10 0 0 0 0 0 0\n veth1a: 9999 1 0 0 0 0 0 0 9999 1 0 0 0 0 0 0\n\
             @@df\nFilesystem Type 1024-blocks Used Available Capacity Mounted on\n\
             /dev/sda1 ext4 1000000 900000 100000 90% /\ntmpfs tmpfs 5000 0 5000 0% /run\n\
             /dev/sdb1 xfs 2000000 500000 1500000 25% /mnt/my data\n\
             @@freq\ncpu MHz\t\t: 3400.000\n\
             @@diskstats\n   7       0 loop0 10 0 80 5 0 0 0 0 0 5 5\n\
             \x20  8       0 sda 100 0 {sectors} 500 50 0 2000 700 0 {busy} 1200 0 0 0 0\n\
             \x20  8       1 sda1 90 0 900 400 50 0 2000 700 0 800 1100\n\
             \x20253       0 dm-0 90 0 900 400 50 0 2000 700 0 800 1100\n\
             @@blocks\ndm-0\nloop0\nsda\n\
             @@hwmon\n/sys/class/hwmon/hwmon0|k10temp|Tctl|61500\n/sys/class/hwmon/hwmon0|k10temp|Tccd1|58000\n\
             /sys/class/hwmon/hwmon1|nvme|Composite|41000\n/sys/class/hwmon/hwmon2|nvme|Composite|44000\n\
             /sys/class/hwmon/hwmon3|nct6779|AUXTIN3|91000\n/sys/class/hwmon/hwmon3|nct6779|PCH_CHIP_TEMP|0\n\
             /sys/class/hwmon/hwmon4|acpitz||40000\n\
             @@zones\nacpitz|40000\nx86_pkg_temp|60000\n\
             @@nvidia\n0, NVIDIA GeForce RTX 5090, 37, 8192, 32768, 64, 210.50\n@@nvapps\n4096\n\
             @@drm\n/sys/class/drm/card1|12|1048576|8388608\n\
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
        let net: Vec<(&str, f64, u64, bool)> = s.net.iter().map(|n| (n.name.as_str(), n.rx_bps, n.rx_total, n.physical)).collect();
        assert_eq!(net, vec![("lo", 0.0, 500, false), ("eth0", 1000.0, 6000, true), ("veth1a", 0.0, 9999, false)]);
        // The CPU sensor counts, not the hotter (and unreliable) mainboard input.
        assert_eq!(s.temp, Some(61.5));
        let sensors: Vec<(&str, &str, f32)> = s.sensors.iter().map(|x| (x.chip.as_str(), x.label.as_str(), x.temp)).collect();
        assert_eq!(
            sensors,
            vec![
                ("k10temp", "Tctl", 61.5),
                ("k10temp", "Tccd1", 58.0),
                ("nvme", "Composite", 41.0),
                ("nvme #2", "Composite", 44.0),
                ("nct6779", "AUXTIN3", 91.0),
                ("acpitz", "", 40.0),
                ("x86_pkg_temp", "zone 1", 60.0),
                ("GPU 0", "NVIDIA GeForce RTX 5090", 64.0),
            ]
        );
        assert_eq!(s.sensors[2].kind(), SensorKind::Disk);
        assert_eq!(chip_temps(&s.sensors)[0], ("k10temp".to_string(), 61.5));

        assert_eq!(s.gpus.len(), 2);
        let g = &s.gpus[0];
        assert_eq!((g.util, g.mem_used, g.mem_total, g.power), (37.0, 8192 << 20, 32768 << 20, Some(210.5)));
        assert_eq!((s.gpus[1].name.as_str(), s.gpus[1].util, s.gpus[1].mem_total), ("AMD GPU (card1)", 12.0, 8388608));

        // Only the whole disk counts: no loop device, partition or device mapper volume.
        assert_eq!(s.disk_io.len(), 1);
        let d = &s.disk_io[0];
        assert_eq!((d.name.as_str(), d.read_bps, d.write_bps, d.iops, d.util), ("sda", 102_400.0, 0.0, 0.0, Some(50.0)));
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

    /// Poll a real machine with the system `ssh`:
    /// `GRIDWATCH_TEST_HOST=user@host cargo test live_host -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_host() {
        let host = std::env::var("GRIDWATCH_TEST_HOST").expect("set GRIDWATCH_TEST_HOST");
        let poll = || {
            let out = std::process::Command::new("ssh").args(["-o", "BatchMode=yes", &host, &wrap(&system_script())]).output().unwrap();
            parse_system(&String::from_utf8_lossy(&out.stdout)).unwrap()
        };
        let a = poll();
        std::thread::sleep(std::time::Duration::from_secs(3));
        let started = std::time::Instant::now();
        let b = poll();
        let mut s = build_sample(&a, &b, 3.0 + started.elapsed().as_secs_f64(), 0);
        s.procs.truncate(6);
        println!("poll took {:?}
{s:#?}", started.elapsed());
        assert!(!s.cores.is_empty() && s.mem_total > 0 && !s.disks.is_empty() && s.proc_count > 0);
    }

    #[test]
    fn gpu_with_shared_memory_takes_usage_from_its_processes() {
        let mut sensors = Vec::new();
        let gpus = parse_gpus(&["0, NVIDIA GB10, 0, [N/A], [N/A], 39, 9.48"], &["170", "170"], &[], &mut sensors);
        assert_eq!((gpus[0].mem_used, gpus[0].mem_total, gpus[0].temp), (340 << 20, 0, Some(39.0)));
        assert_eq!(sensors.len(), 1);
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
        assert!(!MAC_SCRIPT.contains('\''));
    }

    fn mac_output(cpu: &str, ticks: &str, rx: u64) -> String {
        format!(
            "@@darwin\n{cpu}\n@@sysctl\nncpu=2\nmemsize=17179869184\npagesize=16384\n\
             boottime={{ sec = 1000000, usec = 153828 }} Tue Nov 10 11:00:14 2026\n\
             swap=total = 2048.00M  used = 512.00M  free = 1536.00M  (encrypted)\nload={{ 2.20 2.16 2.10 }}\nnow=1086400\n\
             @@vmstat\nMach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:        1000.\n\
             Pages wired down:      100000.\nPages purgeable:        20000.\n\"Translation faults\":   984444550.\n\
             Anonymous pages:       220000.\nPages occupied by compressor:   50000.\n\
             @@net\nName       Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll\n\
             lo0        16384 <Link#1>                       1594102     0  550720904  1594102     0  550720904     0\n\
             en0        1500  <Link#16>   02:00:00:00:00:01  7254849     0 {rx}  2789985     0  4000     0\n\
             en0        1500  192.0.2       192.0.2.10       7254849     - {rx}  2789985     -  4000     -\n\
             utun0      1380  <Link#19>                            0     0          0       69     0      12525     0\n\
             @@df\nFilesystem      Type   1024-blocks       Used  Available Capacity  Mounted on\n\
             /dev/disk3s1s1  apfs    1000000   12000 400000     1%    /\n\
             devfs           devfs          202        202          0   100%    /dev\n\
             /dev/disk3s5    apfs    1000000  580000 400000    60%    /System/Volumes/Data\n\
             map auto_home   autofs        0          0          0   100%    /System/Volumes/Data/home\n\
             /dev/disk5s1    exfat    500000  100000 400000    20%    /Volumes/My Backup\n\
             @@ioreg\n      \"Statistics\" = {{\"Operations (Write)\"=0,\"Bytes (Read)\"=0,\"Bytes (Write)\"=0,\"Operations (Read)\"=0}}\n\
             \x20 |   \"Statistics\" = {{\"Operations (Write)\"=100,\"Latency Time (Write)\"=0,\"Bytes (Read)\"={rx},\"Total Time (Read)\"=5,\"Bytes (Write)\"=7000,\"Operations (Read)\"=200,\"Retries (Write)\"=0}}\n\
             @@ps\n  PID USER                RSS %MEM     ELAPSED STAT      TIME ARGS\n\
             \x20 422 _windowserver     204800  1.2 07-01:21:32 Rs   {ticks} /System/Library/WindowServer -daemon\n\
             \x20 900 void                   0  0.0       00:10 Z      0:00.00 (defunct)\n\
             @@info\nkernel=Darwin 25.6.0\nmodel=Apple M5 Max\nos=macOS 26.7\n\
             @@macmon\n{{\"all_power\":1.2,\"gpu_power\":0.5,\"gpu_usage\":[338,0.25],\"temp\":{{\"cpu_temp_avg\":34.5,\"gpu_temp_avg\":37.25}}}}\n@@end\n"
        )
    }

    #[test]
    fn parses_macos_with_per_core_counters() {
        let a = parse_system(&mac_output("@@cores\n0 100 0 100 800\n1 0 0 0 1000", "949:08.11", 1000)).unwrap();
        let b = parse_system(&mac_output("@@cores\n0 300 50 150 1000\n1 0 0 0 1500", "949:10.61", 6000)).unwrap();
        let s = build_sample(&a, &b, 5.0, 1);

        // core 0: user 250, system 50, idle 200 of 500; core 1 idle
        assert_eq!(s.cores, vec![60.0, 0.0]);
        assert!((s.cpu.total - 30.0).abs() < 0.01);
        assert!((s.cpu.user - 25.0).abs() < 0.01);
        assert!(!s.info.no_per_core);
        assert_eq!(s.load, [2.20, 2.16, 2.10]);
        assert_eq!(s.mem_total, 16 << 30);
        // (220000 - 20000 + 100000 + 50000) pages of 16 KiB
        assert_eq!(s.mem_used, 350_000 * 16384);
        assert_eq!(s.swap_total, 2048 << 20);
        assert_eq!(s.swap_used, 512 << 20);
        assert_eq!(s.uptime, 86400);
        assert_eq!(s.rx_bps, 1000.0); // lo0, utun0 and the address rows are ignored
        let net: Vec<(&str, f64, bool)> = s.net.iter().map(|n| (n.name.as_str(), n.rx_bps, n.physical)).collect();
        assert_eq!(net, vec![("lo0", 0.0, false), ("en0", 1000.0, true), ("utun0", 0.0, false)]);
        assert_eq!(s.info.os, "macOS 26.7");
        assert!(s.info.is_mac);
        assert_eq!(s.temp, Some(34.5));
        assert_eq!(s.sensors.len(), 2);
        assert_eq!((s.sensors[1].kind(), s.sensors[1].temp), (SensorKind::Gpu, 37.25));
        assert_eq!(s.gpus.len(), 1);
        assert_eq!((s.gpus[0].util, s.gpus[0].mem_total, s.gpus[0].temp, s.gpus[0].power), (25.0, 0, Some(37.25), Some(0.5)));
        assert_eq!(s.disk_io.len(), 1);
        assert_eq!((s.disk_io[0].name.as_str(), s.disk_io[0].read_bps, s.disk_io[0].util), ("disk0", 1000.0, None));

        // The data volume stands for the internal disk; used = total - available.
        let disks: Vec<(&str, f32)> = s.disks.iter().map(|d| (d.mount.as_str(), d.pct().round())).collect();
        assert_eq!(disks, vec![("/", 60.0), ("/Volumes/My Backup", 20.0)]);
        assert_eq!(s.disks[0].fstype, "apfs");

        assert_eq!((s.proc_count, s.zombies), (2, 1));
        let p = &s.procs[0];
        // 2.5 CPU seconds in 5 s = half a core
        assert_eq!((p.pid, p.cpu.round(), p.state), (422, 50.0, 'R'));
        assert_eq!(p.elapsed, 7 * 86400 + 3600 + 21 * 60 + 32);
        assert_eq!(p.cmd, "/System/Library/WindowServer -daemon");
    }

    #[test]
    fn parses_macos_without_python() {
        let top = "@@top\nCPU usage: 2.57% user, 6.6% sys, 91.36% idle \nCPU usage: 5.31% user, 3.77% sys, 90.90% idle ";
        let a = parse_system(&mac_output(top, "0:01.00", 0)).unwrap();
        let s = build_sample(&a, &a, 5.0, 1);
        assert!((s.cpu.total - 9.1).abs() < 0.01);
        assert_eq!(s.cpu.user, 5.31);
        assert_eq!(s.cpu.system, 3.77);
        assert_eq!(s.cores.len(), 2);
        assert!(s.info.no_per_core);
        assert!(parse_system(&mac_output("@@top", "0:01.00", 0)).is_err());
    }
}
