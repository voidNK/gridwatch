//! Tells the user about new trouble: a sound and a desktop notification.
//!
//! The collectors call [`Notifier::check`] after every poll, so alerts also arrive
//! while the window is minimised.

use crate::model::{ConnState, ServerState};
use crate::store::Notifications;
use crate::suggest::{self, Severity};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A problem that went away is remembered this long, so a value hovering
/// around its threshold does not alert again and again.
const FORGET_AFTER: Duration = Duration::from_secs(600);
/// Several servers reporting at the same moment share one sound.
const SOUND_GAP: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// Identifies the problem across polls, even when the numbers in its title change.
    pub key: String,
    pub severity: Severity,
    pub title: String,
}

/// Everything about a server that is worth an alert right now.
pub fn alerts(state: &ServerState) -> Vec<Alert> {
    let mut out = Vec::new();
    if let ConnState::Error(msg) = &state.conn {
        out.push(Alert {
            key: "offline".into(),
            severity: Severity::Critical,
            title: format!("Connection lost: {msg}"),
        });
    }
    for s in suggest::analyze(state) {
        if s.severity <= Severity::Warning {
            out.push(Alert {
                // "Disk / is 91% full" and "Disk / is 92% full" are the same problem.
                key: s.title.chars().filter(|c| !c.is_ascii_digit()).collect(),
                severity: s.severity,
                title: s.title,
            });
        }
    }
    out
}

/// Remembers which problems of one server the user has already been told about.
#[derive(Default)]
struct Tracker {
    seen: HashMap<String, (Severity, Instant)>,
    last_alert: Option<Instant>,
}

impl Tracker {
    /// Returns the alerts that are new, or more severe than when last reported.
    fn update(&mut self, current: &[Alert], now: Instant) -> Vec<Alert> {
        let mut new = Vec::new();
        for alert in current {
            let known = self
                .seen
                .get(&alert.key)
                .is_some_and(|(severity, _)| *severity <= alert.severity);
            if !known {
                new.push(alert.clone());
            }
            self.seen.insert(alert.key.clone(), (alert.severity, now));
        }
        self.seen
            .retain(|_, (_, last_seen)| now.duration_since(*last_seen) < FORGET_AFTER);
        new
    }
}

enum Job {
    Sound(Severity, f32),
    Desktop {
        summary: String,
        body: String,
        critical: bool,
    },
}

pub struct Notifier {
    settings: Mutex<Notifications>,
    trackers: Mutex<HashMap<String, Tracker>>,
    last_sound: Mutex<Option<Instant>>,
    jobs: mpsc::Sender<Job>,
}

impl Notifier {
    pub fn new(settings: Notifications) -> Self {
        let (jobs, queue) = mpsc::channel();
        // Audio devices and notification services can block, so they get their own thread.
        std::thread::Builder::new()
            .name("notify".into())
            .spawn(move || {
                for job in queue {
                    let result = match job {
                        Job::Sound(severity, volume) => play(severity, volume),
                        Job::Desktop {
                            summary,
                            body,
                            critical,
                        } => show(&summary, &body, critical),
                    };
                    if let Err(e) = result {
                        eprintln!("gridwatch: notification failed: {e}");
                    }
                }
            })
            .expect("cannot start the notification thread");
        Self {
            settings: Mutex::new(settings),
            trackers: Mutex::default(),
            last_sound: Mutex::default(),
            jobs,
        }
    }

    pub fn set(&self, settings: Notifications) {
        *self.settings.lock().unwrap() = settings;
    }

    pub fn forget(&self, server_id: &str) {
        self.trackers.lock().unwrap().remove(server_id);
    }

    /// Play the alert sound and show a notification even while alerts are switched off (settings dialog).
    pub fn test(&self, severity: Severity) {
        let settings = self.settings.lock().unwrap().clone();
        if settings.sound {
            let _ = self.jobs.send(Job::Sound(severity, settings.volume));
        }
        if settings.desktop {
            let _ = self.jobs.send(Job::Desktop {
                summary: "gridwatch test".into(),
                body: "This is what an alert looks like.".into(),
                critical: false,
            });
        }
    }

    /// Compare a server's state with what was already reported and alert about the difference.
    pub fn check(&self, server_id: &str, server_name: &str, state: &ServerState) {
        let settings = self.settings.lock().unwrap().clone();
        let wanted = |a: &Alert| {
            if a.key == "offline" {
                settings.offline
            } else {
                !settings.critical_only || a.severity == Severity::Critical
            }
        };
        let now = Instant::now();
        let current = alerts(state);
        let open: Vec<&Alert> = current.iter().filter(|a| wanted(a)).collect();

        let mut trackers = self.trackers.lock().unwrap();
        let tracker = trackers.entry(server_id.to_string()).or_default();
        // Tracking continues while muted, so switching alerts back on is not followed by old news.
        let new: Vec<Alert> = tracker
            .update(&current, now)
            .into_iter()
            .filter(|a| wanted(a))
            .collect();
        if !settings.enabled || open.is_empty() {
            return;
        }
        let repeat = Duration::from_secs(settings.repeat_minutes as u64 * 60);
        let reminder = new.is_empty()
            && settings.repeat_minutes > 0
            && tracker.last_alert.is_none_or(|at| now.duration_since(at) >= repeat);
        if new.is_empty() && !reminder {
            return;
        }
        tracker.last_alert = Some(now);
        drop(trackers);

        let list: Vec<&Alert> = if reminder { open } else { new.iter().collect() };
        let severity = list.iter().map(|a| a.severity).min().unwrap_or(Severity::Warning);
        if settings.desktop {
            let summary = match (reminder, list.len()) {
                (true, 1) => format!("{server_name}: still 1 open problem"),
                (true, n) => format!("{server_name}: still {n} open problems"),
                (false, 1) => format!("{server_name}: {}", severity.label().to_lowercase()),
                (false, n) => format!("{server_name}: {n} new problems"),
            };
            let mut lines: Vec<&str> = list.iter().take(4).map(|a| a.title.as_str()).collect();
            if list.len() > lines.len() {
                lines.push("…");
            }
            let _ = self.jobs.send(Job::Desktop {
                summary,
                body: lines.join("\n"),
                critical: severity == Severity::Critical,
            });
        }
        if settings.sound {
            let mut last = self.last_sound.lock().unwrap();
            if last.is_none_or(|at| now.duration_since(at) >= SOUND_GAP) {
                *last = Some(now);
                let _ = self.jobs.send(Job::Sound(severity, settings.volume));
            }
        }
    }
}

/// The alert tones are synthesised, so no sound files have to be shipped.
/// Returns mono samples: a rising two-note chime for warnings, three urgent beeps for critical.
fn tone(severity: Severity, volume: f32, rate: u32) -> Vec<f32> {
    // (frequency in Hz, start in seconds, length in seconds)
    let notes: &[(f32, f32, f32)] = if severity == Severity::Critical {
        &[(988.0, 0.0, 0.16), (988.0, 0.22, 0.16), (1319.0, 0.44, 0.38)]
    } else {
        &[(659.0, 0.0, 0.22), (880.0, 0.16, 0.42)]
    };
    let length = notes.iter().map(|n| n.1 + n.2).fold(0.0, f32::max);
    let mut samples = vec![0.0f32; (length * rate as f32) as usize];
    for &(freq, start, len) in notes {
        let first = (start * rate as f32) as usize;
        for i in 0..(len * rate as f32) as usize {
            let t = i as f32 / rate as f32;
            // Short attack and a smooth release avoid clicks.
            let envelope = (t / 0.006).min(1.0) * ((len - t) / 0.05).min(1.0) * (-3.0 * t / len).exp();
            let phase = std::f32::consts::TAU * freq * t;
            let wave = phase.sin() + 0.25 * (2.0 * phase).sin();
            if let Some(sample) = samples.get_mut(first + i) {
                *sample += wave * envelope * 0.3 * volume.clamp(0.0, 1.0);
            }
        }
    }
    samples
}

fn play(severity: Severity, volume: f32) -> Result<(), String> {
    use std::num::NonZero;
    const RATE: u32 = 44_100;
    let samples = tone(severity, volume, RATE);
    let length = Duration::from_secs_f32(samples.len() as f32 / RATE as f32);
    // Opened per sound: follows a changed default device and holds none while quiet.
    let mut sink = rodio::DeviceSinkBuilder::open_default_sink().map_err(|e| format!("no audio output: {e}"))?;
    sink.log_on_drop(false);
    let buffer = rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(RATE).unwrap(), samples);
    sink.mixer().add(buffer);
    std::thread::sleep(length + Duration::from_millis(250));
    Ok(())
}

/// Desktop notification: toast on Windows, Notification Center on macOS, D-Bus on Linux.
/// Always silent, the sound is ours and has its own switch.
fn show(summary: &str, body: &str, critical: bool) -> Result<(), String> {
    let mut n = notify_rust::Notification::new();
    n.appname("gridwatch").summary(summary).body(body);
    #[cfg(all(unix, not(target_os = "macos")))]
    n.urgency(if critical {
        notify_rust::Urgency::Critical
    } else {
        notify_rust::Urgency::Normal
    });
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let _ = critical;
    n.show().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(key: &str, severity: Severity) -> Alert {
        Alert {
            key: key.into(),
            severity,
            title: key.into(),
        }
    }

    #[test]
    fn reports_each_problem_once_and_escalations_again() {
        let mut t = Tracker::default();
        let start = Instant::now();
        let disk = alert("disk", Severity::Warning);
        assert_eq!(t.update(&[disk.clone()], start), vec![disk.clone()]);
        assert!(t.update(&[disk.clone()], start + Duration::from_secs(5)).is_empty());

        // A second problem is reported on its own.
        let load = alert("load", Severity::Warning);
        assert_eq!(
            t.update(&[disk.clone(), load.clone()], start + Duration::from_secs(10)),
            vec![load.clone()]
        );

        // Getting worse is news, getting better is not.
        let worse = alert("disk", Severity::Critical);
        assert_eq!(
            t.update(&[worse.clone(), load.clone()], start + Duration::from_secs(15)),
            vec![worse]
        );
        assert!(
            t.update(&[disk.clone(), load], start + Duration::from_secs(20))
                .is_empty()
        );

        // Gone briefly and back: still known. Gone for long: reported again.
        assert!(t.update(&[], start + Duration::from_secs(30)).is_empty());
        assert!(t.update(&[disk.clone()], start + Duration::from_secs(60)).is_empty());
        let later = start + Duration::from_secs(60) + FORGET_AFTER;
        assert!(t.update(&[], later).is_empty());
        assert_eq!(t.update(&[disk.clone()], later + Duration::from_secs(5)), vec![disk]);
    }

    #[test]
    fn alert_keys_ignore_changing_numbers() {
        let mut state = ServerState {
            latest: Some(crate::model::Sample {
                ts: 1000,
                cores: vec![10.0; 4],
                mem_total: 8 << 30,
                mem_used: 2 << 30,
                disks: vec![crate::model::Disk {
                    mount: "/".into(),
                    fstype: "ext4".into(),
                    total: 100,
                    used: 91,
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let before = alerts(&state);
        state.latest.as_mut().unwrap().disks[0].used = 93;
        state.conn = ConnState::Error("timeout".into());
        let after = alerts(&state);
        assert_eq!(before.len(), 1);
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].key, "offline");
        assert_eq!(before[0].key, after[1].key);
        assert_ne!(before[0].title, after[1].title);
    }

    #[test]
    fn tones_stay_within_range() {
        for severity in [Severity::Warning, Severity::Critical] {
            let samples = tone(severity, 1.0, 44_100);
            assert!(samples.len() > 20_000);
            assert!(samples.iter().all(|s| s.abs() <= 1.0));
            assert!(samples.last().unwrap().abs() < 0.01);
        }
    }
}
