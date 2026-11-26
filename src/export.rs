//! A copy of the history in an InfluxDB, for Grafana or as a backup.
//!
//! A thread of its own reads the samples from the local database and sends what is new,
//! server by server, in InfluxDB's line protocol. What was sent is remembered per server
//! and target database, so nothing goes missing when the database cannot be reached for
//! a while, and the stored history goes over as well when a target is set up.

use crate::store::{Influx, InfluxVersion, Store};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Samples of one server per request. A sample becomes a few dozen lines: one for the
/// machine, one per core, sensor, GPU, interface and container.
const BATCH: usize = 300;
/// How often to look for new samples.
const PERIOD: Duration = Duration::from_secs(10);
/// How long to wait after a failure before trying again.
const RETRY: Duration = Duration::from_secs(60);
/// Changes to the settings arrive while the user types; send only once they stop.
const SETTLE: Duration = Duration::from_secs(3);

/// What the export thread works with; the window hands it over whenever it changes.
#[derive(Clone, Debug, Default, PartialEq)]
struct Settings {
    config: Influx,
    /// Password (1.x) or token (2.x).
    secret: String,
    /// Id and name of each server.
    servers: Vec<(String, String)>,
}

/// How the export goes, for the settings window.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// Lines written since gridwatch started.
    pub lines: u64,
    /// When the last request went through.
    pub last_ok: Option<i64>,
    /// Why the last request failed; cleared by the next one that goes through.
    pub error: Option<String>,
    /// Older samples are still being sent.
    pub catching_up: bool,
}

pub struct Exporter {
    settings: Arc<Mutex<(u64, Settings)>>,
    status: Arc<Mutex<Status>>,
    wake: mpsc::Sender<()>,
}

impl Exporter {
    pub fn start(store: Arc<Store>) -> Self {
        let settings: Arc<Mutex<(u64, Settings)>> = Arc::default();
        let status: Arc<Mutex<Status>> = Arc::default();
        let (wake, woken) = mpsc::channel();
        {
            let (settings, status) = (settings.clone(), status.clone());
            std::thread::Builder::new()
                .name("export".into())
                .spawn(move || run(&store, &settings, &status, &woken))
                .expect("cannot start the export thread");
        }
        Self { settings, status, wake }
    }

    /// Hand over the current settings; cheap when nothing changed.
    pub fn update(&self, config: &Influx, secret: &str, servers: impl Iterator<Item = (String, String)>) {
        let next = Settings {
            config: config.clone(),
            secret: secret.to_string(),
            servers: servers.collect(),
        };
        let mut current = self.settings.lock().unwrap();
        if current.1 != next {
            current.0 += 1;
            current.1 = next;
            drop(current);
            let _ = self.wake.send(());
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}

fn run(store: &Store, settings: &Mutex<(u64, Settings)>, status: &Mutex<Status>, woken: &mpsc::Receiver<()>) {
    loop {
        let (generation, current) = settings.lock().unwrap().clone();
        let wait = if current.config.enabled && !current.config.url.trim().is_empty() {
            let changed = || settings.lock().unwrap().0 != generation;
            match send_new(store, &current, status, changed) {
                Ok(()) => PERIOD,
                Err(e) => {
                    let mut status = status.lock().unwrap();
                    status.error = Some(e);
                    status.catching_up = false;
                    RETRY
                }
            }
        } else {
            *status.lock().unwrap() = Status::default();
            Duration::from_secs(3600)
        };
        match woken.recv_timeout(wait) {
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
            // The settings changed: wait until the user is done with them.
            Ok(()) => loop {
                match woken.recv_timeout(SETTLE) {
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            },
        }
    }
}

/// Send every sample that has not been sent yet. Stops early when the settings change.
fn send_new(
    store: &Store,
    settings: &Settings,
    status: &Mutex<Status>,
    changed: impl Fn() -> bool,
) -> Result<(), String> {
    let target = settings.config.target();
    let now = crate::now_ts();
    for (id, name) in &settings.servers {
        let mut after = match store.export_cursor(&target, id).map_err(|e| e.to_string())? {
            Some(ts) => ts,
            None => settings.config.history.start(now),
        };
        loop {
            if changed() {
                return Ok(());
            }
            let points = store.samples_after(id, after, BATCH).map_err(|e| e.to_string())?;
            let Some(last) = points.last().map(|p| p.ts) else {
                break;
            };
            let containers = store
                .container_samples_between(id, after, last)
                .map_err(|e| e.to_string())?;
            let mut body = String::new();
            let lines = write_lines(&mut body, id, name, &points, &containers);
            post(&settings.config, &settings.secret, &body)?;
            store.set_export_cursor(&target, id, last).map_err(|e| e.to_string())?;
            after = last;
            let mut status = status.lock().unwrap();
            status.lines += lines;
            status.last_ok = Some(crate::now_ts());
            status.error = None;
            status.catching_up = points.len() == BATCH;
            if points.len() < BATCH {
                break;
            }
        }
    }
    status.lock().unwrap().catching_up = false;
    Ok(())
}

/// Write the samples as lines of InfluxDB's line protocol and return how many.
///
/// Every value is written as a float: InfluxDB keeps the type of a field from its first
/// value and refuses others, and a value that is whole now may not be next time.
fn write_lines(
    out: &mut String,
    server_id: &str,
    server: &str,
    points: &[crate::model::HistPoint],
    containers: &[(i64, String, f32, u64)],
) -> u64 {
    let mut count = 0;
    let base = [("host", server), ("server_id", server_id)];
    let mut line = |measurement: &str, extra: Option<(&str, &str)>, fields: &[(&str, Option<f64>)], ts: i64| {
        let fields: Vec<_> = fields
            .iter()
            .filter_map(|(k, v)| v.filter(|v| v.is_finite()).map(|v| (k, v)))
            .collect();
        if fields.is_empty() {
            return;
        }
        out.push_str(&escape(measurement, false));
        for (k, v) in base.iter().copied().chain(extra) {
            // InfluxDB does not take empty tag values.
            if !v.is_empty() {
                let _ = write!(out, ",{}={}", escape(k, true), escape(v, true));
            }
        }
        for (i, (k, v)) in fields.iter().enumerate() {
            let _ = write!(out, "{}{}={v}", if i == 0 { ' ' } else { ',' }, escape(k, true));
        }
        let _ = writeln!(out, " {ts}");
        count += 1;
    };
    for p in points {
        let f = |v: f32| Some(v as f64);
        line(
            "gridwatch_system",
            None,
            &[
                ("cpu", f(p.cpu.total)),
                ("cpu_user", f(p.cpu.user)),
                ("cpu_system", f(p.cpu.system)),
                ("cpu_iowait", f(p.cpu.iowait)),
                ("cpu_steal", f(p.cpu.steal)),
                ("load1", f(p.load[0])),
                ("load5", f(p.load[1])),
                ("load15", f(p.load[2])),
                ("mem_used", Some(p.mem_used as f64)),
                ("mem_cached", Some(p.mem_cached as f64)),
                ("mem_total", Some(p.mem_total as f64)),
                ("swap_used", Some(p.swap_used as f64)),
                ("net_rx_bps", Some(p.rx_bps)),
                ("net_tx_bps", Some(p.tx_bps)),
                ("disk_read_bps", Some(p.disk_read_bps)),
                ("disk_write_bps", Some(p.disk_write_bps)),
                ("disk_util", p.disk_util.map(f64::from)),
                ("temp", p.temp.map(f64::from)),
            ],
            p.ts,
        );
        for (i, usage) in p.cores.iter().enumerate() {
            line(
                "gridwatch_core",
                Some(("core", &i.to_string())),
                &[("usage", f(*usage))],
                p.ts,
            );
        }
        for (chip, temp) in &p.temps {
            line("gridwatch_temp", Some(("chip", chip)), &[("temp", f(*temp))], p.ts);
        }
        for (i, (util, mem)) in p.gpus.iter().enumerate() {
            line(
                "gridwatch_gpu",
                Some(("gpu", &i.to_string())),
                &[("util", f(*util)), ("mem_used", Some(*mem as f64))],
                p.ts,
            );
        }
        for (iface, rx, tx) in &p.nets {
            line(
                "gridwatch_net",
                Some(("iface", iface)),
                &[("rx_bps", Some(*rx)), ("tx_bps", Some(*tx))],
                p.ts,
            );
        }
    }
    for (ts, name, cpu, mem) in containers {
        line(
            "gridwatch_container",
            Some(("container", name)),
            &[("cpu", Some(*cpu as f64)), ("mem_used", Some(*mem as f64))],
            *ts,
        );
    }
    count
}

/// Escape a name for the line protocol: measurements need commas and spaces escaped,
/// tag keys, tag values and field keys also the equals sign.
fn escape(text: &str, tag: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            ',' | ' ' => out.push('\\'),
            '=' if tag => out.push('\\'),
            '\n' | '\r' => {
                out.push(' ');
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    out
}

/// Send lines to the database, compressed.
fn post(config: &Influx, secret: &str, body: &str) -> Result<(), String> {
    let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
    gzip.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    let compressed = gzip.finish().map_err(|e| e.to_string())?;

    let base = config.url.trim().trim_end_matches('/');
    let url = match config.version {
        InfluxVersion::V1 => format!("{base}/write?db={}&precision=s", encode(config.database.trim())),
        InfluxVersion::V2 => format!(
            "{base}/api/v2/write?org={}&bucket={}&precision=s",
            encode(config.org.trim()),
            encode(config.database.trim())
        ),
    };
    let mut request = ureq::post(&url)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Encoding", "gzip")
        .header("User-Agent", concat!("gridwatch/", env!("CARGO_PKG_VERSION")));
    match config.version {
        InfluxVersion::V1 if !config.user.trim().is_empty() => {
            let login = format!("{}:{secret}", config.user.trim());
            request = request.header("Authorization", format!("Basic {}", base64(login.as_bytes())));
        }
        InfluxVersion::V2 => request = request.header("Authorization", format!("Token {}", secret.trim())),
        InfluxVersion::V1 => {}
    }
    let mut response = request.send(&compressed[..]).map_err(|e| format!("{base}: {e}"))?;
    let code = response.status().as_u16();
    if (200..300).contains(&code) {
        return Ok(());
    }
    let text = response.body_mut().read_to_string().unwrap_or_default();
    Err(explain(code, &text))
}

/// A short message from an error answer. Both versions of InfluxDB answer with JSON that
/// carries the reason under `error` or `message`.
fn explain(code: u16, text: &str) -> String {
    let reason = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| {
            v.get("error")
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| text.trim().chars().take(200).collect());
    let hint = match code {
        401 | 403 => " (check user and password or token)",
        404 => " (check the address, the database or bucket, and the InfluxDB version)",
        _ => "",
    };
    if reason.is_empty() {
        format!("HTTP {code}{hint}")
    } else {
        format!("HTTP {code}: {reason}{hint}")
    }
}

/// Percent-encoding for a query parameter.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CpuUsage, HistPoint};

    #[test]
    fn writes_line_protocol() {
        let point = HistPoint {
            ts: 1_700_000_000,
            cpu: CpuUsage {
                total: 12.5,
                ..Default::default()
            },
            mem_used: 1024,
            cores: vec![10.0, 15.0],
            temps: vec![("k10temp".into(), 55.0)],
            nets: vec![("eth 0".into(), 100.0, 200.0)],
            temp: None,
            ..Default::default()
        };
        let containers = [(1_700_000_000, "web,1".to_string(), 3.0, 2048)];
        let mut out = String::new();
        let count = write_lines(&mut out, "s1", "my server", &[point], &containers);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(count, 6);
        assert_eq!(lines.len(), 6);
        assert!(lines[0].starts_with("gridwatch_system,host=my\\ server,server_id=s1 cpu=12.5,"));
        assert!(lines[0].contains(",mem_used=1024,"));
        assert!(!lines[0].contains("temp="), "a missing value is left out");
        assert!(lines[0].ends_with(" 1700000000"));
        assert_eq!(
            lines[1],
            "gridwatch_core,host=my\\ server,server_id=s1,core=0 usage=10 1700000000"
        );
        assert_eq!(
            lines[3],
            "gridwatch_temp,host=my\\ server,server_id=s1,chip=k10temp temp=55 1700000000"
        );
        assert_eq!(
            lines[4],
            "gridwatch_net,host=my\\ server,server_id=s1,iface=eth\\ 0 rx_bps=100,tx_bps=200 1700000000"
        );
        assert_eq!(
            lines[5],
            "gridwatch_container,host=my\\ server,server_id=s1,container=web\\,1 cpu=3,mem_used=2048 1700000000"
        );
    }

    #[test]
    fn escapes_and_encodes() {
        assert_eq!(escape("a=b c,d", true), "a\\=b\\ c\\,d");
        assert_eq!(escape("a=b", false), "a=b");
        assert_eq!(encode("my db/ä"), "my%20db%2F%C3%A4");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
    }

    #[test]
    fn explains_errors() {
        assert_eq!(
            explain(404, r#"{"error":"database not found: \"x\""}"#),
            "HTTP 404: database not found: \"x\" (check the address, the database or bucket, and the InfluxDB version)"
        );
        assert_eq!(
            explain(401, r#"{"code":"unauthorized","message":"unauthorized access"}"#),
            "HTTP 401: unauthorized access (check user and password or token)"
        );
    }

    /// Sends the history of a database in memory to a little server on this computer and
    /// checks what arrives, and that a second round sends nothing again.
    #[test]
    fn sends_new_samples_once() {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        {
            let received = received.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let (mut head, mut length) = (String::new(), 0);
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap();
                        }
                        head.push_str(&line);
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let mut text = String::new();
                    flate2::read::GzDecoder::new(&body[..])
                        .read_to_string(&mut text)
                        .unwrap();
                    received.lock().unwrap().push((head, text));
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .unwrap();
                }
            });
        }

        let store = Store::in_memory().unwrap();
        let now = crate::now_ts();
        for ts in [now - 7200, now - 60, now - 30] {
            let point = HistPoint {
                ts,
                mem_total: 100,
                ..Default::default()
            };
            store
                .insert("s1", &point, &crate::model::DockerState::default())
                .unwrap();
        }
        let settings = Settings {
            config: Influx {
                enabled: true,
                url: format!("http://127.0.0.1:{port}/"),
                database: "grid watch".into(),
                user: "me".into(),
                ..Default::default()
            },
            secret: "pw".into(),
            servers: vec![("s1".into(), "one".into())],
        };
        let status = Mutex::new(Status::default());
        send_new(&store, &settings, &status, || false).unwrap();
        send_new(&store, &settings, &status, || false).unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1, "the second round has nothing new");
        let (head, body) = &received[0];
        assert!(head.starts_with("POST /write?db=grid%20watch&precision=s "), "{head}");
        assert!(head.contains("bWU6cHc="), "basic login of me:pw");
        assert_eq!(body.lines().count(), 3, "all samples of the last day");
        assert_eq!(status.lock().unwrap().lines, 3);
    }
}
