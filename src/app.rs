//! The window: server list on the left, tabs for the selected server on the right.

use crate::collector::{self, Env};
use crate::model::*;
use crate::notify::Notifier;
use crate::store::{Config, Notifications, Paths, Store};
use crate::suggest::{self, Severity, Suggestion};
use crate::update::{self, Release};
use crate::widgets::{self as w, ChartOptions, Series};
use crate::{Args, demo, now_ts, secrets};
use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Sense, Ui};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RANGES: [(&str, i64); 6] = [
    ("5 min", 300),
    ("15 min", 900),
    ("1 h", 3600),
    ("6 h", 21600),
    ("24 h", 86400),
    ("7 d", 604800),
];
/// The screenshot that is taken with the settings window open.
const SETTINGS_SHOT: &str = "settings.png";
const HOMEPAGE: &str = "https://github.com/voidNK/gridwatch";
const DONATE_PAYPAL: &str = "https://paypal.me/lizo";
const DONATE_KOFI: &str = "https://ko-fi.com/A0A05OS1A";
const INTERVALS: [u64; 5] = [2, 5, 10, 30, 60];
const RETENTION_DAYS: [u32; 5] = [1, 7, 14, 30, 90];
/// Minutes between reminders about problems that stay open; 0 = never.
const REPEAT_MINUTES: [u32; 5] = [0, 5, 15, 30, 60];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Overview,
    Cpu,
    Cores,
    Memory,
    Processes,
    Network,
    Disks,
    Temps,
    Gpu,
    Docker,
    Hardware,
    Suggestions,
}

struct Server {
    cfg: ServerConfig,
    shared: Shared,
    task: tokio::task::JoinHandle<()>,
    /// Containers that own a chart colour, in order of first appearance.
    color_slots: Vec<String>,
}

/// Add / edit dialog.
struct Dialog {
    editing: Option<usize>,
    name: String,
    host: String,
    port: String,
    user: String,
    auth: usize,
    key_path: String,
    secret: String,
    error: String,
}

struct Screenshots {
    dir: PathBuf,
    shots: Vec<(usize, Tab, &'static str)>,
    index: usize,
    frames: u32,
    since: Instant,
    delay: Duration,
    requested: bool,
    done: Arc<AtomicBool>,
}

/// Where the search for a newer version stands.
#[derive(Clone, Default, PartialEq)]
enum UpdateState {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    CheckFailed(String),
    Available(Release),
    Installing(Release),
    InstallFailed(Release, String),
    /// The new version is in place and runs from the next start.
    Installed(String),
}

pub struct App {
    rt: tokio::runtime::Runtime,
    env: Arc<Env>,
    /// `None` in demo mode: nothing is read from or written to disk.
    paths: Option<Paths>,
    config: Config,
    servers: Vec<Server>,
    /// Passwords and passphrases live in memory only.
    secrets: HashMap<String, String>,
    selected: usize,
    tab: Tab,
    range: usize,
    dialog: Option<Dialog>,
    /// Index of the server card being dragged to a new place in the list.
    dragging: Option<usize>,
    settings_open: bool,
    /// Written by the background task that asks GitHub or installs.
    update: Arc<Mutex<UpdateState>>,
    update_open: bool,
    confirm_remove: bool,
    secret_input: String,
    proc_filter: String,
    last_prune: Instant,
    screenshots: Option<Screenshots>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> anyhow::Result<Self> {
        w::apply_theme(&cc.egui_ctx);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (paths, config, store) = if args.demo {
            let config = Config {
                servers: demo::servers(),
                ..Default::default()
            };
            (None, config, Store::in_memory()?)
        } else {
            let paths = Paths::resolve()?;
            let config = Config::load(&paths.config)?;
            let store = Store::open(&paths.database)?;
            (Some(paths), config, store)
        };
        let range = 2;
        let env = Arc::new(Env {
            store: Arc::new(store),
            known_hosts: paths.as_ref().map(|p| p.known_hosts.clone()).unwrap_or_default(),
            repaint: cc.egui_ctx.clone(),
            interval: AtomicU64::new(config.interval_secs),
            keep_secs: AtomicI64::new(RANGES[range].1),
            // Taking screenshots must not ring or pop up anything.
            notifier: Notifier::new(Notifications {
                enabled: config.notifications.enabled && args.screenshot_dir.is_none(),
                ..config.notifications.clone()
            }),
        });
        let _ = env.store.prune(config.retention_days, now_ts());

        // In the demo each tab is shown on the server where it has the most to tell.
        let pick = |demo_server: usize| if args.demo { demo_server } else { 0 };
        let screenshots = args.screenshot_dir.map(|dir| Screenshots {
            dir,
            shots: vec![
                (0, Tab::Overview, "overview.png"),
                (0, Tab::Cpu, "cpu.png"),
                (0, Tab::Cores, "cores.png"),
                (pick(1), Tab::Memory, "memory.png"),
                (0, Tab::Processes, "processes.png"),
                (0, Tab::Network, "network.png"),
                (pick(1), Tab::Disks, "disks.png"),
                (pick(1), Tab::Temps, "temperatures.png"),
                (pick(2), Tab::Gpu, "gpu.png"),
                (0, Tab::Docker, "docker.png"),
                (pick(1), Tab::Hardware, "hardware.png"),
                (pick(2), Tab::Suggestions, "suggestions.png"),
                (0, Tab::Overview, SETTINGS_SHOT),
            ],
            index: 0,
            frames: 0,
            since: Instant::now(),
            delay: Duration::from_secs_f32(args.screenshot_delay.max(0.2)),
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
        });

        let mut app = Self {
            rt,
            env,
            paths,
            servers: Vec::new(),
            secrets: HashMap::new(),
            selected: 0,
            tab: Tab::Overview,
            range,
            dialog: None,
            dragging: None,
            settings_open: false,
            update: Arc::default(),
            update_open: false,
            confirm_remove: false,
            secret_input: String::new(),
            proc_filter: String::new(),
            last_prune: Instant::now(),
            screenshots,
            config,
        };
        if !app.is_demo() {
            if !cfg!(windows) {
                app.config.show_wsl = false;
            }
            let (local, wsl) = (app.config.show_local, app.config.show_wsl);
            app.config.servers.retain(|s| match s.kind {
                ServerKind::Local => local,
                ServerKind::Wsl => wsl,
                ServerKind::Ssh => true,
            });
            if app.config.remember_secrets {
                for cfg in app
                    .config
                    .servers
                    .iter()
                    .filter(|c| c.kind == ServerKind::Ssh && c.auth != AuthMethod::Auto)
                {
                    if let Some(secret) = secrets::load(&cfg.id) {
                        app.secrets.insert(cfg.id.clone(), secret);
                    }
                }
            }
        }
        for (i, cfg) in app.config.servers.clone().into_iter().enumerate() {
            let server = app.start(cfg, i);
            app.servers.push(server);
        }
        if !app.is_demo() {
            app.apply_builtin();
            if app.config.check_updates && app.screenshots.is_none() {
                app.check_for_update();
            }
        }
        Ok(app)
    }

    fn is_demo(&self) -> bool {
        self.paths.is_none()
    }

    /// Load the stored history for a server and start its collector task.
    fn start(&self, cfg: ServerConfig, index: usize) -> Server {
        let shared: Shared = Arc::new(Mutex::new(ServerState::default()));
        let task = if self.is_demo() {
            collector::spawn_demo(self.rt.handle(), cfg.clone(), index, shared.clone(), self.env.clone())
        } else if cfg.kind != ServerKind::Ssh {
            self.load_history(&cfg.id, &shared);
            collector::spawn_local(self.rt.handle(), cfg.clone(), shared.clone(), self.env.clone())
        } else {
            self.load_history(&cfg.id, &shared);
            let secret = self.secrets.get(&cfg.id).cloned();
            collector::spawn(self.rt.handle(), cfg.clone(), secret, shared.clone(), self.env.clone())
        };
        Server {
            cfg,
            shared,
            task,
            color_slots: Vec::new(),
        }
    }

    fn load_history(&self, id: &str, shared: &Shared) {
        let now = now_ts();
        let since = now - RANGES[self.range].1;
        let history = self.env.store.load_history(id, since, now).unwrap_or_default();
        let containers = self
            .env
            .store
            .load_container_history(id, since, now)
            .unwrap_or_default();
        let mut state = shared.lock().unwrap();
        state.history = history;
        state.container_history = containers;
    }

    fn restart(&mut self, index: usize) {
        let old = &self.servers[index];
        old.task.abort();
        let server = self.start(old.cfg.clone(), index);
        self.servers[index] = server;
    }

    fn save_config(&mut self) {
        self.config.servers = self.servers.iter().map(|s| s.cfg.clone()).collect();
        if let Some(paths) = &self.paths
            && let Err(e) = self.config.save(&paths.config)
        {
            eprintln!("gridwatch: cannot save config: {e:#}");
        }
    }

    fn set_range(&mut self, range: usize) {
        self.range = range;
        self.env.keep_secs.store(RANGES[range].1, Ordering::Relaxed);
        for s in &self.servers {
            self.load_history(&s.cfg.id, &s.shared);
        }
    }

    fn remove_selected(&mut self) {
        // This computer and WSL are only hidden; their history stays for when they come back.
        match self.servers[self.selected].cfg.kind {
            ServerKind::Local => self.config.show_local = false,
            ServerKind::Wsl => self.config.show_wsl = false,
            ServerKind::Ssh => {
                let server = self.servers.remove(self.selected);
                server.task.abort();
                let _ = self.env.store.delete_server(&server.cfg.id);
                self.secrets.remove(&server.cfg.id);
                secrets::delete(&server.cfg.id);
                self.env.notifier.forget(&server.cfg.id);
                self.selected = self.selected.min(self.servers.len().saturating_sub(1));
            }
        }
        self.apply_builtin();
    }

    /// Add or drop the entries for this computer and its WSL distribution, as chosen in the settings.
    fn apply_builtin(&mut self) {
        for (kind, wanted) in [
            (ServerKind::Local, self.config.show_local),
            (ServerKind::Wsl, self.config.show_wsl),
        ] {
            let position = self.servers.iter().position(|s| s.cfg.kind == kind);
            match (position, wanted) {
                (None, true) => {
                    // This computer goes to the top, WSL right below it.
                    let local = self.servers.iter().position(|s| s.cfg.kind == ServerKind::Local);
                    let at = if kind == ServerKind::Local {
                        0
                    } else {
                        local.map_or(0, |i| i + 1)
                    };
                    let server = self.start(ServerConfig::builtin(kind), at);
                    if !self.servers.is_empty() && self.selected >= at {
                        self.selected += 1;
                    }
                    self.servers.insert(at, server);
                }
                (Some(i), false) => {
                    let server = self.servers.remove(i);
                    server.task.abort();
                    self.env.notifier.forget(&server.cfg.id);
                    if self.selected > i {
                        self.selected -= 1;
                    }
                }
                _ => {}
            }
        }
        self.selected = self.selected.min(self.servers.len().saturating_sub(1));
        self.save_config();
    }

    /// Put the server at `from` in front of the card that is at `slot` now.
    fn move_server(&mut self, from: usize, slot: usize) {
        let to = if slot > from { slot - 1 } else { slot };
        if from >= self.servers.len() || to >= self.servers.len() || to == from {
            return;
        }
        let selected = self.servers[self.selected].cfg.id.clone();
        let server = self.servers.remove(from);
        self.servers.insert(to, server);
        self.selected = self.servers.iter().position(|s| s.cfg.id == selected).unwrap_or(0);
        self.save_config();
    }

    /// Keep a password or passphrase for the next start, if the user wants that.
    fn remember(&self, server_id: &str, secret: &str) {
        if !self.is_demo()
            && self.config.remember_secrets
            && let Err(e) = secrets::save(server_id, secret)
        {
            eprintln!("gridwatch: cannot save the password in the system keychain: {e}");
        }
    }

    fn open_dialog(&mut self, editing: Option<usize>) {
        let d = match editing.map(|i| &self.servers[i].cfg) {
            Some(cfg) => Dialog {
                editing,
                name: cfg.name.clone(),
                host: cfg.host.clone(),
                port: cfg.port.to_string(),
                user: cfg.user.clone(),
                auth: match cfg.auth {
                    AuthMethod::Auto => 0,
                    AuthMethod::KeyFile { .. } => 1,
                    AuthMethod::Password => 2,
                },
                key_path: match &cfg.auth {
                    AuthMethod::KeyFile { path } => path.clone(),
                    _ => "~/.ssh/id_ed25519".into(),
                },
                secret: String::new(),
                error: String::new(),
            },
            None => Dialog {
                editing,
                name: String::new(),
                host: String::new(),
                port: "22".into(),
                user: "root".into(),
                auth: 0,
                key_path: "~/.ssh/id_ed25519".into(),
                secret: String::new(),
                error: String::new(),
            },
        };
        self.dialog = Some(d);
    }

    /// Validate the dialog and add or update the server.
    fn submit_dialog(&mut self) {
        let Some(d) = &mut self.dialog else { return };
        // Accept "user@host" typed into the host field.
        if let Some((user, host)) = d.host.clone().split_once('@') {
            d.user = user.trim().to_string();
            d.host = host.trim().to_string();
        }
        let host = d.host.trim().to_string();
        let user = d.user.trim().to_string();
        let Ok(port) = d.port.trim().parse::<u16>() else {
            d.error = "Port must be a number between 1 and 65535.".into();
            return;
        };
        if host.is_empty() || user.is_empty() {
            d.error = "Host and user are required.".into();
            return;
        }
        let auth = match d.auth {
            1 => AuthMethod::KeyFile {
                path: d.key_path.trim().to_string(),
            },
            2 => AuthMethod::Password,
            _ => AuthMethod::Auto,
        };
        let name = if d.name.trim().is_empty() {
            host.clone()
        } else {
            d.name.trim().to_string()
        };
        let editing = d.editing;
        let secret = std::mem::take(&mut d.secret);
        let id = editing
            .map(|i| self.servers[i].cfg.id.clone())
            .unwrap_or_else(ServerConfig::new_id);
        let cfg = ServerConfig {
            id: id.clone(),
            name,
            host,
            port,
            user,
            auth,
            kind: ServerKind::Ssh,
        };

        if secret.is_empty() {
            if editing.is_some_and(|i| self.servers[i].cfg.auth != cfg.auth) {
                self.secrets.remove(&id);
                secrets::delete(&id);
            }
        } else {
            self.remember(&id, &secret);
            self.secrets.insert(id, secret);
        }
        match editing {
            Some(i) => {
                self.servers[i].cfg = cfg;
                self.restart(i);
            }
            None => {
                let server = self.start(cfg, self.servers.len());
                self.servers.push(server);
                self.selected = self.servers.len() - 1;
            }
        }
        self.dialog = None;
        self.save_config();
    }

    fn drive_screenshots(&mut self, ctx: &egui::Context) {
        let Some(shots) = &mut self.screenshots else { return };
        ctx.request_repaint();
        let (server, tab, file) = shots.shots[shots.index];
        self.selected = server.min(self.servers.len().saturating_sub(1));
        self.tab = tab;
        self.settings_open = file == SETTINGS_SHOT;
        shots.frames += 1;
        if !shots.requested {
            if shots.frames > 20 && shots.since.elapsed() >= shots.delay {
                let path = shots.dir.join(file);
                let done = shots.done.clone();
                let _ = std::fs::create_dir_all(&shots.dir);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                    move |image: Arc<egui::ColorImage>| {
                        let [width, height] = image.size;
                        if let Err(e) = image::save_buffer(
                            &path,
                            image.as_raw(),
                            width as u32,
                            height as u32,
                            image::ColorType::Rgba8,
                        ) {
                            eprintln!("gridwatch: cannot save {}: {e}", path.display());
                        }
                        done.store(true, Ordering::SeqCst);
                    },
                )));
                shots.requested = true;
            }
        } else if shots.done.swap(false, Ordering::SeqCst) {
            shots.index += 1;
            shots.frames = 0;
            shots.requested = false;
            shots.since = Instant::now();
            shots.delay = Duration::from_millis(300);
            if shots.index == shots.shots.len() {
                self.screenshots = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drive_screenshots(&ctx);
        if self.last_prune.elapsed() > Duration::from_secs(3600) {
            self.last_prune = Instant::now();
            let _ = self.env.store.prune(self.config.retention_days, now_ts());
        }
        // Keep the time axis moving even when no sample arrives.
        ctx.request_repaint_after(Duration::from_secs(1));

        egui::Panel::top("top")
            .frame(Frame::new().fill(w::SURFACE).inner_margin(Margin::symmetric(14, 8)))
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::left("servers")
            .resizable(false)
            .exact_size(264.0)
            .frame(Frame::new().fill(w::PAGE).inner_margin(Margin::same(10)))
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default()
            .frame(Frame::new().fill(w::PAGE).inner_margin(Margin::same(14)))
            .show(ui, |ui| self.central(ui));

        self.dialog_window(&ctx);
        self.settings_window(&ctx);
        self.update_window(&ctx);
        self.remove_window(&ctx);
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("gridwatch").size(18.0).strong());
            ui.label(w::muted(concat!("v", env!("CARGO_PKG_VERSION"))));
            if self.is_demo() {
                ui.label(w::muted("demo data"));
            }
            ui.add_space(18.0);
            let mut range = self.range;
            for (i, (label, _)) in RANGES.iter().enumerate() {
                ui.selectable_value(&mut range, i, *label);
            }
            if range != self.range {
                self.set_range(range);
            }
            ui.add_space(18.0);
            ui.label(w::muted("Refresh"));
            let mut interval = self.config.interval_secs;
            egui::ComboBox::from_id_salt("interval")
                .width(64.0)
                .selected_text(format!("{interval} s"))
                .show_ui(ui, |ui| {
                    for v in INTERVALS {
                        ui.selectable_value(&mut interval, v, format!("{v} s"));
                    }
                });
            if interval != self.config.interval_secs {
                self.config.interval_secs = interval;
                self.env.interval.store(interval, Ordering::Relaxed);
                self.save_config();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Settings").clicked() {
                    self.settings_open = !self.settings_open;
                }
                let update = self.update.lock().unwrap().clone();
                let label = match &update {
                    UpdateState::Available(release) => Some(format!("Update to {}", release.version)),
                    UpdateState::Installing(_) => Some("Updating…".to_string()),
                    UpdateState::InstallFailed(..) => Some("Update failed".to_string()),
                    UpdateState::Installed(_) => Some("Restart to update".to_string()),
                    _ => None,
                };
                if let Some(label) = label {
                    let button = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(w::SERIES[0]);
                    if ui.add(button).clicked() {
                        self.update_open = true;
                    }
                }
                let on = self.config.notifications.enabled;
                let bell = ui
                    .selectable_label(on, if on { "🔔 Alerts on" } else { "🔕 Alerts off" })
                    .on_hover_text(
                        "Sound and desktop notification when a server gets into trouble.
Click to switch on or off.",
                    );
                if bell.clicked() {
                    self.config.notifications.enabled = !on;
                    self.apply_notifications();
                }
                if !self.is_demo() && ui.button("+ Add server").clicked() {
                    self.open_dialog(None);
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(w::muted(format!("SERVERS ({})", self.servers.len())));
            if self.servers.len() > 1 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(w::muted("drag to sort"))
                });
            }
        });
        ui.add_space(2.0);
        let mut clicked = None;
        let mut dropped = None;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut cards = Vec::new();
            for (i, server) in self.servers.iter().enumerate() {
                let state = server.shared.lock().unwrap();
                let selected = i == self.selected;
                let alerts = suggest::alert_count(&suggest::analyze(&state));
                let fill = if self.dragging == Some(i) {
                    w::GRID
                } else if selected {
                    w::SURFACE
                } else {
                    w::PAGE
                };
                let response = Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, if selected { w::AXIS } else { w::GRID }))
                    .corner_radius(CornerRadius::same(5))
                    .inner_margin(Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        // Two lines per server: state and name, then CPU and memory.
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 3.0);
                        let (color, label) = conn_status(&state);
                        let small = |text: String, color: Color32| RichText::new(text).size(11.5).color(color);
                        ui.horizontal(|ui| {
                            let (dot, _) = ui.allocate_exact_size(egui::Vec2::splat(9.0), Sense::hover());
                            ui.painter().circle_filled(dot.center(), 4.5, color);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(small(label.to_string(), w::TEXT_SECONDARY));
                                if alerts > 0 {
                                    let text = format!("{alerts} alert{}", if alerts == 1 { "" } else { "s" });
                                    ui.label(small(text, w::WARNING));
                                }
                                // The name gets what is left and is cut off if it does not fit.
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(egui::Label::new(RichText::new(&server.cfg.name).strong()).truncate());
                                });
                            });
                        });
                        match &state.latest {
                            Some(s) => {
                                let mem = s.mem_used as f32 / s.mem_total.max(1) as f32;
                                ui.horizontal(|ui| {
                                    for (name, fraction) in [("cpu", s.cpu.total / 100.0), ("mem", mem)] {
                                        ui.label(small(name.to_string(), w::MUTED));
                                        w::meter(ui, fraction, 50.0);
                                        ui.label(small(format!("{:.0}%", fraction * 100.0), w::TEXT_SECONDARY));
                                        ui.add_space(4.0);
                                    }
                                });
                            }
                            // Nothing measured yet: say where it is instead.
                            None => {
                                ui.label(small(server.cfg.address(), w::MUTED));
                            }
                        }
                    })
                    .response
                    .interact(Sense::click_and_drag())
                    .on_hover_text(server.cfg.address());
                cards.push(response.rect);
                if response.clicked() {
                    clicked = Some(i);
                }
                if response.drag_started() {
                    self.dragging = Some(i);
                }
            }

            // While a card is dragged, a line shows where it would land.
            if let (Some(from), Some(pointer)) = (self.dragging, ui.ctx().pointer_interact_pos()) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                let slot = cards.iter().filter(|card| card.center().y < pointer.y).count();
                if slot != from && slot != from + 1 {
                    let y = match cards.get(slot) {
                        Some(card) => card.top() - 2.0,
                        None => cards.last().map_or(0.0, |card| card.bottom() + 2.0),
                    };
                    ui.painter()
                        .hline(cards[0].x_range(), y, egui::Stroke::new(2.0, w::SERIES[0]));
                }
                if ui.input(|i| i.pointer.any_released()) {
                    dropped = Some((from, slot));
                }
            }
        });
        if !ui.input(|i| i.pointer.any_down()) {
            self.dragging = None;
        }
        if let Some(i) = clicked {
            self.selected = i;
            self.secret_input.clear();
        }
        if let Some((from, slot)) = dropped {
            self.move_server(from, slot);
        }
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.servers.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.label(RichText::new("No servers yet").size(22.0).strong());
                ui.label(w::muted(
                    "Add a Linux or macOS machine you can reach over SSH. Nothing needs to be installed on it.",
                ));
                ui.add_space(8.0);
                if ui.button("+ Add server").clicked() {
                    self.open_dialog(None);
                }
            });
            return;
        }
        let index = self.selected.min(self.servers.len() - 1);
        let now = now_ts();
        let range = (now - RANGES[self.range].1, now);
        let demo = self.is_demo();
        let builtin = self.servers[index].cfg.kind != ServerKind::Ssh;
        let remember = self.config.remember_secrets;
        let mut hide = false;
        let mut edit = false;
        let mut reconnect = false;

        {
            let server = &mut self.servers[index];
            let state = server.shared.lock().unwrap();
            let suggestions = suggest::analyze(&state);

            // Header
            ui.horizontal(|ui| {
                ui.label(RichText::new(&server.cfg.name).size(22.0).strong());
                let (color, label) = conn_status(&state);
                w::status(ui, color, label);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !demo && builtin {
                        hide = ui.button("Hide").on_hover_text("Bring it back in Settings.").clicked();
                    } else if !demo {
                        if ui.button("Remove").clicked() {
                            self.confirm_remove = true;
                        }
                        edit = ui.button("Edit").clicked();
                    }
                });
            });
            let mut facts = vec![server.cfg.address()];
            if let Some(s) = &state.latest {
                for fact in [&s.info.os, &s.info.kernel, &s.info.cpu_model] {
                    if !fact.is_empty() {
                        facts.push(fact.clone());
                    }
                }
                facts.push(format!("{} cores", s.cores.len()));
                facts.push(format!("up {}", fmt_duration(s.uptime)));
            }
            ui.label(w::muted(facts.join("  ·  ")));
            ui.add_space(4.0);

            match &state.conn {
                ConnState::Error(msg) => {
                    w::card(ui, |ui| {
                        w::status(ui, w::CRITICAL, "Connection problem - retrying automatically");
                        ui.label(RichText::new(msg).color(w::TEXT_SECONDARY));
                    });
                }
                ConnState::NeedsSecret(prompt) => {
                    w::card(ui, |ui| {
                        ui.label(RichText::new(prompt).strong());
                        ui.horizontal(|ui| {
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut self.secret_input)
                                    .password(true)
                                    .desired_width(260.0),
                            );
                            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (ui.button("Connect").clicked() || enter) && !self.secret_input.is_empty() {
                                reconnect = true;
                            }
                        });
                        ui.label(w::muted(if remember {
                            "Saved in the keychain of this computer, so it is asked for only once (see Settings)."
                        } else {
                            "Kept in memory until gridwatch is closed; never written to disk."
                        }));
                    });
                }
                ConnState::Connecting if state.latest.is_none() => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(w::muted("Connecting and taking the first measurement…"));
                    });
                }
                _ => {}
            }

            if let Some(sample) = &state.latest {
                let alerts = suggest::alert_count(&suggestions);
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (Tab::Overview, "Overview".to_string()),
                        (
                            Tab::Gpu,
                            if sample.gpus.len() > 1 {
                                format!("GPU ({})", sample.gpus.len())
                            } else {
                                "GPU".to_string()
                            },
                        ),
                        (Tab::Cpu, "CPU".to_string()),
                        (Tab::Cores, format!("Cores ({})", sample.cores.len())),
                        (Tab::Memory, "Memory".to_string()),
                        (Tab::Processes, format!("Processes ({})", sample.proc_count)),
                        (Tab::Network, "Network".to_string()),
                        (Tab::Disks, "Disks".to_string()),
                        (Tab::Temps, "Temperatures".to_string()),
                        (Tab::Docker, "Docker".to_string()),
                        (Tab::Hardware, "Hardware".to_string()),
                        (
                            Tab::Suggestions,
                            if alerts > 0 {
                                format!("Suggestions ({alerts})")
                            } else {
                                "Suggestions".to_string()
                            },
                        ),
                    ] {
                        // Only machines with a graphics card get the GPU tab.
                        if tab == Tab::Gpu && sample.gpus.is_empty() {
                            continue;
                        }
                        ui.selectable_value(&mut self.tab, tab, label);
                    }
                });
                ui.add_space(4.0);
                if self.tab == Tab::Gpu && sample.gpus.is_empty() {
                    self.tab = Tab::Overview;
                }
                let tab = self.tab;
                let filter = &mut self.proc_filter;
                let slots = &mut server.color_slots;
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match tab {
                        Tab::Overview => overview_tab(ui, &state, sample, range),
                        Tab::Cpu => cpu_tab(ui, &state, sample, range),
                        Tab::Cores => cores_tab(ui, &state, sample, range),
                        Tab::Memory => memory_tab(ui, &state, sample, range),
                        Tab::Processes => processes_tab(ui, sample, filter),
                        Tab::Network => network_tab(ui, &state, sample, range),
                        Tab::Disks => disks_tab(ui, &state, sample, range),
                        Tab::Temps => temps_tab(ui, &state, sample, range),
                        Tab::Gpu => gpu_tab(ui, &state, sample, range),
                        Tab::Docker => docker_tab(ui, &state, sample, range, slots),
                        Tab::Hardware => hardware_tab(ui, &state),
                        Tab::Suggestions => suggestions_tab(ui, &suggestions),
                    });
            }
        }

        if hide {
            self.selected = index;
            self.remove_selected();
            return;
        }
        if edit {
            self.open_dialog(Some(index));
        }
        if reconnect {
            let id = self.servers[index].cfg.id.clone();
            let secret = std::mem::take(&mut self.secret_input);
            self.remember(&id, &secret);
            self.secrets.insert(id, secret);
            self.restart(index);
        }
    }

    fn dialog_window(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.dialog else { return };
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(if d.editing.is_some() {
            "Edit server"
        } else {
            "Add server"
        })
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Grid::new("server-form")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut d.name)
                            .hint_text("optional, defaults to the host")
                            .desired_width(280.0),
                    );
                    ui.end_row();
                    ui.label("Host");
                    ui.add(
                        egui::TextEdit::singleline(&mut d.host)
                            .hint_text("hostname, IP or user@host")
                            .desired_width(280.0),
                    );
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut d.port).desired_width(70.0));
                    ui.end_row();
                    ui.label("User");
                    ui.add(egui::TextEdit::singleline(&mut d.user).desired_width(280.0));
                    ui.end_row();
                    ui.label("Authentication");
                    let kinds = ["SSH agent / default keys", "Key file", "Password"];
                    egui::ComboBox::from_id_salt("auth")
                        .width(280.0)
                        .selected_text(kinds[d.auth])
                        .show_ui(ui, |ui| {
                            for (i, kind) in kinds.iter().enumerate() {
                                ui.selectable_value(&mut d.auth, i, *kind);
                            }
                        });
                    ui.end_row();
                    if d.auth == 1 {
                        ui.label("Key file");
                        ui.add(egui::TextEdit::singleline(&mut d.key_path).desired_width(280.0));
                        ui.end_row();
                    }
                    if d.auth != 0 {
                        ui.label(if d.auth == 1 { "Passphrase" } else { "Password" });
                        ui.add(
                            egui::TextEdit::singleline(&mut d.secret)
                                .password(true)
                                .hint_text("asked for on connect if left empty")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                    }
                });
            if !d.error.is_empty() {
                w::status(ui, w::CRITICAL, &d.error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                submit = ui.button(if d.editing.is_some() { "Save" } else { "Add" }).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dialog = None;
        } else if submit {
            self.submit_dialog();
        }
    }

    /// Hand the notification settings to the collectors and save them.
    fn apply_notifications(&mut self) {
        self.env.notifier.set(Notifications {
            enabled: self.config.notifications.enabled && self.screenshots.is_none(),
            ..self.config.notifications.clone()
        });
        self.save_config();
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let before = self.config.clone();
        let mut open = true;
        let mut close = false;
        let mut test = None;
        let mut check_now = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_size([940.0, 560.0])
            .show(ctx, |ui| {
                // Scrolls when the window is made smaller than its content.
                ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    // Settings take two thirds of the width, the about column the rest.
                    let width = ui.available_width().max(780.0);
                    let column = |share: f32| egui::vec2(width * share - 14.0, 0.0);
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(column(2.0 / 3.0), Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            let n = &mut self.config.notifications;
                            w::section(ui, "Notifications", "when a server gets into trouble");
                            ui.checkbox(&mut n.enabled, "Alerts enabled")
                                .on_hover_text("Same switch as the bell in the top bar.");
                            ui.add_enabled_ui(n.enabled, |ui| {
                                egui::Grid::new("notify-form")
                                    .num_columns(2)
                                    .spacing([16.0, 8.0])
                                    .show(ui, |ui| {
                                        ui.checkbox(&mut n.sound, "Play a sound");
                                        ui.add_enabled_ui(n.sound, |ui| {
                                            ui.horizontal(|ui| {
                                                let mut pct = n.volume * 100.0;
                                                ui.add(
                                                    egui::Slider::new(&mut pct, 5.0..=100.0)
                                                        .fixed_decimals(0)
                                                        .suffix("%")
                                                        .text("volume"),
                                                );
                                                n.volume = pct / 100.0;
                                            });
                                        });
                                        ui.end_row();
                                        ui.checkbox(&mut n.desktop, "Show a desktop notification");
                                        ui.label(w::muted("Windows toast, macOS Notification Center, Linux desktop"));
                                        ui.end_row();
                                        ui.label("Alert on");
                                        ui.horizontal(|ui| {
                                            ui.selectable_value(&mut n.critical_only, false, "Warnings and critical");
                                            ui.selectable_value(&mut n.critical_only, true, "Critical only");
                                        });
                                        ui.end_row();
                                        ui.checkbox(&mut n.offline, "Server unreachable");
                                        ui.label(w::muted("alert when the SSH connection is lost or fails"));
                                        ui.end_row();
                                        ui.label("Remind me");
                                        let text = |m: u32| {
                                            if m == 0 {
                                                "never".to_string()
                                            } else {
                                                format!("every {m} min")
                                            }
                                        };
                                        ui.horizontal(|ui| {
                                            egui::ComboBox::from_id_salt("repeat")
                                                .width(110.0)
                                                .selected_text(text(n.repeat_minutes))
                                                .show_ui(ui, |ui| {
                                                    for m in REPEAT_MINUTES {
                                                        ui.selectable_value(&mut n.repeat_minutes, m, text(m));
                                                    }
                                                });
                                            ui.label(w::muted("while problems stay open"));
                                        });
                                        ui.end_row();
                                    });
                            });
                            ui.horizontal(|ui| {
                                if ui.button("Test warning").clicked() {
                                    test = Some(Severity::Warning);
                                }
                                if ui.button("Test critical").clicked() {
                                    test = Some(Severity::Critical);
                                }
                                ui.label(w::muted("A problem is announced once, and again if it gets worse."));
                            });

                            ui.add_space(6.0);
                            w::section(ui, "Data", "");
                            egui::Grid::new("data-form")
                                .num_columns(2)
                                .spacing([16.0, 8.0])
                                .show(ui, |ui| {
                                    ui.label("Refresh every");
                                    egui::ComboBox::from_id_salt("settings-interval")
                                        .width(110.0)
                                        .selected_text(format!("{} s", self.config.interval_secs))
                                        .show_ui(ui, |ui| {
                                            for v in INTERVALS {
                                                ui.selectable_value(
                                                    &mut self.config.interval_secs,
                                                    v,
                                                    format!("{v} s"),
                                                );
                                            }
                                        });
                                    ui.end_row();
                                    ui.label("Keep history for");
                                    egui::ComboBox::from_id_salt("retention")
                                        .width(110.0)
                                        .selected_text(format!("{} days", self.config.retention_days))
                                        .show_ui(ui, |ui| {
                                            for v in RETENTION_DAYS {
                                                ui.selectable_value(
                                                    &mut self.config.retention_days,
                                                    v,
                                                    format!("{v} days"),
                                                );
                                            }
                                        });
                                    ui.end_row();
                                    if let Some(paths) = &self.paths {
                                        ui.label("Settings file");
                                        ui.label(w::muted(paths.config.display().to_string()));
                                        ui.end_row();
                                        ui.label("History");
                                        ui.label(w::muted(paths.database.display().to_string()));
                                        ui.end_row();
                                    }
                                });
                            if self.paths.is_some() {
                                ui.add_space(6.0);
                                w::section(ui, "This computer", "");
                                ui.checkbox(&mut self.config.show_local, "Show this computer in the server list");
                                if cfg!(windows) {
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut self.config.show_wsl, "Show its Linux (WSL)");
                                        ui.label(w::muted("the default distribution; watching it keeps WSL running"));
                                    });
                                }
                                ui.horizontal(|ui| {
                                    ui.checkbox(
                                        &mut self.config.remember_secrets,
                                        "Save passwords in the system keychain",
                                    );
                                    ui.label(w::muted(if cfg!(windows) {
                                        "Windows Credential Manager"
                                    } else if cfg!(target_os = "macos") {
                                        "macOS Keychain"
                                    } else {
                                        "GNOME Keyring or KWallet"
                                    }));
                                });
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut self.config.check_updates, "Look for a new version at start");
                                    let state = self.update.lock().unwrap().clone();
                                    if ui
                                        .add_enabled(state != UpdateState::Checking, egui::Button::new("Check now"))
                                        .clicked()
                                    {
                                        check_now = true;
                                    }
                                    match state {
                                        UpdateState::Checking => {
                                            ui.spinner();
                                        }
                                        UpdateState::UpToDate => {
                                            ui.label(w::muted(concat!(
                                                "you have the newest version, ",
                                                env!("CARGO_PKG_VERSION")
                                            )));
                                        }
                                        UpdateState::CheckFailed(message) => {
                                            ui.label(w::muted(message));
                                        }
                                        UpdateState::Available(release) => {
                                            ui.label(format!(
                                                "{} is available, see the button in the top bar",
                                                release.version
                                            ));
                                        }
                                        _ => {}
                                    }
                                });
                            } else {
                                ui.label(w::muted("Demo mode: settings are not saved."));
                            }
                            ui.add_space(4.0);
                            close = ui.button("Close").clicked();
                        });
                        ui.separator();
                        ui.allocate_ui_with_layout(column(1.0 / 3.0), Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 10.0;
                            w::section(ui, "About", concat!("gridwatch ", env!("CARGO_PKG_VERSION")));
                            ui.label(w::muted(
                                "gridwatch is free to use. If it helps you, a donation is welcome.",
                            ));
                            for (title, subtitle, url, icon) in [
                                ("Ko-fi", "buy me a coffee", DONATE_KOFI, w::Icon::Cup),
                                ("PayPal", "paypal.me/lizo", DONATE_PAYPAL, w::Icon::Wallet),
                            ] {
                                if w::big_icon_button(ui, icon, title, subtitle)
                                    .on_hover_text(url)
                                    .clicked()
                                {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                }
                            }
                            if ui.button("Website").on_hover_text(HOMEPAGE).clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(HOMEPAGE));
                            }
                        });
                    });
                });
            });

        if self.config.interval_secs != before.interval_secs {
            self.env.interval.store(self.config.interval_secs, Ordering::Relaxed);
        }
        if self.config.retention_days < before.retention_days {
            let _ = self.env.store.prune(self.config.retention_days, now_ts());
        }
        if self.config.remember_secrets != before.remember_secrets {
            // Switching it off also removes what was stored before.
            for server in &self.servers {
                match self.secrets.get(&server.cfg.id) {
                    Some(secret) if self.config.remember_secrets => self.remember(&server.cfg.id, secret),
                    _ => secrets::delete(&server.cfg.id),
                }
            }
        }
        if self.config != before {
            self.apply_notifications();
        }
        if self.config.show_local != before.show_local || self.config.show_wsl != before.show_wsl {
            self.apply_builtin();
        }
        if check_now {
            self.check_for_update();
        }
        if let Some(severity) = test {
            self.env.notifier.test(severity);
        }
        if !open || close {
            self.settings_open = false;
        }
    }

    /// Ask GitHub in the background whether a newer version exists.
    fn check_for_update(&self) {
        let (state, repaint) = (self.update.clone(), self.env.repaint.clone());
        *state.lock().unwrap() = UpdateState::Checking;
        self.rt.spawn_blocking(move || {
            let result = match update::check() {
                Ok(Some(release)) => UpdateState::Available(release),
                Ok(None) => UpdateState::UpToDate,
                Err(e) => UpdateState::CheckFailed(format!("{e:#}")),
            };
            *state.lock().unwrap() = result;
            repaint.request_repaint();
        });
    }

    fn install_update(&self, release: Release) {
        let (state, repaint) = (self.update.clone(), self.env.repaint.clone());
        *state.lock().unwrap() = UpdateState::Installing(release.clone());
        self.rt.spawn_blocking(move || {
            let result = match update::install(&release) {
                Ok(()) => UpdateState::Installed(release.version.clone()),
                Err(e) => UpdateState::InstallFailed(release, format!("{e:#}")),
            };
            *state.lock().unwrap() = result;
            repaint.request_repaint();
        });
    }

    /// The window behind the update button: what is new, and the button that installs it.
    fn update_window(&mut self, ctx: &egui::Context) {
        if !self.update_open {
            return;
        }
        let state = self.update.lock().unwrap().clone();
        let mut open = true;
        let mut install = None;
        let mut restart = false;
        let mut close = false;
        egui::Window::new("Update")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_size([520.0, 360.0])
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let notes = |ui: &mut Ui, release: &Release| {
                    ui.label(
                        RichText::new(format!("gridwatch {} is available", release.version))
                            .size(16.0)
                            .strong(),
                    );
                    ui.label(w::muted(concat!("You have ", env!("CARGO_PKG_VERSION"), ".")));
                    if !release.notes.is_empty() {
                        ScrollArea::vertical()
                            .max_height(220.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.label(RichText::new(&release.notes).color(w::TEXT_SECONDARY));
                            });
                    }
                };
                match &state {
                    UpdateState::Available(release) => {
                        notes(ui, release);
                        ui.horizontal(|ui| {
                            if ui.button("Download and install").clicked() {
                                install = Some(release.clone());
                            }
                            close = ui.button("Later").clicked();
                        });
                        ui.label(w::muted(
                            "Downloaded from the gridwatch releases on GitHub and checked against its checksum.",
                        ));
                    }
                    UpdateState::Installing(release) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Downloading and installing {}…", release.version));
                        });
                    }
                    UpdateState::InstallFailed(release, message) => {
                        w::status(ui, w::CRITICAL, "The update could not be installed");
                        ui.label(RichText::new(message).color(w::TEXT_SECONDARY));
                        ui.label(w::muted("The version you have keeps running."));
                        ui.horizontal(|ui| {
                            if ui.button("Try again").clicked() {
                                install = Some(release.clone());
                            }
                            close = ui.button("Close").clicked();
                        });
                    }
                    UpdateState::Installed(version) => {
                        w::status(ui, w::GOOD, &format!("gridwatch {version} is installed"));
                        ui.label("It starts with the next launch.");
                        ui.horizontal(|ui| {
                            restart = ui.button("Restart now").clicked();
                            close = ui.button("Later").clicked();
                        });
                    }
                    _ => close = true,
                }
            });
        if let Some(release) = install {
            self.install_update(release);
        }
        if restart {
            match update::restart() {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => *self.update.lock().unwrap() = UpdateState::CheckFailed(format!("{e:#}")),
            }
        }
        if !open || close {
            self.update_open = false;
        }
    }

    fn remove_window(&mut self, ctx: &egui::Context) {
        if !self.confirm_remove || self.servers.is_empty() {
            self.confirm_remove = false;
            return;
        }
        let name = self.servers[self.selected].cfg.name.clone();
        let mut remove = false;
        egui::Window::new("Remove server")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Remove {name} and delete its stored history?"));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    remove = ui.button("Remove").clicked();
                    if ui.button("Cancel").clicked() {
                        self.confirm_remove = false;
                    }
                });
            });
        if remove {
            self.confirm_remove = false;
            self.remove_selected();
        }
    }
}

fn conn_status(state: &ServerState) -> (Color32, &'static str) {
    match &state.conn {
        ConnState::Online => (w::GOOD, "online"),
        ConnState::Connecting => (w::MUTED, "connecting"),
        ConnState::NeedsSecret(_) => (w::WARNING, "locked"),
        ConnState::Error(_) => (w::CRITICAL, "offline"),
    }
}

fn points(state: &ServerState, f: impl Fn(&HistPoint) -> f64) -> Vec<[f64; 2]> {
    state.history.iter().map(|p| [p.ts as f64, f(p)]).collect()
}

const GIB: f64 = (1u64 << 30) as f64;

fn overview_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let mem_pct = s.mem_used as f32 / s.mem_total.max(1) as f32 * 100.0;
    let cores = s.cores.len().max(1) as f32;
    let worst_disk = s.disks.iter().max_by(|a, b| a.pct().total_cmp(&b.pct()));
    let (containers, container_sub, container_state) = match &state.docker {
        DockerState::Ok(list) => {
            let running = list.iter().filter(|c| c.state == "running").count();
            let broken = list
                .iter()
                .filter(|c| c.state == "restarting" || c.status.contains("unhealthy"))
                .count();
            (
                running.to_string(),
                format!(
                    "running, {} stopped",
                    list.len() - running - broken.min(list.len() - running)
                ),
                (broken > 0).then_some((w::CRITICAL, "container failing")),
            )
        }
        DockerState::NotInstalled => ("–".into(), "docker not installed".into(), None),
        DockerState::Error(_) => ("–".into(), "docker not accessible".into(), None),
        DockerState::Unknown => ("…".into(), String::new(), None),
    };
    w::tiles(
        ui,
        &[
            (
                "CPU",
                format!("{:.0}%", s.cpu.total),
                format!("{:.0} cores", cores),
                w::level(s.cpu.total, 85.0, 95.0),
            ),
            (
                "Memory",
                format!("{mem_pct:.0}%"),
                format!("{} of {}", fmt_bytes(s.mem_used), fmt_bytes(s.mem_total)),
                w::level(mem_pct, 90.0, 95.0),
            ),
            if s.info.is_local_windows {
                // Windows has no load average.
                ("Processes", s.proc_count.to_string(), "running".into(), None)
            } else {
                (
                    "Load (1 / 5 / 15 min)",
                    format!("{:.2}", s.load[0]),
                    format!("{:.2} / {:.2}", s.load[1], s.load[2]),
                    w::level(s.load[1] / cores * 100.0, 150.0, 300.0),
                )
            },
            (
                "Network",
                fmt_rate(s.rx_bps),
                format!("in · {} out", fmt_rate(s.tx_bps)),
                None,
            ),
            (
                "Fullest disk",
                worst_disk.map(|d| format!("{:.0}%", d.pct())).unwrap_or("–".into()),
                worst_disk.map(|d| d.mount.clone()).unwrap_or_default(),
                worst_disk.and_then(|d| w::level(d.pct(), 85.0, 95.0)),
            ),
            ("Containers", containers, container_sub, container_state),
        ],
    );

    w::card(ui, |ui| {
        w::section(ui, "CPU usage", "all cores");
        let series = vec![Series::new("CPU", 0, points(state, |p| p.cpu.total as f64))];
        let mut opt = ChartOptions::new(190.0, w::fmt_pct);
        opt.y_max = Some(100.0);
        opt.fill = true;
        w::chart(ui, "ov-cpu", range, series, opt);
    });
    ui.columns(2, |cols| {
        w::card(&mut cols[0], |ui| {
            w::section(ui, "Memory used", &format!("of {}", fmt_bytes(s.mem_total)));
            let series = vec![Series::new("Memory", 0, points(state, |p| p.mem_used as f64 / GIB))];
            let mut opt = ChartOptions::new(170.0, w::fmt_gib);
            opt.y_max = Some(s.mem_total as f64 / GIB);
            opt.fill = true;
            w::chart(ui, "ov-mem", range, series, opt);
        });
        w::card(&mut cols[1], |ui| {
            w::section(ui, "Network throughput", "physical interfaces");
            let series = vec![
                Series::new("Received", 0, points(state, |p| p.rx_bps * 8.0 / 1e6)),
                Series::new("Sent", 1, points(state, |p| p.tx_bps * 8.0 / 1e6)),
            ];
            w::chart(ui, "ov-net", range, series, ChartOptions::new(170.0, w::fmt_mbit));
        });
    });

    filesystems_card(ui, s);
}

fn filesystems_card(ui: &mut Ui, s: &Sample) {
    w::card(ui, |ui| {
        w::section(ui, "Filesystems", "");
        let mut rows: Vec<&Disk> = s.disks.iter().collect();
        if let Some((column, descending)) = w::sort_choice(ui, "filesystems") {
            rows.sort_by(|a, b| {
                let order = match column {
                    0 => a.mount.cmp(&b.mount),
                    1 => a.fstype.cmp(&b.fstype),
                    2 | 3 => a.pct().total_cmp(&b.pct()),
                    _ => a.total.cmp(&b.total),
                };
                w::directed(order, descending)
            });
        }
        let heads = ["Mount", "Type", "Used", "", "Size"];
        w::table(ui, "filesystems", &heads, &[0, 1], rows.len(), |i, c| {
            let d = rows[i];
            c.text(d.mount.as_str());
            c.text(w::muted(&d.fstype));
            c.cell(|ui| w::meter(ui, d.pct() / 100.0, 260.0));
            c.text(format!("{:.0}%", d.pct()));
            c.text(w::muted(format!("{} of {}", fmt_bytes(d.used), fmt_bytes(d.total))));
        });
        if s.swap_total > 0 {
            ui.label(w::muted(format!(
                "Swap: {} of {} used",
                fmt_bytes(s.swap_used),
                fmt_bytes(s.swap_total)
            )));
        }
    });
}

fn cpu_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let cores = s.cores.len().max(1) as f32;
    w::tiles(
        ui,
        &[
            (
                "Total",
                format!("{:.0}%", s.cpu.total),
                "busy".into(),
                w::level(s.cpu.total, 85.0, 95.0),
            ),
            ("User", format!("{:.0}%", s.cpu.user), "applications".into(), None),
            (
                "System",
                format!("{:.0}%", s.cpu.system),
                "kernel and interrupts".into(),
                None,
            ),
            (
                "I/O wait",
                format!("{:.0}%", s.cpu.iowait),
                "idle, waiting for disk".into(),
                w::level(s.cpu.iowait, 15.0, 40.0),
            ),
            (
                "Steal",
                format!("{:.0}%", s.cpu.steal),
                "taken by the hypervisor".into(),
                w::level(s.cpu.steal, 10.0, 30.0),
            ),
            (
                "Temperature",
                s.temp.map(|t| format!("{t:.0} °C")).unwrap_or("–".into()),
                if s.temp.is_some() {
                    "hottest sensor".into()
                } else {
                    "no sensor exposed".into()
                },
                s.temp.and_then(|t| w::level(t, 80.0, 90.0)),
            ),
        ],
    );
    w::card(ui, |ui| {
        w::section(ui, "CPU time by state", "share of all cores");
        let series = vec![
            Series::new("User", 0, points(state, |p| p.cpu.user as f64)),
            Series::new("System", 1, points(state, |p| p.cpu.system as f64)),
            Series::new("I/O wait", 2, points(state, |p| p.cpu.iowait as f64)),
            Series::new("Steal", 3, points(state, |p| p.cpu.steal as f64)),
        ];
        let mut opt = ChartOptions::new(220.0, w::fmt_pct);
        opt.y_max = Some(100.0);
        w::chart(ui, "cpu-states", range, series, opt);
    });
    ui.columns(2, |cols| {
        w::card(&mut cols[0], |ui| {
            w::section(ui, "Load average", &format!("dashed line: {cores:.0} cores"));
            if s.info.is_local_windows {
                ui.add_space(70.0);
                ui.vertical_centered(|ui| ui.label(w::muted("Windows does not keep a load average.")));
                ui.add_space(86.0);
                return;
            }
            let series = vec![
                Series::new("1 min", 0, points(state, |p| p.load[0] as f64)),
                Series::new("5 min", 1, points(state, |p| p.load[1] as f64)),
                Series::new("15 min", 2, points(state, |p| p.load[2] as f64)),
            ];
            let mut opt = ChartOptions::new(180.0, w::fmt_plain);
            opt.reference = Some((cores as f64, "Cores"));
            w::chart(ui, "cpu-load", range, series, opt);
        });
        w::card(&mut cols[1], |ui| {
            w::section(ui, "Temperature", "hottest sensor");
            let pts: Vec<[f64; 2]> = state
                .history
                .iter()
                .filter_map(|p| Some([p.ts as f64, p.temp? as f64]))
                .collect();
            if pts.is_empty() {
                ui.add_space(70.0);
                ui.vertical_centered(|ui| ui.label(w::muted("This machine exposes no temperature sensor.")));
                ui.add_space(86.0);
            } else {
                w::chart(
                    ui,
                    "cpu-temp",
                    range,
                    vec![Series::new("Temperature", 0, pts)],
                    ChartOptions::new(180.0, w::fmt_temp),
                );
            }
        });
    });
}

/// One small chart per core: more than eight lines in one plot would be unreadable.
fn cores_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let busiest = s.cores.iter().copied().fold(0.0, f32::max);
    let idlest = s.cores.iter().copied().fold(100.0, f32::min);
    let avg_freq = if s.freqs.is_empty() {
        None
    } else {
        Some(s.freqs.iter().sum::<f32>() / s.freqs.len() as f32)
    };
    w::tiles(
        ui,
        &[
            ("Cores", s.cores.len().to_string(), s.info.cpu_model.clone(), None),
            (
                "Busiest core",
                format!("{busiest:.0}%"),
                "now".into(),
                w::level(busiest, 95.0, 101.0),
            ),
            ("Idlest core", format!("{idlest:.0}%"), "now".into(), None),
            (
                "Clock",
                avg_freq.map(|f| format!("{:.2} GHz", f / 1000.0)).unwrap_or("–".into()),
                "average across cores".into(),
                None,
            ),
        ],
    );
    if s.info.no_per_core {
        w::card(ui, |ui| {
            ui.label(RichText::new("This Mac does not report usage per core").strong());
            ui.label(w::muted(
                "gridwatch reads the per-core counters through python3, which comes with the Xcode command line tools 
                 (xcode-select --install) or Homebrew. Without it only the total on the CPU tab is available.",
            ));
        });
        return;
    }
    let columns = ((ui.available_width() / 270.0) as usize).clamp(1, 8);
    let indices: Vec<usize> = (0..s.cores.len()).collect();
    for row in indices.chunks(columns) {
        ui.columns(columns, |cols| {
            for (col, &i) in cols.iter_mut().zip(row) {
                w::card(col, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Core {i}")).strong());
                        ui.label(format!("{:.0}%", s.cores[i]));
                        if let Some(f) = s.freqs.get(i) {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(w::muted(format!("{f:.0} MHz")));
                            });
                        }
                    });
                    let pts = state
                        .history
                        .iter()
                        .filter_map(|p| Some([p.ts as f64, *p.cores.get(i)? as f64]))
                        .collect();
                    let mut opt = ChartOptions::new(72.0, w::fmt_pct);
                    opt.y_max = Some(100.0);
                    opt.fill = true;
                    opt.compact = true;
                    w::chart(
                        ui,
                        &format!("core-{i}"),
                        range,
                        vec![Series::new(format!("Core {i}"), 0, pts)],
                        opt,
                    );
                });
            }
        });
    }
}

fn memory_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let total = s.mem_total.max(1);
    let used_pct = s.mem_used as f32 / total as f32 * 100.0;
    let swap_pct = s.swap_used as f32 / s.swap_total.max(1) as f32 * 100.0;
    w::tiles(
        ui,
        &[
            (
                "Used",
                format!("{used_pct:.0}%"),
                format!("{} of {}", fmt_bytes(s.mem_used), fmt_bytes(s.mem_total)),
                w::level(used_pct, 90.0, 95.0),
            ),
            (
                "Available",
                fmt_bytes(s.mem_total.saturating_sub(s.mem_used)),
                "free, or freed on demand".into(),
                None,
            ),
            (
                "Cache",
                fmt_bytes(s.mem_cached),
                "files in memory, freed on demand".into(),
                None,
            ),
            if s.swap_total > 0 {
                (
                    "Swap",
                    format!("{swap_pct:.0}%"),
                    format!("{} of {}", fmt_bytes(s.swap_used), fmt_bytes(s.swap_total)),
                    w::level(swap_pct, 50.0, 90.0),
                )
            } else {
                (
                    "Swap",
                    "none".to_string(),
                    "this machine has no swap space".into(),
                    None,
                )
            },
        ],
    );

    w::card(ui, |ui| {
        w::section(
            ui,
            "Where the memory is",
            &format!("{} installed", fmt_bytes(s.mem_total)),
        );
        w::stacked_bar(
            ui,
            &[
                ("Used by programs", s.mem_used as f64, w::SERIES[0]),
                ("Cache", s.mem_cached as f64, w::SERIES[2]),
                ("Free", s.mem_free as f64, w::AXIS),
            ],
            |bytes| fmt_bytes(bytes as u64),
        );
    });

    ui.columns(2, |cols| {
        w::card(&mut cols[0], |ui| {
            w::section(ui, "Memory over time", "");
            let series = vec![
                Series::new("Used by programs", 0, points(state, |p| p.mem_used as f64 / GIB)),
                Series::new("Cache", 2, points(state, |p| p.mem_cached as f64 / GIB)),
            ];
            let mut opt = ChartOptions::new(200.0, w::fmt_gib);
            opt.y_max = Some(s.mem_total as f64 / GIB);
            w::chart(ui, "mem-time", range, series, opt);
        });
        w::card(&mut cols[1], |ui| {
            w::section(
                ui,
                "Swap used",
                &if s.swap_total > 0 {
                    format!("of {}", fmt_bytes(s.swap_total))
                } else {
                    String::new()
                },
            );
            if s.swap_total == 0 {
                ui.add_space(86.0);
                ui.vertical_centered(|ui| ui.label(w::muted("This machine has no swap space.")));
                ui.add_space(106.0);
                return;
            }
            let series = vec![Series::new("Swap", 1, points(state, |p| p.swap_used as f64 / GIB))];
            let mut opt = ChartOptions::new(200.0, w::fmt_gib);
            opt.y_max = Some(s.swap_total as f64 / GIB);
            opt.fill = true;
            w::chart(ui, "mem-swap", range, series, opt);
        });
    });

    let largest = s.procs.iter().map(|p| p.rss).max().unwrap_or(1).max(1);
    let mut procs: Vec<&Proc> = s.procs.iter().filter(|p| p.rss > 0).collect();
    procs.sort_by_key(|p| std::cmp::Reverse(p.rss));
    procs.truncate(15);
    w::card(ui, |ui| {
        w::section(ui, "Largest processes", "by memory they hold");
        if let Some((column, descending)) = w::sort_choice(ui, "mem-procs") {
            procs.sort_by(|a, b| {
                let order = match column {
                    0 => a.pid.cmp(&b.pid),
                    1 => a.user.cmp(&b.user),
                    2 | 3 => a.rss.cmp(&b.rss),
                    4 => a.mem_pct.total_cmp(&b.mem_pct),
                    _ => a.cmd.cmp(&b.cmd),
                };
                w::directed(order, descending)
            });
        }
        let heads = ["PID", "User", "Memory", "", "Share", "Command"];
        w::table(ui, "mem-procs", &heads, &[1, 5], procs.len(), |i, c| {
            let p = procs[i];
            c.text(RichText::new(p.pid.to_string()).monospace());
            c.text(p.user.as_str());
            c.text(fmt_bytes(p.rss));
            c.cell(|ui| w::bar(ui, p.rss as f32 / largest as f32, 140.0, w::SERIES[0]));
            c.text(format!("{:.1}%", p.mem_pct));
            c.cell(|ui| {
                ui.label(RichText::new(&p.cmd).monospace()).on_hover_text(&p.cmd);
            });
        });
    });

    if !s.mem_detail.is_empty() {
        w::card(ui, |ui| {
            w::section(ui, "Details", "as the operating system counts it; the entries overlap");
            let mut rows: Vec<&(String, u64)> = s.mem_detail.iter().collect();
            if let Some((column, descending)) = w::sort_choice(ui, "mem-detail") {
                rows.sort_by(|a, b| w::directed(if column == 0 { a.0.cmp(&b.0) } else { a.1.cmp(&b.1) }, descending));
            }
            w::table(
                ui,
                "mem-detail",
                &["Kind", "Size", "", "Of all memory"],
                &[0],
                rows.len(),
                |i, c| {
                    let (kind, bytes) = rows[i];
                    c.text(kind.as_str());
                    c.text(fmt_bytes(*bytes));
                    c.cell(|ui| w::bar(ui, *bytes as f32 / total as f32, 200.0, w::SERIES[0]));
                    c.text(format!("{:.1}%", *bytes as f64 / total as f64 * 100.0));
                },
            );
        });
    }
}

fn hardware_tab(ui: &mut Ui, state: &ServerState) {
    let Some(hardware) = &state.hardware else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(w::muted("Reading what is built into this machine…"));
        });
        return;
    };
    if hardware.is_empty() {
        w::card(ui, |ui| {
            ui.label(RichText::new("This machine does not tell what it is made of").strong());
            ui.label(w::muted(
                "None of the usual tools (lscpu, lsblk, lspci, system_profiler) answered.",
            ));
        });
    }
    for group in hardware {
        w::card(ui, |ui| {
            w::section(ui, &group.title, &group.note);
            // A single thing is a list of facts; several of a kind are a table.
            if let [item] = group.items.as_slice() {
                if !item.name.is_empty() {
                    ui.label(RichText::new(&item.name).strong());
                }
                egui::Grid::new(("hardware", &group.title))
                    .num_columns(2)
                    .spacing([28.0, 6.0])
                    .show(ui, |ui| {
                        for (key, value) in &item.props {
                            ui.label(w::muted(key));
                            ui.label(value);
                            ui.end_row();
                        }
                    });
                return;
            }
            let mut keys: Vec<&str> = Vec::new();
            for (key, _) in group.items.iter().flat_map(|item| &item.props) {
                if !keys.contains(&key.as_str()) {
                    keys.push(key);
                }
            }
            let value = |item: &crate::hardware::HwItem, column: usize| -> String {
                match column {
                    0 => item.name.clone(),
                    n => item
                        .props
                        .iter()
                        .find(|(k, _)| k == keys[n - 1])
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default(),
                }
            };
            let mut heads = vec!["Name"];
            heads.extend(&keys);
            let id = format!("hardware-{}", group.title);
            let mut rows: Vec<&crate::hardware::HwItem> = group.items.iter().collect();
            if let Some((column, descending)) = w::sort_choice(ui, &id) {
                rows.sort_by(|a, b| w::directed(value(a, column).cmp(&value(b, column)), descending));
            }
            let text: Vec<usize> = (0..heads.len()).collect();
            w::table(ui, &id, &heads, &text, rows.len(), |i, c| {
                c.text(RichText::new(&rows[i].name).strong());
                for column in 1..heads.len() {
                    let text = value(rows[i], column);
                    c.text(if text.is_empty() { "–".to_string() } else { text });
                }
            });
        });
    }
}

fn processes_tab(ui: &mut Ui, s: &Sample, filter: &mut String) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(filter)
                .hint_text("Filter by command, user or PID")
                .desired_width(280.0),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if s.zombies > 0 {
                w::status(ui, w::WARNING, &format!("{} zombies", s.zombies));
            }
            ui.label(w::muted(format!(
                "{} processes · 100% CPU = one full core",
                s.proc_count
            )));
        });
    });
    let needle = filter.to_lowercase();
    let mut procs: Vec<&Proc> = s
        .procs
        .iter()
        .filter(|p| {
            needle.is_empty()
                || p.cmd.to_lowercase().contains(&needle)
                || p.user.to_lowercase().contains(&needle)
                || p.pid.to_string() == needle
        })
        .collect();
    // Busiest first unless a column was clicked.
    procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss.cmp(&a.rss)));
    w::card(ui, |ui| {
        if let Some((column, descending)) = w::sort_choice(ui, "procs") {
            procs.sort_by(|a, b| {
                let order = match column {
                    0 => a.pid.cmp(&b.pid),
                    1 => a.user.cmp(&b.user),
                    2 | 3 => a.cpu.total_cmp(&b.cpu),
                    4 => a.mem_pct.total_cmp(&b.mem_pct),
                    5 => a.rss.cmp(&b.rss),
                    6 => a.elapsed.cmp(&b.elapsed),
                    _ => a.cmd.cmp(&b.cmd),
                };
                w::directed(order, descending)
            });
        }
        let heads = ["PID", "User", "CPU", "", "Memory", "RSS", "Running", "Command"];
        w::table(ui, "procs", &heads, &[1, 7], procs.len().min(80), |i, c| {
            let p = procs[i];
            c.text(RichText::new(p.pid.to_string()).monospace());
            c.text(p.user.as_str());
            c.text(format!("{:.1}%", p.cpu));
            c.cell(|ui| w::bar(ui, p.cpu / 100.0, 90.0, w::SERIES[0]));
            c.text(format!("{:.1}%", p.mem_pct));
            c.text(fmt_bytes(p.rss));
            c.text(w::muted(fmt_duration(p.elapsed)));
            c.cell(|ui| {
                // Cut off at the edge of the window; the tooltip has the whole command.
                let color = if p.state == 'Z' { w::MUTED } else { w::TEXT };
                ui.label(RichText::new(&p.cmd).monospace().color(color))
                    .on_hover_text(&p.cmd);
            });
        });
        if procs.is_empty() {
            ui.label(w::muted("No process matches the filter."));
        }
    });
}

const MIB: f64 = (1u64 << 20) as f64;

fn network_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let ports = s.net.iter().filter(|n| n.physical).count();
    let busiest = s
        .net
        .iter()
        .filter(|n| n.physical)
        .max_by(|a, b| (a.rx_bps + a.tx_bps).total_cmp(&(b.rx_bps + b.tx_bps)));
    w::tiles(
        ui,
        &[
            (
                "Received",
                fmt_rate(s.rx_bps),
                format!("{ports} physical port{}", if ports == 1 { "" } else { "s" }),
                None,
            ),
            (
                "Sent",
                fmt_rate(s.tx_bps),
                format!("{ports} physical port{}", if ports == 1 { "" } else { "s" }),
                None,
            ),
            (
                "Busiest port",
                busiest.map(|n| n.name.clone()).unwrap_or("–".into()),
                busiest
                    .map(|n| format!("{} in · {} out", fmt_rate(n.rx_bps), fmt_rate(n.tx_bps)))
                    .unwrap_or_default(),
                None,
            ),
            (
                "Interfaces",
                s.net.len().to_string(),
                format!("{} virtual", s.net.len() - ports),
                None,
            ),
        ],
    );

    // Real ports first; an interface keeps its colour in both charts.
    let mut names: Vec<&NetIface> = s
        .net
        .iter()
        .filter(|n| {
            state
                .history
                .back()
                .is_some_and(|p| p.nets.iter().any(|h| h.0 == n.name))
        })
        .collect();
    names.sort_by_key(|n| !n.physical);
    names.truncate(w::SERIES.len());
    let series = |f: fn(&(String, f64, f64)) -> f64| -> Vec<Series> {
        names
            .iter()
            .enumerate()
            .map(|(slot, n)| {
                let pts = state
                    .history
                    .iter()
                    .filter_map(|p| Some([p.ts as f64, f(p.nets.iter().find(|h| h.0 == n.name)?) * 8.0 / 1e6]))
                    .collect();
                Series::new(n.name.clone(), slot, pts)
            })
            .collect()
    };
    ui.columns(2, |cols| {
        w::card(&mut cols[0], |ui| {
            w::section(ui, "Received by interface", "");
            w::chart(
                ui,
                "net-rx",
                range,
                series(|h| h.1),
                ChartOptions::new(210.0, w::fmt_mbit),
            );
        });
        w::card(&mut cols[1], |ui| {
            w::section(ui, "Sent by interface", "");
            w::chart(
                ui,
                "net-tx",
                range,
                series(|h| h.2),
                ChartOptions::new(210.0, w::fmt_mbit),
            );
        });
    });

    w::card(ui, |ui| {
        w::section(
            ui,
            "Interfaces",
            "rates since the last poll, totals since the interface came up",
        );
        if s.net.is_empty() {
            ui.label(w::muted("This machine reports no interface counters."));
            return;
        }
        let top = s.net.iter().map(|n| n.rx_bps.max(n.tx_bps)).fold(1.0, f64::max);
        // Real ports on top, then whatever carried the most.
        let mut rows: Vec<&NetIface> = s.net.iter().collect();
        rows.sort_by(|a, b| {
            b.physical
                .cmp(&a.physical)
                .then((b.rx_total + b.tx_total).cmp(&(a.rx_total + a.tx_total)))
        });
        if let Some((column, descending)) = w::sort_choice(ui, "interfaces") {
            rows.sort_by(|a, b| {
                let order = match column {
                    0 => a.name.cmp(&b.name),
                    1 | 2 => a.rx_bps.total_cmp(&b.rx_bps),
                    3 | 4 => a.tx_bps.total_cmp(&b.tx_bps),
                    5 => a.rx_total.cmp(&b.rx_total),
                    6 => a.tx_total.cmp(&b.tx_total),
                    _ => a.physical.cmp(&b.physical),
                };
                w::directed(order, descending)
            });
        }
        let heads = [
            "Interface",
            "Received",
            "",
            "Sent",
            "",
            "Total received",
            "Total sent",
            "Kind",
        ];
        w::table(ui, "interfaces", &heads, &[0], rows.len(), |i, c| {
            let n = rows[i];
            c.text(RichText::new(&n.name).strong());
            c.text(fmt_rate(n.rx_bps));
            c.cell(|ui| w::bar(ui, (n.rx_bps / top) as f32, 110.0, w::SERIES[0]));
            c.text(fmt_rate(n.tx_bps));
            c.cell(|ui| w::bar(ui, (n.tx_bps / top) as f32, 110.0, w::SERIES[1]));
            c.text(fmt_bytes(n.rx_total));
            c.text(fmt_bytes(n.tx_total));
            c.text(w::muted(if n.physical {
                "port"
            } else {
                "virtual, not in the totals"
            }));
        });
    });
}

fn disks_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let read: f64 = s.disk_io.iter().map(|d| d.read_bps).sum();
    let write: f64 = s.disk_io.iter().map(|d| d.write_bps).sum();
    let iops: f64 = s.disk_io.iter().map(|d| d.iops).sum();
    let busiest = s
        .disk_io
        .iter()
        .filter(|d| d.util.is_some())
        .max_by(|a, b| a.util.unwrap_or(0.0).total_cmp(&b.util.unwrap_or(0.0)));
    let fullest = s.disks.iter().max_by(|a, b| a.pct().total_cmp(&b.pct()));
    let busiest_label = busiest
        .map(|d| format!("Busiest disk · {}", d.name))
        .unwrap_or_default();
    let count = format!(
        "{} disk{}",
        s.disk_io.len(),
        if s.disk_io.len() == 1 { "" } else { "s" }
    );
    w::tiles(
        ui,
        &[
            ("Read", fmt_bytes_rate(read), count.clone(), None),
            ("Write", fmt_bytes_rate(write), count, None),
            match busiest {
                Some(d) => {
                    let util = d.util.unwrap_or(0.0);
                    (
                        busiest_label.as_str(),
                        format!("{util:.0}%"),
                        "share of time busy".into(),
                        w::level(util, 85.0, 95.0),
                    )
                }
                None if iops > 0.0 => (
                    "Operations",
                    format!("{iops:.0}"),
                    "reads and writes per second".into(),
                    None,
                ),
                None => (
                    "Disks",
                    s.disk_io.len().to_string(),
                    "with transfer counters".into(),
                    None,
                ),
            },
            (
                "Fullest filesystem",
                fullest.map(|d| format!("{:.0}%", d.pct())).unwrap_or("–".into()),
                fullest.map(|d| d.mount.clone()).unwrap_or_default(),
                fullest.and_then(|d| w::level(d.pct(), 85.0, 95.0)),
            ),
        ],
    );

    let transfer = |ui: &mut Ui| {
        w::card(ui, |ui| {
            w::section(ui, "Disk transfer", "all physical disks");
            let series = vec![
                Series::new("Read", 0, points(state, |p| p.disk_read_bps / MIB)),
                Series::new("Write", 1, points(state, |p| p.disk_write_bps / MIB)),
            ];
            w::chart(
                ui,
                "disk-transfer",
                range,
                series,
                ChartOptions::new(200.0, w::fmt_mib_s),
            );
        });
    };
    let util: Vec<[f64; 2]> = state
        .history
        .iter()
        .filter_map(|p| Some([p.ts as f64, p.disk_util? as f64]))
        .collect();
    if util.is_empty() {
        transfer(ui);
    } else {
        ui.columns(2, |cols| {
            transfer(&mut cols[0]);
            w::card(&mut cols[1], |ui| {
                w::section(ui, "Busiest disk", "share of time with requests pending");
                let mut opt = ChartOptions::new(200.0, w::fmt_pct);
                opt.y_max = Some(100.0);
                opt.fill = true;
                w::chart(ui, "disk-util", range, vec![Series::new("Busy", 0, util)], opt);
            });
        });
    }

    w::card(ui, |ui| {
        w::section(ui, "Disks", "transfer since the last poll");
        if s.disk_io.is_empty() {
            ui.label(w::muted("This machine reports no disk counters."));
            return;
        }
        // Windows and macOS report bytes only: no operation counts, no busy time.
        let detailed = s.disk_io.iter().any(|d| d.util.is_some());
        let heads: &[&str] = if detailed {
            &["Disk", "Read", "Write", "Operations / s", "Busy", ""]
        } else {
            &["Disk", "Read", "Write"]
        };
        let mut rows: Vec<&DiskIo> = s.disk_io.iter().collect();
        if let Some((column, descending)) = w::sort_choice(ui, "disk-io") {
            rows.sort_by(|a, b| {
                let order = match column {
                    0 => a.name.cmp(&b.name),
                    1 => a.read_bps.total_cmp(&b.read_bps),
                    2 => a.write_bps.total_cmp(&b.write_bps),
                    3 => a.iops.total_cmp(&b.iops),
                    _ => a.util.unwrap_or(0.0).total_cmp(&b.util.unwrap_or(0.0)),
                };
                w::directed(order, descending)
            });
        }
        w::table(ui, "disk-io", heads, &[0], rows.len(), |i, c| {
            let d = rows[i];
            c.text(RichText::new(&d.name).strong());
            c.text(fmt_bytes_rate(d.read_bps));
            c.text(fmt_bytes_rate(d.write_bps));
            if !detailed {
                return;
            }
            c.text(format!("{:.0}", d.iops));
            match d.util {
                Some(util) => {
                    c.text(format!("{util:.0}%"));
                    c.cell(|ui| w::meter(ui, util / 100.0, 220.0));
                }
                None => {
                    c.text(w::muted("–"));
                    c.text(w::muted("not reported by this system"));
                }
            }
        });
    });
    filesystems_card(ui, s);
}

fn temps_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    if s.sensors.is_empty() {
        w::card(ui, |ui| {
            ui.label(RichText::new("This machine exposes no temperature sensors").strong());
            ui.label(w::muted(if s.info.is_mac {
                "macOS keeps its sensors private. Install macmon on the Mac (brew install macmon) and gridwatch reads CPU and GPU temperature through it."
            } else if s.info.is_local_windows {
                "Windows does not hand temperatures to ordinary programs."
            } else {
                "Virtual machines have none. On real hardware the sensor drivers may be missing: sudo sensors-detect."
            }));
        });
        return;
    }
    let chips = chip_temps(&s.sensors);
    let mut rows: Vec<(String, String, String, w::Level)> = Vec::new();
    for kind in [SensorKind::Cpu, SensorKind::Gpu, SensorKind::Disk, SensorKind::Other] {
        let hottest = s
            .sensors
            .iter()
            .filter(|x| x.kind() == kind)
            .max_by(|a, b| a.temp.total_cmp(&b.temp));
        if let Some(x) = hottest {
            let (warm, hot) = x.limits();
            let name = if x.label.is_empty() {
                "hottest reading".to_string()
            } else {
                x.label.clone()
            };
            // Mainboard inputs are often not connected and read nonsense: shown, not judged.
            let state = if kind == SensorKind::Other {
                None
            } else {
                w::level(x.temp, warm, hot)
            };
            rows.push((
                format!("{} · {}", kind.label(), x.chip),
                format!("{:.0} °C", x.temp),
                name,
                state,
            ));
        }
    }
    rows.push((
        "Sensors".to_string(),
        s.sensors.len().to_string(),
        format!("on {} chip{}", chips.len(), if chips.len() == 1 { "" } else { "s" }),
        None,
    ));
    let tiles: Vec<w::Tile> = rows
        .iter()
        .map(|r| (r.0.as_str(), r.1.clone(), r.2.clone(), r.3))
        .collect();
    w::tiles(ui, &tiles);

    w::card(ui, |ui| {
        let hint = if chips.len() > w::SERIES.len() {
            "hottest reading of the first 8 chips"
        } else {
            "hottest reading of each chip"
        };
        w::section(ui, "Temperature by chip", hint);
        let series = chips
            .iter()
            .take(w::SERIES.len())
            .enumerate()
            .map(|(slot, (chip, _))| {
                let pts = state
                    .history
                    .iter()
                    .filter_map(|p| Some([p.ts as f64, p.temps.iter().find(|t| t.0 == *chip)?.1 as f64]))
                    .collect();
                Series::new(chip.clone(), slot, pts)
            })
            .collect();
        w::chart(ui, "temps", range, series, ChartOptions::new(240.0, w::fmt_temp));
    });

    w::card(ui, |ui| {
        w::section(ui, "All sensors", "");
        let mut rows: Vec<&Sensor> = s.sensors.iter().collect();
        if let Some((column, descending)) = w::sort_choice(ui, "sensors") {
            rows.sort_by(|a, b| {
                let order = match column {
                    0 => a.chip.cmp(&b.chip),
                    1 => a.label.cmp(&b.label),
                    2 => a.kind().label().cmp(b.kind().label()),
                    _ => a.temp.total_cmp(&b.temp),
                };
                w::directed(order, descending)
            });
        }
        let heads = ["Chip", "Reading", "Kind", "Temperature", ""];
        w::table(ui, "sensors", &heads, &[0, 1, 2], rows.len(), |i, c| {
            let x = rows[i];
            let (warm, hot) = x.limits();
            c.text(RichText::new(&x.chip).strong());
            c.text(if x.label.is_empty() { "–" } else { x.label.as_str() });
            c.text(w::muted(x.kind().label()));
            c.text(format!("{:.1} °C", x.temp));
            match w::level(x.temp, warm, hot).filter(|_| x.kind() != SensorKind::Other) {
                Some((color, label)) => c.cell(|ui| w::status(ui, color, label)),
                None => c.text(""),
            }
        });
        let sata = s.disk_io.iter().any(|d| d.name.starts_with("sd"));
        if sata && !s.info.is_mac && !s.sensors.iter().any(|x| x.chip.starts_with("drivetemp")) {
            ui.add_space(4.0);
            ui.label(w::muted("SATA disks report their temperature once the drivetemp kernel module is loaded: sudo modprobe drivetemp"));
        }
    });
}

fn gpu_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let busiest = s.gpus.iter().map(|g| g.util).fold(0.0, f32::max);
    let used: u64 = s.gpus.iter().map(|g| g.mem_used).sum();
    let total: u64 = s.gpus.iter().map(|g| g.mem_total).sum();
    let hottest = s.gpus.iter().filter_map(|g| g.temp).reduce(f32::max);
    let power: Option<f32> = s.gpus.iter().filter_map(|g| g.power).reduce(|a, b| a + b);
    let name = if s.gpus.len() == 1 {
        s.gpus[0].name.clone()
    } else {
        format!("busiest of {} GPUs", s.gpus.len())
    };
    w::tiles(
        ui,
        &[
            ("GPU load", format!("{busiest:.0}%"), name, None),
            if total > 0 {
                let pct = used as f32 / total as f32 * 100.0;
                (
                    "GPU memory",
                    format!("{pct:.0}%"),
                    format!("{} of {}", fmt_bytes(used), fmt_bytes(total)),
                    w::level(pct, 95.0, 101.0),
                )
            } else {
                (
                    "GPU memory",
                    fmt_bytes(used),
                    "shared with the system memory".into(),
                    None,
                )
            },
            (
                "Temperature",
                hottest.map(|t| format!("{t:.0} °C")).unwrap_or("–".into()),
                if hottest.is_some() {
                    "hottest GPU".into()
                } else {
                    "not reported".into()
                },
                hottest.and_then(|t| w::level(t, 85.0, 95.0)),
            ),
            (
                "Power",
                power
                    .map(|p| {
                        if p >= 10.0 {
                            format!("{p:.0} W")
                        } else {
                            format!("{p:.1} W")
                        }
                    })
                    .unwrap_or("–".into()),
                if power.is_some() {
                    "drawn right now".into()
                } else {
                    "not reported".into()
                },
                None,
            ),
        ],
    );

    let series = |f: fn(&(f32, u64)) -> f64| -> Vec<Series> {
        (0..s.gpus.len().min(w::SERIES.len()))
            .map(|i| {
                let pts = state
                    .history
                    .iter()
                    .filter_map(|p| Some([p.ts as f64, f(p.gpus.get(i)?)]))
                    .collect();
                Series::new(format!("GPU {i}"), i, pts)
            })
            .collect()
    };
    ui.columns(2, |cols| {
        w::card(&mut cols[0], |ui| {
            w::section(ui, "GPU load", "share of time busy");
            let mut opt = ChartOptions::new(200.0, w::fmt_pct);
            opt.y_max = Some(100.0);
            opt.fill = s.gpus.len() == 1;
            w::chart(ui, "gpu-load", range, series(|g| g.0 as f64), opt);
        });
        w::card(&mut cols[1], |ui| {
            w::section(
                ui,
                "GPU memory used",
                &if total > 0 {
                    format!("of {}", fmt_bytes(total))
                } else {
                    "shared with the system memory".to_string()
                },
            );
            let mut opt = ChartOptions::new(200.0, w::fmt_gib);
            if s.gpus.len() == 1 && total > 0 {
                opt.y_max = Some(total as f64 / GIB);
            }
            opt.fill = s.gpus.len() == 1;
            w::chart(ui, "gpu-mem", range, series(|g| g.1 as f64 / GIB), opt);
        });
    });

    w::card(ui, |ui| {
        let mut rows: Vec<(usize, &Gpu)> = s.gpus.iter().enumerate().collect();
        if let Some((column, descending)) = w::sort_choice(ui, "gpus") {
            rows.sort_by(|(i, a), (j, b)| {
                let order = match column {
                    0 => i.cmp(j),
                    1 => a.name.cmp(&b.name),
                    2 | 3 => a.util.total_cmp(&b.util),
                    4 => a.mem_used.cmp(&b.mem_used),
                    5 => a.temp.unwrap_or(0.0).total_cmp(&b.temp.unwrap_or(0.0)),
                    _ => a.power.unwrap_or(0.0).total_cmp(&b.power.unwrap_or(0.0)),
                };
                w::directed(order, descending)
            });
        }
        let heads = ["#", "Name", "Load", "", "Memory", "Temperature", "Power"];
        w::table(ui, "gpus", &heads, &[0, 1], rows.len(), |row, c| {
            let (i, g) = rows[row];
            c.text(i.to_string());
            c.text(RichText::new(&g.name).strong());
            c.text(format!("{:.0}%", g.util));
            c.cell(|ui| w::bar(ui, g.util / 100.0, 160.0, w::SERIES[i % w::SERIES.len()]));
            c.text(if g.mem_total > 0 {
                format!("{} of {}", fmt_bytes(g.mem_used), fmt_bytes(g.mem_total))
            } else {
                format!("{} (shared)", fmt_bytes(g.mem_used))
            });
            c.text(g.temp.map(|t| format!("{t:.0} °C")).unwrap_or("–".into()));
            c.text(g.power.map(|p| format!("{p:.1} W")).unwrap_or("–".into()));
        });
    });
}

fn container_status(c: &Container) -> (Color32, &'static str) {
    match c.state.as_str() {
        "running" if c.unhealthy() => (w::WARNING, "unhealthy"),
        "running" => (w::GOOD, "running"),
        "restarting" => (w::CRITICAL, "restarting"),
        _ if c.failed() => (w::WARNING, "failed"),
        "exited" => (w::MUTED, "stopped"),
        "paused" => (w::MUTED, "paused"),
        _ => (w::MUTED, "created"),
    }
}

fn docker_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64), slots: &mut Vec<String>) {
    let list = match &state.docker {
        DockerState::Ok(list) => list,
        other => {
            w::card(ui, |ui| match other {
                DockerState::NotInstalled => {
                    ui.label(RichText::new("Docker is not installed on this machine").strong());
                    ui.label(w::muted(
                        "The docker command was not found in the PATH of the SSH user.",
                    ));
                }
                DockerState::Error(msg) => {
                    w::status(ui, w::WARNING, "Docker is installed but could not be queried");
                    ui.label(RichText::new(msg).color(w::TEXT_SECONDARY));
                    ui.label(w::muted("Either the docker daemon is not running, or the SSH user is not in the docker group (sudo usermod -aG docker <user>)."));
                }
                _ => {
                    ui.label(w::muted("Waiting for the first docker measurement…"));
                }
            });
            return;
        }
    };

    let running = list.iter().filter(|c| c.state == "running").count();
    let failing = list
        .iter()
        .filter(|c| matches!(container_status(c).1, "restarting" | "failed" | "unhealthy"))
        .count();
    let cpu: f32 = list.iter().map(|c| c.cpu).sum();
    let mem: u64 = list.iter().map(|c| c.mem_used).sum();
    w::tiles(
        ui,
        &[
            (
                "Running",
                running.to_string(),
                format!("of {} containers", list.len()),
                None,
            ),
            (
                "Need attention",
                failing.to_string(),
                "restarting, failed or unhealthy".into(),
                (failing > 0).then_some((w::CRITICAL, "see Suggestions")),
            ),
            (
                "Container CPU",
                format!("{cpu:.0}%"),
                format!("of {}% ({} cores)", s.cores.len() * 100, s.cores.len()),
                None,
            ),
            (
                "Container memory",
                fmt_bytes(mem),
                format!("of {} host memory", fmt_bytes(s.mem_total)),
                None,
            ),
        ],
    );

    // The heaviest containers get a chart colour, and keep it for as long as the app runs.
    let weight = |name: &String| -> f64 {
        let Some(h) = state.container_history.get(name).filter(|h| !h.is_empty()) else {
            return 0.0;
        };
        let n = h.len() as f64;
        let cpu = h.iter().map(|p| p.1 as f64).sum::<f64>() / n / (s.cores.len().max(1) as f64 * 100.0);
        let mem = h.iter().map(|p| p.2 as f64).sum::<f64>() / n / s.mem_total.max(1) as f64;
        cpu + mem
    };
    let mut names: Vec<&String> = state.container_history.keys().collect();
    names.sort_by(|a, b| weight(b).total_cmp(&weight(a)).then(a.cmp(b)));
    for name in names {
        if slots.len() < w::SERIES.len() && !slots.contains(name) {
            slots.push(name.clone());
        }
    }
    let series = |f: fn(&ContainerPoint) -> f64| -> Vec<Series> {
        slots
            .iter()
            .enumerate()
            .filter_map(|(slot, name)| {
                let history = state.container_history.get(name)?;
                Some(Series::new(
                    name.clone(),
                    slot,
                    history.iter().map(|p| [p.0 as f64, f(p)]).collect(),
                ))
            })
            .collect()
    };
    let uncharted = state.container_history.len().saturating_sub(slots.len());
    let hint = if uncharted > 0 {
        format!("{} heaviest; {uncharted} more in the table", slots.len())
    } else {
        String::new()
    };
    ui.columns(2, |cols| {
        w::card(&mut cols[0], |ui| {
            w::section(
                ui,
                "CPU by container",
                if hint.is_empty() { "100% = one core" } else { &hint },
            );
            w::chart(
                ui,
                "docker-cpu",
                range,
                series(|p| p.1 as f64),
                ChartOptions::new(200.0, w::fmt_pct),
            );
        });
        w::card(&mut cols[1], |ui| {
            w::section(ui, "Memory by container", &hint);
            w::chart(
                ui,
                "docker-mem",
                range,
                series(|p| p.2 as f64 / GIB),
                ChartOptions::new(200.0, w::fmt_gib),
            );
        });
    });

    // Running containers first, busiest on top.
    let mut rows: Vec<&Container> = list.iter().collect();
    rows.sort_by(|a, b| {
        b.has_stats
            .cmp(&a.has_stats)
            .then(b.cpu.total_cmp(&a.cpu))
            .then(a.name.cmp(&b.name))
    });
    w::card(ui, |ui| {
        ScrollArea::horizontal().show(ui, |ui| {
            container_table(ui, &rows, s);
        });
    });
}

fn container_table(ui: &mut Ui, rows: &[&Container], s: &Sample) {
    let mut rows = rows.to_vec();
    if let Some((column, descending)) = w::sort_choice(ui, "containers") {
        rows.sort_by(|a, b| {
            let order = match column {
                0 => container_status(a).1.cmp(container_status(b).1),
                1 => a.name.cmp(&b.name),
                2 => a.image.cmp(&b.image),
                3 => a.cpu.total_cmp(&b.cpu),
                4 | 5 => a.mem_used.cmp(&b.mem_used),
                6 => a.net.cmp(&b.net),
                7 => a.pids.cmp(&b.pids),
                _ => a.status.cmp(&b.status),
            };
            w::directed(order, descending)
        });
    }
    let heads = [
        "State", "Name", "Image", "CPU", "Memory", "", "Net I/O", "PIDs", "Status",
    ];
    w::table(ui, "containers", &heads, &[0, 1, 2, 6, 8], rows.len(), |i, c| {
        let container = rows[i];
        let (color, label) = container_status(container);
        c.cell(|ui| w::status(ui, color, label));
        c.text(RichText::new(&container.name).strong());
        c.cell(|ui| {
            let image: String = container.image.chars().take(36).collect();
            ui.label(w::muted(image)).on_hover_text(&container.image);
        });
        if container.has_stats {
            c.text(format!("{:.1}%", container.cpu));
            let limit = container.mem_limit;
            if limit > 0 && limit < s.mem_total.saturating_sub(s.mem_total / 20) {
                c.text(format!("{} / {}", fmt_bytes(container.mem_used), fmt_bytes(limit)));
                c.cell(|ui| w::meter(ui, container.mem_used as f32 / limit as f32, 80.0));
            } else {
                c.text(fmt_bytes(container.mem_used));
                c.text(w::muted("no limit"));
            }
            c.text(w::muted(&container.net));
            c.text(container.pids.to_string());
        } else {
            for _ in 0..5 {
                c.text(w::muted("–"));
            }
        }
        c.text(w::muted(&container.status));
    });
}

fn suggestions_tab(ui: &mut Ui, suggestions: &[Suggestion]) {
    for s in suggestions {
        w::card(ui, |ui| {
            let color = match s.severity {
                Severity::Critical => w::CRITICAL,
                Severity::Warning => w::WARNING,
                Severity::Info => w::SERIES[0],
                Severity::Good => w::GOOD,
            };
            ui.horizontal(|ui| {
                w::status(ui, color, s.severity.label());
                ui.label(RichText::new(&s.title).size(15.0).strong());
            });
            ui.label(RichText::new(&s.detail).color(w::TEXT_SECONDARY));
            if !s.action.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(w::muted("What to do:  "));
                    // Text in backticks is a command.
                    for (i, part) in s.action.split('`').enumerate() {
                        if i % 2 == 1 {
                            ui.label(RichText::new(part).monospace().background_color(w::GRID));
                        } else {
                            ui.label(part);
                        }
                    }
                });
            }
        });
    }
}
