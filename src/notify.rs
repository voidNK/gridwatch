//! Tells the user about new trouble: a sound and a desktop notification.
//!
//! The collectors call [`Notifier::check`] after every poll, so alerts also arrive
//! while the window is minimised.

use crate::model::{ConnState, ServerState};
use crate::store::{Notifications, Tone};
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
    Sound(Tone, f32),
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
    /// When each server last got a new critical problem, for the red frame in the server list.
    critical_hits: Mutex<HashMap<String, Instant>>,
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
                        Job::Sound(tone, volume) => play(tone, volume),
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
            critical_hits: Mutex::default(),
            jobs,
        }
    }

    pub fn set(&self, settings: Notifications) {
        *self.settings.lock().unwrap() = settings;
    }

    /// How long ago a critical problem newly appeared on this server, if one ever did.
    pub fn critical_hit(&self, server_id: &str) -> Option<Duration> {
        self.critical_hits.lock().unwrap().get(server_id).map(Instant::elapsed)
    }

    pub fn forget(&self, server_id: &str) {
        self.trackers.lock().unwrap().remove(server_id);
        self.critical_hits.lock().unwrap().remove(server_id);
    }

    /// Play the alert sound and show a notification even while alerts are switched off (settings dialog).
    pub fn test(&self, severity: Severity) {
        let settings = self.settings.lock().unwrap().clone();
        if settings.sound {
            let _ = self
                .jobs
                .send(Job::Sound(tone_for(&settings, severity), settings.volume));
        }
        if settings.desktop {
            let example = Alert {
                key: "disk".into(),
                severity,
                title: format!("This is what a {} alert looks like", severity.label().to_lowercase()),
            };
            let _ = self.jobs.send(Job::Desktop {
                summary: summary(&settings, "gridwatch test", severity, false, 1),
                body: line(&settings, &example),
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
        let fresh = tracker.update(&current, now);
        // The server list marks this whatever the alert settings say.
        if fresh.iter().any(|a| a.severity == Severity::Critical) {
            self.critical_hits.lock().unwrap().insert(server_id.to_string(), now);
        }
        let new: Vec<Alert> = fresh.into_iter().filter(|a| wanted(a)).collect();
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
            let mut lines: Vec<String> = list.iter().take(4).map(|a| line(&settings, a)).collect();
            if list.len() > lines.len() {
                lines.push("…".to_string());
            }
            let _ = self.jobs.send(Job::Desktop {
                summary: summary(&settings, server_name, severity, reminder, list.len()),
                body: lines.join("\n"),
                critical: severity == Severity::Critical,
            });
        }
        if settings.sound {
            let mut last = self.last_sound.lock().unwrap();
            if last.is_none_or(|at| now.duration_since(at) >= SOUND_GAP) {
                *last = Some(now);
                let _ = self
                    .jobs
                    .send(Job::Sound(tone_for(&settings, severity), settings.volume));
            }
        }
    }
}

fn tone_for(settings: &Notifications, severity: Severity) -> Tone {
    if severity == Severity::Critical {
        settings.critical_tone
    } else {
        settings.warning_tone
    }
}

/// The headline of a notification, e.g. "🔴 db-01: 2 new problems".
fn summary(settings: &Notifications, server: &str, severity: Severity, reminder: bool, count: usize) -> String {
    let text = match (reminder, count) {
        (true, 1) => format!("{server}: still 1 open problem"),
        (true, n) => format!("{server}: still {n} open problems"),
        (false, 1) => format!("{server}: {}", severity.label().to_lowercase()),
        (false, n) => format!("{server}: {n} new problems"),
    };
    if !settings.emoji {
        return text;
    }
    let mark = if severity == Severity::Critical { "🔴" } else { "🟡" };
    format!("{mark} {text}")
}

/// One alert as a line of a notification, with a symbol for what it is about.
fn line(settings: &Notifications, alert: &Alert) -> String {
    if !settings.emoji {
        return alert.title.clone();
    }
    let title = alert.title.to_lowercase();
    let about = |words: &[&str]| words.iter().any(|w| title.contains(w));
    let symbol = if alert.key == "offline" {
        "🔌"
    } else if about(&["temperature", "°c"]) {
        "🌡️"
    } else if about(&["container", "docker"]) {
        "🐳"
    } else if about(&["gpu"]) {
        "🎮"
    } else if about(&["disk"]) {
        "💾"
    } else if about(&["memory", "swap"]) {
        "🧠"
    } else if about(&["cpu", "load", "core", "i/o wait"]) {
        "🔥"
    } else if about(&["zombie"]) {
        "🧟"
    } else {
        "⚠️"
    };
    format!("{symbol} {}", alert.title)
}

/// The alert tones are synthesised, so no sound files have to be shipped.
/// Returns mono samples.
fn tone(kind: Tone, volume: f32, rate: u32) -> Vec<f32> {
    // (frequency in Hz, start in seconds, length in seconds)
    let notes: &[(f32, f32, f32)] = match kind {
        // Two rising notes.
        Tone::Chime => &[(659.0, 0.0, 0.22), (880.0, 0.16, 0.42)],
        // Three short beeps, the last one higher.
        Tone::Beeps => &[(988.0, 0.0, 0.16), (988.0, 0.22, 0.16), (1319.0, 0.44, 0.38)],
        // One struck bell: a note with two overtones ringing out.
        Tone::Bell => &[(1047.0, 0.0, 1.1), (1568.0, 0.0, 0.8), (2093.0, 0.0, 0.5)],
        // Two soft pings, like an echo sounder.
        Tone::Sonar => &[(740.0, 0.0, 0.55), (740.0, 0.75, 0.55)],
        // High and low in turn; hard to miss.
        Tone::Alarm => &[
            (880.0, 0.0, 0.18),
            (660.0, 0.2, 0.18),
            (880.0, 0.4, 0.18),
            (660.0, 0.6, 0.18),
            (880.0, 0.8, 0.18),
            (660.0, 1.0, 0.18),
        ],
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
                *sample += wave * envelope * 0.3;
            }
        }
    }
    // Notes that sound together add up; keep the loudest moment the same for every tone.
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(0.001);
    let gain = 0.36 / peak * volume.clamp(0.0, 1.0);
    for sample in &mut samples {
        *sample *= gain;
    }
    samples
}

fn play(kind: Tone, volume: f32) -> Result<(), String> {
    use std::num::NonZero;
    const RATE: u32 = 44_100;
    let samples = tone(kind, volume, RATE);
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
    // macOS shows notifications in the name of an installed application. Left alone, the
    // library looks one up by a name that does not exist, and macOS answers with a
    // "Choose Application" dialog. Naming Finder up front avoids the lookup.
    #[cfg(target_os = "macos")]
    {
        static APPLICATION: std::sync::Once = std::sync::Once::new();
        APPLICATION.call_once(|| {
            let _ = notify_rust::set_application("com.apple.Finder");
        });
    }
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
    fn notifications_carry_marks_only_when_wanted() {
        let mut settings = Notifications::default();
        let disk = alert("Disk / is 97% full", Severity::Critical);
        let offline = Alert {
            key: "offline".into(),
            ..alert("Connection lost: timeout", Severity::Critical)
        };
        assert_eq!(
            summary(&settings, "db-01", Severity::Critical, false, 2),
            "🔴 db-01: 2 new problems"
        );
        assert_eq!(
            summary(&settings, "db-01", Severity::Warning, true, 1),
            "🟡 db-01: still 1 open problem"
        );
        assert_eq!(line(&settings, &disk), "💾 Disk / is 97% full");
        assert_eq!(line(&settings, &offline), "🔌 Connection lost: timeout");
        assert_eq!(
            line(&settings, &alert("High temperature: 91 °C", Severity::Critical)),
            "🌡️ High temperature: 91 °C"
        );
        assert_eq!(
            line(&settings, &alert("Something new", Severity::Warning)),
            "⚠️ Something new"
        );
        settings.emoji = false;
        assert_eq!(
            summary(&settings, "db-01", Severity::Critical, false, 1),
            "db-01: critical"
        );
        assert_eq!(line(&settings, &disk), "Disk / is 97% full");
    }

    /// Makes noise and shows a notification on this computer: `cargo test hear_and_see -- --ignored`
    #[test]
    #[ignore]
    fn hear_and_see() {
        let settings = Notifications::default();
        let example = alert("Disk / is 97% full", Severity::Critical);
        show(
            &summary(&settings, "db-01", Severity::Critical, false, 1),
            &line(&settings, &example),
            false,
        )
        .unwrap();
        for kind in [Tone::Bell, Tone::Sonar] {
            play(kind, 0.4).unwrap();
        }
    }

    #[test]
    fn remembers_when_a_critical_problem_arrived() {
        // Alerts are off: the mark for the server list must not depend on them.
        let notifier = Notifier::new(Notifications {
            enabled: false,
            ..Default::default()
        });
        let mut state = ServerState::default();
        notifier.check("a", "server a", &state);
        assert!(notifier.critical_hit("a").is_none());

        state.conn = ConnState::Error("timeout".into());
        notifier.check("a", "server a", &state);
        let first = *notifier.critical_hits.lock().unwrap().get("a").unwrap();
        // The same problem seen again is not a new hit.
        std::thread::sleep(Duration::from_millis(15));
        notifier.check("a", "server a", &state);
        assert_eq!(*notifier.critical_hits.lock().unwrap().get("a").unwrap(), first);
        assert!(notifier.critical_hit("a").unwrap() >= Duration::from_millis(15));
        assert!(notifier.critical_hit("b").is_none());

        notifier.forget("a");
        assert!(notifier.critical_hit("a").is_none());
    }

    #[test]
    fn tones_stay_within_range() {
        for kind in Tone::ALL {
            let samples = tone(kind, 1.0, 44_100);
            assert!(samples.len() > 20_000);
            assert!(samples.iter().all(|s| s.abs() <= 1.0));
            assert!(samples.last().unwrap().abs() < 0.01);
        }
    }
}
