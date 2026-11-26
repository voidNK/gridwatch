//! Local history in SQLite, plus the JSON config file.

use crate::model::*;
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
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
    /// A copy of the history in an InfluxDB.
    #[serde(default)]
    pub influx: Influx,
}

/// Where to send the history, see `export.rs`. The password or token is kept in the keychain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Influx {
    pub enabled: bool,
    /// e.g. `http://homeassistant.local:8086`
    pub url: String,
    pub version: InfluxVersion,
    /// The database (1.x) or the bucket (2.x).
    pub database: String,
    /// 2.x only.
    pub org: String,
    /// 1.x only; empty when the database needs no login.
    pub user: String,
    /// How far back to start for a server that was not sent to this database before.
    pub history: InfluxHistory,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InfluxVersion {
    /// InfluxDB 1.x, also the Home Assistant add-on: database, user and password.
    #[default]
    V1,
    /// InfluxDB 2.x and later: organisation, bucket and token.
    V2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InfluxHistory {
    /// Only what is measured from now on.
    Nothing,
    #[default]
    Day,
    /// Everything that is stored.
    All,
}

impl InfluxHistory {
    pub const ALL: [InfluxHistory; 3] = [InfluxHistory::Nothing, InfluxHistory::Day, InfluxHistory::All];

    pub fn label(self) -> &'static str {
        match self {
            InfluxHistory::Nothing => "nothing older",
            InfluxHistory::Day => "the last day",
            InfluxHistory::All => "all stored history",
        }
    }

    /// The moment to start after, for a server sent for the first time.
    pub fn start(self, now: i64) -> i64 {
        match self {
            InfluxHistory::Nothing => now,
            InfluxHistory::Day => now - 86400,
            InfluxHistory::All => 0,
        }
    }
}

impl Default for Influx {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            version: InfluxVersion::V1,
            database: "gridwatch".to_string(),
            org: String::new(),
            user: String::new(),
            history: InfluxHistory::Day,
        }
    }
}

impl Influx {
    /// Names the database the data goes to. What was sent is remembered per target, so
    /// another address or database starts over and gets the history as well.
    pub fn target(&self) -> String {
        format!(
            "{}|{:?}|{}|{}",
            self.url.trim().trim_end_matches('/'),
            self.version,
            self.database.trim(),
            self.org.trim()
        )
    }
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
            influx: Influx::default(),
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
    /// A second connection that only reads. Loading a long range takes a while and must
    /// not hold up the collectors, which write through `conn`. Not there for a database in memory.
    reader: Option<Mutex<Connection>>,
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
CREATE TABLE IF NOT EXISTS export_cursors (
    target TEXT NOT NULL, server_id TEXT NOT NULL, ts INTEGER NOT NULL,
    PRIMARY KEY (target, server_id)
) WITHOUT ROWID;
";

/// The columns of a history point, in the order [`point_from_row`] reads them.
const POINT_COLUMNS: &str = "ts, cpu, cpu_user, cpu_system, cpu_iowait, cpu_steal, load1, load5, load15,
    mem_used, mem_total, swap_used, rx_bps, tx_bps, temp, cores,
    disk_read, disk_write, disk_util, temps, gpus, nets, mem_cached";

fn point_from_row(r: &rusqlite::Row) -> rusqlite::Result<HistPoint> {
    let number = |i: usize| r.get::<_, Option<f64>>(i).map(|v| v.unwrap_or(0.0));
    Ok(HistPoint {
        ts: r.get(0)?,
        cpu: CpuUsage {
            total: number(1)? as f32,
            user: number(2)? as f32,
            system: number(3)? as f32,
            iowait: number(4)? as f32,
            steal: number(5)? as f32,
        },
        load: [number(6)? as f32, number(7)? as f32, number(8)? as f32],
        mem_used: number(9)? as u64,
        mem_total: r.get::<_, Option<i64>>(10)?.unwrap_or(0) as u64,
        swap_used: number(11)? as u64,
        rx_bps: number(12)?,
        tx_bps: number(13)?,
        temp: r.get::<_, Option<f64>>(14)?.map(|t| t as f32),
        cores: json(r.get(15)?),
        // Rows written by older versions have no values in the columns from here on.
        disk_read_bps: number(16)?,
        disk_write_bps: number(17)?,
        disk_util: r.get::<_, Option<f64>>(18)?.map(|u| u as f32),
        temps: json(r.get(19)?),
        gpus: json(r.get(20)?),
        nets: json(r.get(21)?),
        mem_cached: number(22)? as u64,
    })
}

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
const MAX_POINTS: i64 = HISTORY_POINTS;

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let mut store = Self::init(conn)?;
        let read_only = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        store.reader = Connection::open_with_flags(path, read_only).ok().map(Mutex::new);
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn read(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.reader.as_ref().unwrap_or(&self.conn).lock().unwrap()
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
        Ok(Self {
            conn: Mutex::new(conn),
            reader: None,
        })
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

    /// History since `since`, in at most `MAX_POINTS` points.
    ///
    /// The database does the averaging, one row per point comes back. The lists (cores,
    /// sensors, GPUs, interfaces) are stored as text and cannot be averaged there; each
    /// point takes them from its newest sample, which SQLite hands out for the columns
    /// next to a single `MAX()`.
    pub fn load_history(&self, server_id: &str, since: i64, now: i64) -> Result<VecDeque<HistPoint>> {
        let bucket = ((now - since) / MAX_POINTS).max(1);
        let conn = self.read();
        let mut stmt = conn.prepare(
            "SELECT MAX(ts), AVG(cpu), AVG(cpu_user), AVG(cpu_system), AVG(cpu_iowait), AVG(cpu_steal),
                    AVG(load1), AVG(load5), AVG(load15), AVG(mem_used), mem_total, AVG(swap_used),
                    AVG(rx_bps), AVG(tx_bps), AVG(temp), cores,
                    AVG(disk_read), AVG(disk_write), AVG(disk_util), temps, gpus, nets, AVG(mem_cached)
             FROM samples WHERE server_id = ?1 AND ts >= ?2 GROUP BY ts / ?3 ORDER BY ts / ?3",
        )?;
        let rows = stmt.query_map(params![server_id, since, bucket], point_from_row)?;
        rows.collect::<rusqlite::Result<VecDeque<_>>>().map_err(Into::into)
    }

    /// Up to `limit` samples newer than `after`, as they were measured, oldest first.
    pub fn samples_after(&self, server_id: &str, after: i64, limit: usize) -> Result<Vec<HistPoint>> {
        let conn = self.read();
        let mut stmt = conn.prepare(&format!(
            "SELECT {POINT_COLUMNS} FROM samples WHERE server_id = ?1 AND ts > ?2 ORDER BY ts LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![server_id, after, limit as i64], point_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// The container samples with `after < ts <= until`: time, name, CPU and memory.
    pub fn container_samples_between(
        &self,
        server_id: &str,
        after: i64,
        until: i64,
    ) -> Result<Vec<(i64, String, f32, u64)>> {
        let conn = self.read();
        let mut stmt = conn.prepare(
            "SELECT ts, name, cpu, mem_used FROM container_samples
             WHERE server_id = ?1 AND ts > ?2 AND ts <= ?3 ORDER BY ts",
        )?;
        let rows = stmt.query_map(params![server_id, after, until], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<f32>>(2)?.unwrap_or(0.0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// The newest sample of the server that has been sent to `target`, if any was.
    pub fn export_cursor(&self, target: &str, server_id: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT ts FROM export_cursors WHERE target = ?1 AND server_id = ?2")?;
        let mut rows = stmt.query(params![target, server_id])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    pub fn set_export_cursor(&self, target: &str, server_id: &str, ts: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO export_cursors VALUES (?1, ?2, ?3)",
            params![target, server_id, ts],
        )?;
        Ok(())
    }

    /// CPU and memory of every container at the given moments: the timestamps of the
    /// points [`Store::load_history`] returned. One short lookup per point, where
    /// averaging millions of container samples would take seconds.
    pub fn load_container_history(
        &self,
        server_id: &str,
        moments: impl IntoIterator<Item = i64>,
    ) -> Result<HashMap<String, VecDeque<ContainerPoint>>> {
        let conn = self.read();
        let mut stmt =
            conn.prepare("SELECT name, cpu, mem_used FROM container_samples WHERE server_id = ?1 AND ts = ?2")?;
        let mut out: HashMap<String, VecDeque<ContainerPoint>> = HashMap::new();
        for ts in moments {
            let rows = stmt.query_map(params![server_id, ts], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f32>(1)?, r.get::<_, i64>(2)?))
            })?;
            for row in rows {
                let (name, cpu, mem) = row?;
                out.entry(name).or_default().push_back((ts, cpu, mem as u64));
            }
        }
        Ok(out)
    }

    /// The servers that have anything stored. Cheap: the primary key starts with the id.
    fn server_ids(conn: &Connection) -> Result<Vec<String>> {
        let mut stmt = conn
            .prepare("SELECT DISTINCT server_id FROM samples UNION SELECT DISTINCT server_id FROM container_samples")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(ids)
    }

    /// Delete what is older than the retention time. Done server by server, because the
    /// tables are ordered by server and time; a plain `ts < ?` would read every row.
    pub fn prune(&self, retention_days: u32, now: i64) -> Result<()> {
        let cutoff = now - retention_days as i64 * 86400;
        let conn = self.conn.lock().unwrap();
        for id in Self::server_ids(&conn)? {
            conn.execute(
                "DELETE FROM samples WHERE server_id = ?1 AND ts < ?2",
                params![id, cutoff],
            )?;
            conn.execute(
                "DELETE FROM container_samples WHERE server_id = ?1 AND ts < ?2",
                params![id, cutoff],
            )?;
        }
        Ok(())
    }

    /// Delete the history of servers that are not in the list any more, and return how
    /// many there were. They cannot be shown again, the data would only take up space.
    pub fn forget_unknown(&self, known: &[String]) -> Result<usize> {
        let unknown: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            Self::server_ids(&conn)?
                .into_iter()
                .filter(|id| !known.contains(id))
                .collect()
        };
        for id in &unknown {
            self.delete_server(id)?;
        }
        Ok(unknown.len())
    }

    pub fn delete_server(&self, server_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM samples WHERE server_id = ?1", params![server_id])?;
        conn.execute("DELETE FROM container_samples WHERE server_id = ?1", params![server_id])?;
        conn.execute("DELETE FROM export_cursors WHERE server_id = ?1", params![server_id])?;
        Ok(())
    }
}

fn json<T: serde::de::DeserializeOwned + Default>(text: Option<String>) -> T {
    text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
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
        // The lists of a point come from its newest sample.
        assert_eq!(h[0].ts, 1001);
        assert_eq!(h[0].cores, vec![1.0, 0.0]);
        assert_eq!(h[0].temps[0], ("k10temp".to_string(), 51.0));
        assert_eq!(h[0].gpus, vec![(2.0, 1000)]);
        assert_eq!(h[0].nets, vec![("eth0".to_string(), 1.0, 8.0)]);

        let c = store.load_container_history("a", 1000..1010).unwrap();
        assert_eq!(c["web"].len(), 10);
        assert_eq!(c["web"][0], (1000, 10.0, 4096));

        store.prune(1, 1005 + 86400).unwrap();
        assert_eq!(store.load_history("a", 0, 1010).unwrap().len(), 5);
        assert_eq!(store.forget_unknown(&["a".to_string(), "b".to_string()]).unwrap(), 0);
        assert_eq!(store.forget_unknown(&["b".to_string()]).unwrap(), 1);
        assert!(store.load_history("a", 0, 1010).unwrap().is_empty());
        assert_eq!(store.load_history("b", 0, 1010).unwrap().len(), 1);
    }
}

#[cfg(test)]
mod timing {
    use super::*;
    use std::time::Instant;

    /// Times the queries gridwatch runs against the real history database of this computer:
    /// `cargo test real_database_timing -- --ignored --nocapture`. Reads only.
    #[test]
    #[ignore]
    fn real_database_timing() {
        let paths = Paths::resolve().unwrap();
        let conn = Connection::open_with_flags(&paths.database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let count = |table: &str| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let started = Instant::now();
        println!(
            "samples: {} rows, container_samples: {} rows ({:?})",
            count("samples"),
            count("container_samples"),
            started.elapsed()
        );
        for sql in [
            "DELETE FROM samples WHERE ts < 1",
            "DELETE FROM container_samples WHERE ts < 1",
            "SELECT ts FROM samples WHERE server_id = 'x' AND ts >= 1 ORDER BY ts",
            "SELECT name, (ts / 5) * 5 AS b, AVG(cpu), AVG(mem_used) FROM container_samples WHERE server_id = 'x' AND ts >= 1 GROUP BY name, b ORDER BY b",
        ] {
            let plan: Vec<String> = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(3))
                .unwrap()
                .flatten()
                .collect();
            println!("plan for {:.60}: {}", sql, plan.join(" | "));
        }
        let servers: Vec<String> = conn
            .prepare("SELECT DISTINCT server_id FROM samples")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        drop(conn);
        let store = Store {
            conn: Mutex::new(
                Connection::open_with_flags(&paths.database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap(),
            ),
            reader: None,
        };
        let now = crate::now_ts();
        for (label, secs) in [("1 h", 3600), ("24 h", 86400), ("7 d", 604800)] {
            let started = Instant::now();
            let mut points = 0;
            for id in &servers {
                points += store.load_history(id, now - secs, now).unwrap().len();
            }
            let history = started.elapsed();
            let started = Instant::now();
            for id in &servers {
                let moments: Vec<i64> = store
                    .load_history(id, now - secs, now)
                    .unwrap()
                    .iter()
                    .map(|p| p.ts)
                    .collect();
                store.load_container_history(id, moments).unwrap();
            }
            println!(
                "range {label}: {} servers, {points} points, history {history:?}, containers {:?}",
                servers.len(),
                started.elapsed()
            );
        }
    }
}
