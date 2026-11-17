//! Rule based advice derived from the latest sample and the recent history.

use crate::model::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Critical,
    Warning,
    Info,
    Good,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "Critical",
            Severity::Warning => "Warning",
            Severity::Info => "Info",
            Severity::Good => "OK",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// What to do about it; shell commands are written in backticks.
    pub action: String,
}

fn short(cmd: &str) -> String {
    let mut s: String = cmd.chars().take(60).collect();
    if cmd.chars().count() > 60 {
        s.push('…');
    }
    s
}

pub fn analyze(state: &ServerState) -> Vec<Suggestion> {
    let Some(s) = &state.latest else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut add = |severity, title: String, detail: String, action: &str| {
        out.push(Suggestion {
            severity,
            title,
            detail,
            action: action.to_string(),
        });
    };
    use Severity::*;

    // Averages over the last five minutes smooth out short spikes.
    let recent: Vec<&HistPoint> = state.history.iter().filter(|p| p.ts >= s.ts - 300).collect();
    let avg = |f: &dyn Fn(&HistPoint) -> f32, fallback: f32| {
        if recent.is_empty() {
            fallback
        } else {
            recent.iter().map(|p| f(p)).sum::<f32>() / recent.len() as f32
        }
    };
    let cores = s.cores.len().max(1) as f32;
    let cpu_avg = avg(&|p| p.cpu.total, s.cpu.total);
    let iowait_avg = avg(&|p| p.cpu.iowait, s.cpu.iowait);
    let steal_avg = avg(&|p| p.cpu.steal, s.cpu.steal);
    let top_cpu = s.procs.iter().max_by(|a, b| a.cpu.total_cmp(&b.cpu));
    let top_mem = s.procs.iter().max_by_key(|p| p.rss);

    if cpu_avg > 85.0 {
        let culprit = top_cpu
            .map(|p| format!(" Busiest process: {} (pid {}, {:.0}% of a core).", short(&p.cmd), p.pid, p.cpu))
            .unwrap_or_default();
        add(
            if cpu_avg > 95.0 { Critical } else { Warning },
            format!("CPU saturated: {cpu_avg:.0}% average over 5 minutes"),
            format!("All {cores:.0} cores are busy most of the time.{culprit}"),
            "Move batch work off-peak, limit the process with `nice` / `cpulimit`, or add cores.",
        );
    }

    let load_ratio = s.load[1] / cores;
    if load_ratio > 1.5 {
        add(
            if load_ratio > 3.0 { Critical } else { Warning },
            format!("Load {:.1} exceeds the {cores:.0} available cores", s.load[1]),
            format!(
                "The 5 minute load average is {load_ratio:.1}x the core count, so tasks are queueing for CPU or disk."
            ),
            "Check the Processes tab for runaway jobs; compare with I/O wait to tell CPU from disk pressure.",
        );
    }

    if iowait_avg > 15.0 {
        add(
            Warning,
            format!("High I/O wait: {iowait_avg:.0}%"),
            "CPUs sit idle waiting for storage. The disk, not the processor, is the bottleneck.".into(),
            "Find the writer with `iotop -o`, check disk health with `smartctl -a`, consider faster storage.",
        );
    }

    if steal_avg > 10.0 {
        add(
            Warning,
            format!("CPU steal at {steal_avg:.0}%"),
            "The hypervisor gives this VM's CPU time to other guests (noisy neighbour or oversold host).".into(),
            "Ask the provider to migrate the VM or move to dedicated cores.",
        );
    }

    if cpu_avg < 50.0 {
        if let Some((i, v)) = s.cores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) {
            if *v > 95.0 && s.cores.len() > 1 {
                add(
                    Info,
                    format!("Core {i} is pegged while the machine is mostly idle"),
                    format!("One core runs at {v:.0}% but total CPU is only {cpu_avg:.0}%: a single-threaded workload is the limit."),
                    "More cores will not help this workload; use a faster core or parallelise the job.",
                );
            }
        }
    }

    if s.mem_total > 0 {
        let pct = s.mem_used as f32 / s.mem_total as f32 * 100.0;
        if pct > 90.0 {
            let culprit = top_mem
                .map(|p| format!(" Largest process: {} ({}).", short(&p.cmd), fmt_bytes(p.rss)))
                .unwrap_or_default();
            add(
                if pct > 95.0 { Critical } else { Warning },
                format!("Memory almost exhausted: {pct:.0}% used"),
                format!(
                    "Only {} of {} is still available; the OOM killer may start terminating processes.{culprit}",
                    fmt_bytes(s.mem_total - s.mem_used),
                    fmt_bytes(s.mem_total)
                ),
                "Restart or cap the largest consumer, add RAM, or add swap as a safety net.",
            );
        }
        if s.swap_total > 0 && s.swap_used as f32 / s.swap_total as f32 > 0.5 && pct > 75.0 {
            add(
                Warning,
                format!("Heavy swapping: {} in swap", fmt_bytes(s.swap_used)),
                "Memory pressure pushes pages to disk, which slows everything down.".into(),
                "Reduce memory usage or add RAM; check `vmstat 1` for si/so activity.",
            );
        }
    }

    for d in &s.disks {
        let pct = d.pct();
        if pct > 85.0 {
            add(
                if pct > 95.0 { Critical } else { Warning },
                format!("Disk {} is {pct:.0}% full", d.mount),
                format!("{} free of {}.", fmt_bytes(d.total.saturating_sub(d.used)), fmt_bytes(d.total)),
                "Find large directories with `du -xh --max-depth=2 / | sort -h | tail`, rotate logs, run `docker system df`.",
            );
        }
    }

    if let Some(t) = s.temp {
        if t > 80.0 {
            add(
                if t > 90.0 { Critical } else { Warning },
                format!("High temperature: {t:.0} °C"),
                "The CPU will throttle to protect itself, reducing performance.".into(),
                "Check fans, airflow and dust; verify the heatsink is seated.",
            );
        }
    }

    // Disks and GPUs have their own limits; mainboard inputs are too unreliable to judge.
    for sensor in s.sensors.iter().filter(|x| matches!(x.kind(), SensorKind::Disk | SensorKind::Gpu)) {
        let (warm, hot) = sensor.limits();
        if sensor.temp >= warm {
            let disk = sensor.kind() == SensorKind::Disk;
            add(
                if sensor.temp >= hot { Critical } else { Warning },
                format!("{} {} is at {:.0} °C", if disk { "Disk" } else { "GPU" }, sensor.chip, sensor.temp),
                if disk {
                    format!("Drives age quickly above {warm:.0} °C and may throttle or fail above {hot:.0} °C.")
                } else {
                    format!("{} will throttle above {hot:.0} °C.", if sensor.label.is_empty() { "The GPU" } else { sensor.label.as_str() })
                },
                "Improve the airflow around it; check fans and dust.",
            );
        }
    }

    let util_avg = {
        let utils: Vec<f32> = recent.iter().filter_map(|p| p.disk_util).collect();
        (!utils.is_empty()).then(|| utils.iter().sum::<f32>() / utils.len() as f32)
    };
    if let Some(util) = util_avg.filter(|u| *u > 85.0) {
        let busiest = s.disk_io.iter().max_by(|a, b| a.util.unwrap_or(0.0).total_cmp(&b.util.unwrap_or(0.0)));
        let (name, detail) = match busiest {
            Some(d) => (
                d.name.clone(),
                format!("Right now it reads {} and writes {} with {:.0} operations per second.", fmt_bytes_rate(d.read_bps), fmt_bytes_rate(d.write_bps), d.iops),
            ),
            None => ("A disk".to_string(), String::new()),
        };
        add(
            Warning,
            format!("Disk {name} is saturated: busy {util:.0}% of the time"),
            format!("Requests queue up behind each other, so everything that touches this disk slows down. {detail}"),
            "Find the process with `iotop -o`, spread the load over more disks or move to faster storage.",
        );
    }

    for (i, gpu) in s.gpus.iter().enumerate() {
        if gpu.mem_total > 0 && gpu.mem_used as f64 > gpu.mem_total as f64 * 0.95 {
            add(
                Info,
                format!("GPU {i} memory is almost full"),
                format!("{} uses {} of {}; the next allocation may fail.", gpu.name, fmt_bytes(gpu.mem_used), fmt_bytes(gpu.mem_total)),
                "See which process holds the memory with `nvidia-smi`.",
            );
        }
    }

    if s.zombies > 0 {
        add(
            if s.zombies >= 10 { Warning } else { Info },
            format!("{} zombie process(es)", s.zombies),
            "Finished processes whose parent never collected the exit status.".into(),
            "Find the parents with `ps -eo pid,ppid,stat,cmd | grep -w Z` and restart them.",
        );
    }

    if let Some(p) = s.procs.iter().find(|p| p.cpu > 90.0 && p.elapsed > 86400) {
        add(
            Info,
            format!("Long-running process uses a full core (pid {})", p.pid),
            format!("{} has been running for {} at {:.0}% CPU.", short(&p.cmd), fmt_duration(p.elapsed), p.cpu),
            "Verify this is intended work and not a stuck loop.",
        );
    }

    if s.info.reboot_required {
        add(
            Info,
            "Reboot required".into(),
            "Installed updates (usually a kernel or libc) only take effect after a restart.".into(),
            "Schedule a maintenance window and run `sudo reboot`.",
        );
    } else if s.uptime > 180 * 86400 {
        add(
            Info,
            format!("Uptime is {}", fmt_duration(s.uptime)),
            "The running kernel has probably missed several security updates.".into(),
            "Apply updates and reboot, or use live patching.",
        );
    }

    match &state.docker {
        DockerState::Error(msg) => add(
            Info,
            "Docker is installed but not accessible".into(),
            msg.clone(),
            "Start the docker daemon if it is not running. On Linux, add the SSH user to the docker group: `sudo usermod -aG docker $USER`, then log in again.",
        ),
        DockerState::Ok(list) => {
            // One finding for all failed containers: old leftovers would otherwise bury everything else.
            let failed: Vec<&Container> = list.iter().filter(|c| c.failed()).collect();
            if let Some(first) = failed.first() {
                let mut names: Vec<&str> = failed.iter().take(6).map(|c| c.name.as_str()).collect();
                if failed.len() > names.len() {
                    names.push("…");
                }
                add(
                    Warning,
                    if failed.len() == 1 {
                        format!("Container {} exited with an error", first.name)
                    } else {
                        format!("{} containers exited with an error", failed.len())
                    },
                    if failed.len() == 1 {
                        format!("Status: {}.", first.status)
                    } else {
                        format!("{}. Exit code 137 means killed (out of memory or `docker kill`).", names.join(", "))
                    },
                    &format!("Check `docker logs --tail 50 {}`; remove containers that are no longer needed.", first.name),
                );
            }
            for c in list {
                if c.state == "restarting" {
                    add(
                        Critical,
                        format!("Container {} is in a restart loop", c.name),
                        format!("Status: {}.", c.status),
                        &format!("Read the crash reason with `docker logs --tail 50 {}`.", c.name),
                    );
                } else if c.unhealthy() {
                    add(
                        Warning,
                        format!("Container {} is unhealthy", c.name),
                        format!("Its health check is failing. Status: {}.", c.status),
                        &format!("Inspect with `docker inspect --format \"{{{{json .State.Health}}}}\" {}`.", c.name),
                    );
                }
                if c.has_stats && c.mem_limit > 0 {
                    let pct = c.mem_used as f32 / c.mem_limit as f32 * 100.0;
                    let limited = c.mem_limit < s.mem_total.saturating_sub(s.mem_total / 20);
                    if limited && pct > 90.0 {
                        add(
                            Warning,
                            format!("Container {} is at {pct:.0}% of its memory limit", c.name),
                            format!("{} of {} used; it will be OOM-killed at the limit.", fmt_bytes(c.mem_used), fmt_bytes(c.mem_limit)),
                            "Raise the limit (`--memory`) or fix the leak.",
                        );
                    } else if !limited && s.mem_total > 0 && c.mem_used > s.mem_total / 3 {
                        add(
                            Info,
                            format!("Container {} has no memory limit", c.name),
                            format!("It uses {} and could take the whole host down.", fmt_bytes(c.mem_used)),
                            "Set a limit with `--memory` or `deploy.resources.limits.memory` in compose.",
                        );
                    }
                }
                if c.has_stats && c.cpu > 80.0 * cores {
                    add(
                        Warning,
                        format!("Container {} uses {:.0}% CPU", c.name, c.cpu),
                        format!("That is most of the {cores:.0} cores of this host."),
                        "Cap it with `--cpus` so other services keep responding.",
                    );
                }
            }
            let stopped = list.iter().filter(|c| c.state == "exited" || c.state == "dead").count();
            if stopped >= 5 {
                add(
                    Info,
                    format!("{stopped} stopped containers"),
                    "Stopped containers keep their writable layers and logs on disk.".into(),
                    "Clean up with `docker container prune`.",
                );
            }
        }
        _ => {}
    }

    if out.is_empty() {
        out.push(Suggestion {
            severity: Good,
            title: "No issues detected".into(),
            detail: "CPU, memory, disks and containers are all within healthy limits.".into(),
            action: String::new(),
        });
    }
    out.sort_by_key(|s| s.severity);
    out
}

/// Number of findings that need attention (warnings and worse).
pub fn alert_count(list: &[Suggestion]) -> usize {
    list.iter().filter(|s| s.severity <= Severity::Warning).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> ServerState {
        ServerState {
            latest: Some(Sample {
                ts: 1000,
                cores: vec![10.0; 4],
                load: [0.5; 3],
                mem_total: 8 << 30,
                mem_used: 2 << 30,
                disks: vec![Disk { mount: "/".into(), fstype: "ext4".into(), total: 100, used: 40 }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn healthy_machine_is_ok() {
        let list = analyze(&healthy());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].severity, Severity::Good);
        assert_eq!(alert_count(&list), 0);
    }

    #[test]
    fn flags_problems_most_severe_first() {
        let mut state = healthy();
        let s = state.latest.as_mut().unwrap();
        s.disks[0].used = 97;
        s.mem_used = 7400 << 20;
        s.zombies = 2;
        state.docker = DockerState::Ok(vec![Container {
            name: "api".into(),
            state: "restarting".into(),
            status: "Restarting (1) 3 seconds ago".into(),
            ..Default::default()
        }]);
        let list = analyze(&state);
        let titles: Vec<&str> = list.iter().map(|s| s.title.as_str()).collect();
        assert!(titles.iter().any(|t| t.contains("Disk / is 97% full")), "{titles:?}");
        assert!(titles.iter().any(|t| t.contains("Memory almost exhausted")), "{titles:?}");
        assert!(titles.iter().any(|t| t.contains("restart loop")), "{titles:?}");
        assert!(titles.iter().any(|t| t.contains("zombie")), "{titles:?}");
        assert_eq!(list[0].severity, Severity::Critical);
        assert_eq!(list.last().unwrap().severity, Severity::Info);
        assert_eq!(alert_count(&list), 3);
    }

    #[test]
    fn flags_disks_and_gpus() {
        let mut state = healthy();
        let s = state.latest.as_mut().unwrap();
        s.sensors = vec![
            Sensor { chip: "nvme".into(), label: "Composite".into(), temp: 74.0 },
            Sensor { chip: "drivetemp".into(), label: String::new(), temp: 61.0 },
            Sensor { chip: "nct6779".into(), label: "AUXTIN3".into(), temp: 120.0 },
            Sensor { chip: "GPU 0".into(), label: "RTX".into(), temp: 70.0 },
        ];
        s.disk_io = vec![DiskIo { name: "sda".into(), util: Some(99.0), ..Default::default() }];
        s.gpus = vec![Gpu { name: "RTX".into(), mem_used: 97, mem_total: 100, ..Default::default() }];
        state.history.push_back(HistPoint { ts: 990, disk_util: Some(96.0), ..Default::default() });
        let list = analyze(&state);
        let titles: Vec<(Severity, &str)> = list.iter().map(|s| (s.severity, s.title.as_str())).collect();
        assert_eq!(
            titles,
            vec![
                (Severity::Critical, "Disk drivetemp is at 61 °C"),
                (Severity::Warning, "Disk nvme is at 74 °C"),
                (Severity::Warning, "Disk sda is saturated: busy 96% of the time"),
                (Severity::Info, "GPU 0 memory is almost full"),
            ]
        );
    }

}
