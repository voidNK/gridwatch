//! Local history in SQLite, plus the JSON config file.

use crate::model::*;
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub servers: Vec<ServerConfig>,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_retention")]
    pub retention_days: u32,
    #[serde(default)]
    pub notifications: Notifications,
    /// List the computer gridwatch runs on.
    #[serde(default = "yes")]
    pub show_local: bool,
    /// List the default WSL distribution (Windows only). Off by default: polling keeps WSL running.
    #[serde(default)]
    pub show_wsl: bool,
    /// Keep passwords and passphrases in the keychain of the operating system.
    #[serde(default = "yes")]
    pub remember_secrets: bool,
    /// Ask GitHub at start whether a newer version exists.
    #[serde(default = "yes")]
    pub check_updates: bool,
    /// Show an icon in the tray and keep running there when the window is closed.
    #[serde(default = "yes")]
    pub tray: bool,
    /// Offer to start, stop and restart docker containers.
    #[serde(default)]
    pub allow_container_actions: bool,
    /// Offer to end processes.
    #[serde(default)]
    pub allow_kill: bool,
}

fn yes() -> bool {
    true
}

/// How the user wants to hear about trouble.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Master switch, the bell button in the top bar.
    pub enabled: bool,
    pub sound: bool,
    pub desktop: bool,
    /// Stay quiet for warnings.
    pub critical_only: bool,
    /// Alert when a server cannot be reached.
    pub offline: bool,
    /// 0.0 to 1.0
    pub volume: f32,
    /// Alert again while problems stay open; 0 = only once.
    pub repeat_minutes: u32,
    /// A coloured mark and a symbol for the topic in desktop notifications.
    pub emoji: bool,
    pub warning_tone: Tone,
    pub critical_tone: Tone,
}

/// The sounds an alert can make.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tone {
    #[default]
    Chime,
    Beeps,
    Bell,
    Sonar,
    Alarm,
}

impl Tone {
    pub const ALL: [Tone; 5] = [Tone::Chime, Tone::Beeps, Tone::Bell, Tone::Sonar, Tone::Alarm];

    pub fn label(self) -> &'static str {
        match self {
            Tone::Chime => "Chime",
            Tone::Beeps => "Beeps",
            Tone::Bell => "Bell",
            Tone::Sonar => "Sonar",
            Tone::Alarm => "Alarm",
        }
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            sound: true,
            desktop: true,
            critical_only: false,
            offline: true,
            volume: 0.7,
            repeat_minutes: 0,
            emoji: true,
            warning_tone: Tone::Chime,
            critical_tone: Tone::Beeps,
        }
    }
}

fn default_interval() -> u64 {
    5
}

fn default_retention() -> u32 {
    14
}

impl Default for Config {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            interval_secs: default_interval(),
            retention_days: default_retention(),
            notifications: Notifications::default(),
            show_local: true,
            show_wsl: false,
            remember_secrets: true,
            check_updates: true,
            tray: true,
            allow_container_actions: false,
            allow_kill: false,
        }
    }
}

/// Where gridwatch keeps its files on this machine.
pub struct Paths {
    pub config: PathBuf,
    pub database: PathBuf,
    pub known_hosts: PathBuf,
    /// Where the running copy notes the port it listens on, see `instance.rs`.
    pub instance: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let dirs =
            directories::ProjectDirs::from("", "", "gridwatch").context("cannot locate the user profile directory")?;
        std::fs::create_dir_all(dirs.config_dir())?;
        std::fs::create_dir_all(dirs.data_dir())?;
        Ok(Self {
            config: dirs.config_dir().join("config.json"),
            database: dirs.data_dir().join("history.sqlite"),
            known_hosts: dirs.data_dir().join("known_hosts"),
            instance: dirs.data_dir().join("instance.port"),
        })
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS samples (
    server_id TEXT NOT NULL, ts INTEGER NOT NULL,
    cpu REAL, cpu_user REAL, cpu_system REAL, cpu_iowait REAL, cpu_steal REAL,
    load1 REAL, load5 REAL, load15 REAL,
    mem_used INTEGER, mem_total INTEGER, swap_used INTEGER,
    rx_bps REAL, tx_bps REAL, temp REAL, cores TEXT,
    disk_read REAL, disk_write REAL, disk_util REAL, temps TEXT, gpus TEXT, nets TEXT, mem_cached INTEGER,
    PRIMARY KEY (server_id, ts)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS container_samples (
    server_id TEXT NOT NULL, ts INTEGER NOT NULL, name TEXT NOT NULL,
    cpu REAL, mem_used INTEGER,
    PRIMARY KEY (server_id, ts, name)
) WITHOUT ROWID;
";

/// Columns added after the first release; databases created before get them on open.
const ADDED_COLUMNS: [&str; 7] = [
    "disk_read REAL",
    "disk_write REAL",
    "disk_util REAL",
    "temps TEXT",
    "gpus TEXT",
    "nets TEXT",
    "mem_cached INTEGER",
];

/// Number of points a chart gets at most; older data is averaged into buckets.
const MAX_POINTS: i64 = 720;

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        let mut existing = Vec::new();
        conn.pragma(None, "table_info", "samples", |row| {
            existing.push(row.get::<_, String>(1)?);
            Ok(())
        })?;
        for column in ADDED_COLUMNS {
            let name = column.split(' ').next().unwrap_or(column);
            if !existing.iter().any(|e| e == name) {
                conn.execute_batch(&format!("ALTER TABLE samples ADD COLUMN {column}"))?;
            }
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn insert(&self, server_id: &str, p: &HistPoint, docker: &DockerState) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO samples VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
            params![
                server_id,
                p.ts,
                p.cpu.total,
                p.cpu.user,
                p.cpu.system,
                p.cpu.iowait,
                p.cpu.steal,
                p.load[0],
                p.load[1],
                p.load[2],
                p.mem_used as i64,
                p.mem_total as i64,
                p.swap_used as i64,
                p.rx_bps,
                p.tx_bps,
                p.temp,
                serde_json::to_string(&p.cores)?,
                p.disk_read_bps,
                p.disk_write_bps,
                p.disk_util,
                serde_json::to_string(&p.temps)?,
                serde_json::to_string(&p.gpus)?,
                serde_json::to_string(&p.nets)?,
                p.mem_cached as i64,
            ],
        )?;
        if let DockerState::Ok(list) = docker {
            for c in list.iter().filter(|c| c.has_stats) {
                tx.execute(
                    "INSERT OR REPLACE INTO container_samples VALUES (?1,?2,?3,?4,?5)",
                    params![server_id, p.ts, c.name, c.cpu, c.mem_used as i64],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// History since `since`, averaged into at most `MAX_POINTS` buckets.
    pub fn load_history(&self, server_id: &str, since: i64, now: i64) -> Result<VecDeque<HistPoint>> {
        let bucket = ((now - since) / MAX_POINTS).max(1);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT ts, cpu, cpu_user, cpu_system, cpu_iowait, cpu_steal, load1, load5, load15,
                    mem_used, mem_total, swap_used, rx_bps, tx_bps, temp, cores,
                    disk_read, disk_write, disk_util, temps, gpus, nets, mem_cached
             FROM samples WHERE server_id = ?1 AND ts >= ?2 ORDER BY ts",
        )?;
        let rows = stmt.query_map(params![server_id, since], |r| {
            let cores: String = r.get(15)?;
            Ok(HistPoint {
                ts: r.get(0)?,
                cpu: CpuUsage {
                    total: r.get(1)?,
                    user: r.get(2)?,
                    system: r.get(3)?,
                    iowait: r.get(4)?,
                    steal: r.get(5)?,
                },
                load: [r.get(6)?, r.get(7)?, r.get(8)?],
                mem_used: r.get::<_, i64>(9)? as u64,
                mem_total: r.get::<_, i64>(10)? as u64,
                swap_used: r.get::<_, i64>(11)? as u64,
                rx_bps: r.get(12)?,
                tx_bps: r.get(13)?,
                temp: r.get(14)?,
                cores: serde_json::from_str(&cores).unwrap_or_default(),
                // Rows written by older versions have no values here.
                disk_read_bps: r.get::<_, Option<f64>>(16)?.unwrap_or(0.0),
                disk_write_bps: r.get::<_, Option<f64>>(17)?.unwrap_or(0.0),
                disk_util: r.get(18)?,
                temps: json(r.get(19)?),
                gpus: json(r.get(20)?),
                nets: json(r.get(21)?),
                mem_cached: r.get::<_, Option<i64>>(22)?.unwrap_or(0) as u64,
            })
        })?;

        let mut out = VecDeque::new();
        let mut group: Vec<HistPoint> = Vec::new();
        for row in rows {
            let p = row?;
            if group.first().is_some_and(|g| g.ts / bucket != p.ts / bucket) {
                out.push_back(average(&group));
                group.clear();
            }
            group.push(p);
        }
        if !group.is_empty() {
            out.push_back(average(&group));
        }
        Ok(out)
    }

    pub fn load_container_history(
        &self,
        server_id: &str,
        since: i64,
        now: i64,
    ) -> Result<HashMap<String, VecDeque<ContainerPoint>>> {
        let bucket = ((now - since) / MAX_POINTS).max(1);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, (ts / ?3) * ?3 AS b, AVG(cpu), AVG(mem_used)
             FROM container_samples WHERE server_id = ?1 AND ts >= ?2
             GROUP BY name, b ORDER BY b",
        )?;
        let rows = stmt.query_map(params![server_id, since, bucket], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f32>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        let mut out: HashMap<String, VecDeque<ContainerPoint>> = HashMap::new();
        for row in rows {
            let (name, ts, cpu, mem) = row?;
            out.entry(name).or_default().push_back((ts, cpu, mem as u64));
        }
        Ok(out)
    }

    pub fn prune(&self, retention_days: u32, now: i64) -> Result<()> {
        let cutoff = now - retention_days as i64 * 86400;
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM samples WHERE ts < ?1", params![cutoff])?;
        conn.execute("DELETE FROM container_samples WHERE ts < ?1", params![cutoff])?;
        Ok(())
    }

    pub fn delete_server(&self, server_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM samples WHERE server_id = ?1", params![server_id])?;
        conn.execute("DELETE FROM container_samples WHERE server_id = ?1", params![server_id])?;
        Ok(())
    }
}

fn json<T: serde::de::DeserializeOwned + Default>(text: Option<String>) -> T {
    text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn average(group: &[HistPoint]) -> HistPoint {
    if group.len() == 1 {
        return group[0].clone();
    }
    let n = group.len() as f32;
    let mean = |f: &dyn Fn(&HistPoint) -> f32| group.iter().map(f).sum::<f32>() / n;
    let mean64 = |f: &dyn Fn(&HistPoint) -> f64| group.iter().map(f).sum::<f64>() / n as f64;
    let last = &group[group.len() - 1];
    let temps: Vec<f32> = group.iter().filter_map(|p| p.temp).collect();
    let utils: Vec<f32> = group.iter().filter_map(|p| p.disk_util).collect();
    // Per chip and per GPU: sum and count, in order of first appearance.
    let mut chips: Vec<(String, f32, u32)> = Vec::new();
    let mut gpus: Vec<(f32, f64, u32)> = Vec::new();
    let mut nets: Vec<(String, f64, f64, u32)> = Vec::new();
    for p in group {
        for (name, rx, tx) in &p.nets {
            match nets.iter_mut().find(|n| n.0 == *name) {
                Some(n) => (n.1, n.2, n.3) = (n.1 + rx, n.2 + tx, n.3 + 1),
                None => nets.push((name.clone(), *rx, *tx, 1)),
            }
        }
        for (chip, temp) in &p.temps {
            match chips.iter_mut().find(|c| c.0 == *chip) {
                Some(c) => (c.1, c.2) = (c.1 + temp, c.2 + 1),
                None => chips.push((chip.clone(), *temp, 1)),
            }
        }
        for (i, (util, mem)) in p.gpus.iter().enumerate() {
            if gpus.len() <= i {
                gpus.push((0.0, 0.0, 0));
            }
            gpus[i] = (gpus[i].0 + util, gpus[i].1 + *mem as f64, gpus[i].2 + 1);
        }
    }
    let mut cores = BTreeMap::new();
    for p in group {
        for (i, v) in p.cores.iter().enumerate() {
            let e = cores.entry(i).or_insert((0.0f32, 0u32));
            e.0 += v;
            e.1 += 1;
        }
    }
    HistPoint {
        ts: group[group.len() / 2].ts,
        cpu: CpuUsage {
            total: mean(&|p| p.cpu.total),
            user: mean(&|p| p.cpu.user),
            system: mean(&|p| p.cpu.system),
            iowait: mean(&|p| p.cpu.iowait),
            steal: mean(&|p| p.cpu.steal),
        },
        load: [mean(&|p| p.load[0]), mean(&|p| p.load[1]), mean(&|p| p.load[2])],
        mem_used: mean64(&|p| p.mem_used as f64) as u64,
        mem_cached: mean64(&|p| p.mem_cached as f64) as u64,
        mem_total: last.mem_total,
        swap_used: mean64(&|p| p.swap_used as f64) as u64,
        rx_bps: mean64(&|p| p.rx_bps),
        tx_bps: mean64(&|p| p.tx_bps),
        temp: (!temps.is_empty()).then(|| temps.iter().sum::<f32>() / temps.len() as f32),
        cores: cores.into_values().map(|(sum, count)| sum / count as f32).collect(),
        disk_read_bps: mean64(&|p| p.disk_read_bps),
        disk_write_bps: mean64(&|p| p.disk_write_bps),
        disk_util: (!utils.is_empty()).then(|| utils.iter().sum::<f32>() / utils.len() as f32),
        temps: chips
            .into_iter()
            .map(|(chip, sum, count)| (chip, sum / count as f32))
            .collect(),
        gpus: gpus
            .into_iter()
            .map(|(util, mem, count)| (util / count as f32, (mem / count as f64) as u64))
            .collect(),
        nets: nets
            .into_iter()
            .map(|(name, rx, tx, count)| (name, rx / count as f64, tx / count as f64))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(ts: i64, cpu: f32) -> HistPoint {
        HistPoint {
            ts,
            cpu: CpuUsage {
                total: cpu,
                ..Default::default()
            },
            mem_used: 100,
            mem_total: 200,
            cores: vec![cpu, 0.0],
            temp: Some(50.0),
            disk_read_bps: cpu as f64 * 100.0,
            disk_util: Some(cpu),
            temps: vec![("k10temp".into(), 50.0 + cpu), ("nvme".into(), 40.0)],
            gpus: vec![(cpu * 2.0, 1000)],
            nets: vec![("eth0".into(), cpu as f64, 8.0)],
            ..Default::default()
        }
    }

    #[test]
    fn upgrades_a_database_from_the_first_release() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE samples (
                server_id TEXT NOT NULL, ts INTEGER NOT NULL,
                cpu REAL, cpu_user REAL, cpu_system REAL, cpu_iowait REAL, cpu_steal REAL,
                load1 REAL, load5 REAL, load15 REAL,
                mem_used INTEGER, mem_total INTEGER, swap_used INTEGER,
                rx_bps REAL, tx_bps REAL, temp REAL, cores TEXT,
                PRIMARY KEY (server_id, ts)
            ) WITHOUT ROWID;
            INSERT INTO samples VALUES ('a', 1000, 12.5, 10, 2.5, 0, 0, 1, 1, 1, 100, 200, 0, 0, 0, 40, '[12.5]');",
        )
        .unwrap();
        let store = Store::init(conn).unwrap();
        store.insert("a", &point(1001, 4.0), &DockerState::Unknown).unwrap();
        let h = store.load_history("a", 1000, 1010).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].cpu.total, h[0].disk_read_bps, h[0].disk_util), (12.5, 0.0, None));
        assert!(h[0].temps.is_empty() && h[0].gpus.is_empty());
        assert_eq!(h[1].disk_util, Some(4.0));
    }

    #[test]
    fn roundtrip_bucketing_and_pruning() {
        let store = Store::in_memory().unwrap();
        let docker = DockerState::Ok(vec![Container {
            name: "web".into(),
            has_stats: true,
            cpu: 10.0,
            mem_used: 4096,
            ..Default::default()
        }]);
        for i in 0..10 {
            store.insert("a", &point(1000 + i, i as f32), &docker).unwrap();
        }
        store.insert("b", &point(1005, 99.0), &DockerState::Unknown).unwrap();

        // Short range: one bucket per second, nothing merged.
        let h = store.load_history("a", 1000, 1010).unwrap();
        assert_eq!(h.len(), 10);
        assert_eq!(h[3].cpu.total, 3.0);
        assert_eq!(h[3].cores, vec![3.0, 0.0]);
        assert_eq!(h[3].temp, Some(50.0));
        assert_eq!((h[3].disk_read_bps, h[3].disk_util), (300.0, Some(3.0)));
        assert_eq!(
            h[3].temps,
            vec![("k10temp".to_string(), 53.0), ("nvme".to_string(), 40.0)]
        );
        assert_eq!(h[3].gpus, vec![(6.0, 1000)]);

        // Long range: 2-second buckets average neighbouring samples.
        let h = store.load_history("a", 1000, 1000 + 2 * MAX_POINTS).unwrap();
        assert_eq!(h.len(), 5);
        assert_eq!(h[0].cpu.total, 0.5);
        assert_eq!((h[0].disk_read_bps, h[0].disk_util), (50.0, Some(0.5)));
        assert_eq!(h[0].temps[0], ("k10temp".to_string(), 50.5));
        assert_eq!(h[0].gpus, vec![(1.0, 1000)]);
        assert_eq!(h[0].nets, vec![("eth0".to_string(), 0.5, 8.0)]);

        let c = store.load_container_history("a", 0, 1010).unwrap();
        assert_eq!(c["web"].len(), 10);
        assert_eq!(c["web"][0], (1000, 10.0, 4096));

        store.prune(1, 1005 + 86400).unwrap();
        assert_eq!(store.load_history("a", 0, 1010).unwrap().len(), 5);
        store.delete_server("a").unwrap();
        assert!(store.load_history("a", 0, 1010).unwrap().is_empty());
        assert_eq!(store.load_history("b", 0, 1010).unwrap().len(), 1);
    }
}
