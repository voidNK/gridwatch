//! The window: server list on the left, tabs for the selected server on the right.

use crate::collector::{self, Env};
use crate::model::*;
use crate::store::{Config, Paths, Store};
use crate::suggest::{self, Severity, Suggestion};
use crate::widgets::{self as w, ChartOptions, Series};
use crate::{Args, demo, now_ts};
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
const INTERVALS: [u64; 5] = [2, 5, 10, 30, 60];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Overview,
    Cpu,
    Cores,
    Processes,
    Docker,
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
    confirm_remove: bool,
    secret_input: String,
    proc_filter: String,
    proc_by_memory: bool,
    last_prune: Instant,
    screenshots: Option<Screenshots>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> anyhow::Result<Self> {
        w::apply_theme(&cc.egui_ctx);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
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
        });
        let _ = env.store.prune(config.retention_days, now_ts());

        let screenshots = args.screenshot_dir.map(|dir| Screenshots {
            dir,
            shots: vec![
                (0, Tab::Overview, "overview.png"),
                (0, Tab::Cpu, "cpu.png"),
                (0, Tab::Cores, "cores.png"),
                (0, Tab::Processes, "processes.png"),
                (0, Tab::Docker, "docker.png"),
                (if args.demo { 2 } else { 0 }, Tab::Suggestions, "suggestions.png"),
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
            confirm_remove: false,
            secret_input: String::new(),
            proc_filter: String::new(),
            proc_by_memory: false,
            last_prune: Instant::now(),
            screenshots,
            config,
        };
        for (i, cfg) in app.config.servers.clone().into_iter().enumerate() {
            let server = app.start(cfg, i);
            app.servers.push(server);
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
        let containers = self.env.store.load_container_history(id, since, now).unwrap_or_default();
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
        if let Some(paths) = &self.paths {
            if let Err(e) = self.config.save(&paths.config) {
                eprintln!("gridwatch: cannot save config: {e:#}");
            }
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
        let server = self.servers.remove(self.selected);
        server.task.abort();
        let _ = self.env.store.delete_server(&server.cfg.id);
        self.secrets.remove(&server.cfg.id);
        self.selected = self.selected.min(self.servers.len().saturating_sub(1));
        self.save_config();
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
            1 => AuthMethod::KeyFile { path: d.key_path.trim().to_string() },
            2 => AuthMethod::Password,
            _ => AuthMethod::Auto,
        };
        let name = if d.name.trim().is_empty() { host.clone() } else { d.name.trim().to_string() };
        let editing = d.editing;
        let secret = std::mem::take(&mut d.secret);
        let id = editing.map(|i| self.servers[i].cfg.id.clone()).unwrap_or_else(ServerConfig::new_id);
        let cfg = ServerConfig { id: id.clone(), name, host, port, user, auth };

        if secret.is_empty() {
            if editing.is_some_and(|i| self.servers[i].cfg.auth != cfg.auth) {
                self.secrets.remove(&id);
            }
        } else {
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
        shots.frames += 1;
        if !shots.requested {
            if shots.frames > 20 && shots.since.elapsed() >= shots.delay {
                let path = shots.dir.join(file);
                let done = shots.done.clone();
                let _ = std::fs::create_dir_all(&shots.dir);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                    move |image: Arc<egui::ColorImage>| {
                        let [width, height] = image.size;
                        if let Err(e) = image::save_buffer(&path, image.as_raw(), width as u32, height as u32, image::ColorType::Rgba8) {
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
        self.remove_window(&ctx);
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("gridwatch").size(18.0).strong());
            if self.is_demo() {
                ui.label(w::muted("demo data"));
            }
            ui.add_space(18.0);
            ui.label(w::muted("Range"));
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
                if !self.is_demo() && ui.button("+ Add server").clicked() {
                    self.open_dialog(None);
                }
                if let Some(paths) = &self.paths {
                    ui.label(w::muted(format!("history kept {} days", self.config.retention_days)))
                        .on_hover_text(format!(
                            "Config: {}\nHistory: {}",
                            paths.config.display(),
                            paths.database.display()
                        ));
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.label(w::muted(format!("SERVERS ({})", self.servers.len())));
        ui.add_space(2.0);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, server) in self.servers.iter().enumerate() {
                let state = server.shared.lock().unwrap();
                let selected = i == self.selected;
                let alerts = suggest::alert_count(&suggest::analyze(&state));
                let response = Frame::new()
                    .fill(if selected { w::SURFACE } else { w::PAGE })
                    .stroke(egui::Stroke::new(1.0, if selected { w::AXIS } else { w::GRID }))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&server.cfg.name).strong());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let (color, label) = conn_status(&state);
                                w::status(ui, color, label);
                            });
                        });
                        ui.label(w::muted(server.cfg.address()));
                        if let Some(s) = &state.latest {
                            let mem = s.mem_used as f32 / s.mem_total.max(1) as f32;
                            for (label, fraction) in [("CPU", s.cpu.total / 100.0), ("MEM", mem)] {
                                ui.horizontal(|ui| {
                                    ui.label(w::muted(label));
                                    w::meter(ui, fraction, 120.0);
                                    ui.label(RichText::new(format!("{:.0}%", fraction * 100.0)).color(w::TEXT_SECONDARY));
                                });
                            }
                            if alerts > 0 {
                                w::status(ui, w::WARNING, &format!("{alerts} alert{}", if alerts == 1 { "" } else { "s" }));
                            }
                        }
                    })
                    .response
                    .interact(Sense::click());
                if response.clicked() {
                    self.selected = i;
                    self.secret_input.clear();
                }
                ui.add_space(2.0);
            }
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.servers.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.label(RichText::new("No servers yet").size(22.0).strong());
                ui.label(w::muted("Add a Linux machine you can reach over SSH. Nothing needs to be installed on it."));
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
                    if !demo {
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
                            let field = ui.add(egui::TextEdit::singleline(&mut self.secret_input).password(true).desired_width(260.0));
                            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (ui.button("Connect").clicked() || enter) && !self.secret_input.is_empty() {
                                reconnect = true;
                            }
                        });
                        ui.label(w::muted("Kept in memory until gridwatch is closed; never written to disk."));
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
                        (Tab::Cpu, "CPU".to_string()),
                        (Tab::Cores, format!("Cores ({})", sample.cores.len())),
                        (Tab::Processes, format!("Processes ({})", sample.proc_count)),
                        (Tab::Docker, "Docker".to_string()),
                        (Tab::Suggestions, if alerts > 0 { format!("Suggestions ({alerts})") } else { "Suggestions".to_string() }),
                    ] {
                        ui.selectable_value(&mut self.tab, tab, label);
                    }
                });
                ui.add_space(4.0);
                let tab = self.tab;
                let (filter, by_memory) = (&mut self.proc_filter, &mut self.proc_by_memory);
                let slots = &mut server.color_slots;
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match tab {
                    Tab::Overview => overview_tab(ui, &state, sample, range),
                    Tab::Cpu => cpu_tab(ui, &state, sample, range),
                    Tab::Cores => cores_tab(ui, &state, sample, range),
                    Tab::Processes => processes_tab(ui, sample, filter, by_memory),
                    Tab::Docker => docker_tab(ui, &state, sample, range, slots),
                    Tab::Suggestions => suggestions_tab(ui, &suggestions),
                });
            }
        }

        if edit {
            self.open_dialog(Some(index));
        }
        if reconnect {
            let id = self.servers[index].cfg.id.clone();
            self.secrets.insert(id, std::mem::take(&mut self.secret_input));
            self.restart(index);
        }
    }

    fn dialog_window(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.dialog else { return };
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(if d.editing.is_some() { "Edit server" } else { "Add server" })
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("server-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("optional, defaults to the host").desired_width(280.0));
                    ui.end_row();
                    ui.label("Host");
                    ui.add(egui::TextEdit::singleline(&mut d.host).hint_text("hostname, IP or user@host").desired_width(280.0));
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut d.port).desired_width(70.0));
                    ui.end_row();
                    ui.label("User");
                    ui.add(egui::TextEdit::singleline(&mut d.user).desired_width(280.0));
                    ui.end_row();
                    ui.label("Authentication");
                    let kinds = ["SSH agent / default keys", "Key file", "Password"];
                    egui::ComboBox::from_id_salt("auth").width(280.0).selected_text(kinds[d.auth]).show_ui(ui, |ui| {
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
                        ui.add(egui::TextEdit::singleline(&mut d.secret).password(true).hint_text("kept in memory only").desired_width(280.0));
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
            let broken = list.iter().filter(|c| c.state == "restarting" || c.status.contains("unhealthy")).count();
            (
                running.to_string(),
                format!("running, {} stopped", list.len() - running - broken.min(list.len() - running)),
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
            ("CPU", format!("{:.0}%", s.cpu.total), format!("{:.0} cores", cores), w::level(s.cpu.total, 85.0, 95.0)),
            (
                "Memory",
                format!("{mem_pct:.0}%"),
                format!("{} of {}", fmt_bytes(s.mem_used), fmt_bytes(s.mem_total)),
                w::level(mem_pct, 90.0, 95.0),
            ),
            (
                "Load (1 / 5 / 15 min)",
                format!("{:.2}", s.load[0]),
                format!("{:.2} / {:.2}", s.load[1], s.load[2]),
                w::level(s.load[1] / cores * 100.0, 150.0, 300.0),
            ),
            ("Network", fmt_rate(s.rx_bps), format!("in · {} out", fmt_rate(s.tx_bps)), None),
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

    w::card(ui, |ui| {
        w::section(ui, "Filesystems", "");
        egui::Grid::new("disks").num_columns(5).spacing([24.0, 8.0]).show(ui, |ui| {
            for d in &s.disks {
                ui.label(&d.mount);
                ui.label(w::muted(&d.fstype));
                w::meter(ui, d.pct() / 100.0, 260.0);
                ui.label(format!("{:.0}%", d.pct()));
                ui.label(w::muted(format!("{} of {}", fmt_bytes(d.used), fmt_bytes(d.total))));
                ui.end_row();
            }
        });
        if s.swap_total > 0 {
            ui.label(w::muted(format!("Swap: {} of {} used", fmt_bytes(s.swap_used), fmt_bytes(s.swap_total))));
        }
    });
}

fn cpu_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let cores = s.cores.len().max(1) as f32;
    w::tiles(
        ui,
        &[
            ("Total", format!("{:.0}%", s.cpu.total), "busy".into(), w::level(s.cpu.total, 85.0, 95.0)),
            ("User", format!("{:.0}%", s.cpu.user), "applications".into(), None),
            ("System", format!("{:.0}%", s.cpu.system), "kernel and interrupts".into(), None),
            ("I/O wait", format!("{:.0}%", s.cpu.iowait), "idle, waiting for disk".into(), w::level(s.cpu.iowait, 15.0, 40.0)),
            ("Steal", format!("{:.0}%", s.cpu.steal), "taken by the hypervisor".into(), w::level(s.cpu.steal, 10.0, 30.0)),
            (
                "Temperature",
                s.temp.map(|t| format!("{t:.0} °C")).unwrap_or("–".into()),
                if s.temp.is_some() { "hottest sensor".into() } else { "no sensor exposed".into() },
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
            let pts: Vec<[f64; 2]> = state.history.iter().filter_map(|p| Some([p.ts as f64, p.temp? as f64])).collect();
            if pts.is_empty() {
                ui.add_space(70.0);
                ui.vertical_centered(|ui| ui.label(w::muted("This machine exposes no temperature sensor.")));
                ui.add_space(86.0);
            } else {
                w::chart(ui, "cpu-temp", range, vec![Series::new("Temperature", 0, pts)], ChartOptions::new(180.0, w::fmt_temp));
            }
        });
    });
}

/// One small chart per core: more than eight lines in one plot would be unreadable.
fn cores_tab(ui: &mut Ui, state: &ServerState, s: &Sample, range: (i64, i64)) {
    let busiest = s.cores.iter().copied().fold(0.0, f32::max);
    let idlest = s.cores.iter().copied().fold(100.0, f32::min);
    let avg_freq = if s.freqs.is_empty() { None } else { Some(s.freqs.iter().sum::<f32>() / s.freqs.len() as f32) };
    w::tiles(
        ui,
        &[
            ("Cores", s.cores.len().to_string(), s.info.cpu_model.clone(), None),
            ("Busiest core", format!("{busiest:.0}%"), "now".into(), w::level(busiest, 95.0, 101.0)),
            ("Idlest core", format!("{idlest:.0}%"), "now".into(), None),
            (
                "Clock",
                avg_freq.map(|f| format!("{:.2} GHz", f / 1000.0)).unwrap_or("–".into()),
                "average across cores".into(),
                None,
            ),
        ],
    );
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
                    w::chart(ui, &format!("core-{i}"), range, vec![Series::new(format!("Core {i}"), 0, pts)], opt);
                });
            }
        });
    }
}

fn processes_tab(ui: &mut Ui, s: &Sample, filter: &mut String, by_memory: &mut bool) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(filter).hint_text("Filter by command, user or PID").desired_width(280.0));
        ui.label(w::muted("Sort by"));
        ui.selectable_value(by_memory, false, "CPU");
        ui.selectable_value(by_memory, true, "Memory");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if s.zombies > 0 {
                w::status(ui, w::WARNING, &format!("{} zombies", s.zombies));
            }
            ui.label(w::muted(format!("{} processes · 100% CPU = one full core", s.proc_count)));
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
    if *by_memory {
        procs.sort_by(|a, b| b.rss.cmp(&a.rss));
    } else {
        procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss.cmp(&a.rss)));
    }
    w::card(ui, |ui| {
        egui::Grid::new("procs").num_columns(8).striped(true).spacing([18.0, 7.0]).show(ui, |ui| {
            for head in ["PID", "User", "CPU", "", "Memory", "RSS", "Running", "Command"] {
                ui.label(w::muted(head));
            }
            ui.end_row();
            for p in procs.iter().take(80) {
                ui.label(RichText::new(p.pid.to_string()).monospace());
                ui.label(&p.user);
                ui.label(format!("{:.1}%", p.cpu));
                w::bar(ui, p.cpu / 100.0, 90.0, w::SERIES[0]);
                ui.label(format!("{:.1}%", p.mem_pct));
                ui.label(fmt_bytes(p.rss));
                ui.label(w::muted(fmt_duration(p.elapsed)));
                let cmd: String = p.cmd.chars().take(84).collect();
                let label = ui.label(RichText::new(cmd).monospace().color(if p.state == 'Z' { w::MUTED } else { w::TEXT }));
                if p.cmd.chars().count() > 84 {
                    label.on_hover_text(&p.cmd);
                }
                ui.end_row();
            }
        });
        if procs.is_empty() {
            ui.label(w::muted("No process matches the filter."));
        }
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
                    ui.label(w::muted("The docker command was not found in the PATH of the SSH user."));
                }
                DockerState::Error(msg) => {
                    w::status(ui, w::WARNING, "Docker is installed but could not be queried");
                    ui.label(RichText::new(msg).color(w::TEXT_SECONDARY));
                    ui.label(w::muted("Usually the SSH user is not in the docker group: sudo usermod -aG docker <user>"));
                }
                _ => {
                    ui.label(w::muted("Waiting for the first docker measurement…"));
                }
            });
            return;
        }
    };

    let running = list.iter().filter(|c| c.state == "running").count();
    let failing = list.iter().filter(|c| matches!(container_status(c).1, "restarting" | "failed" | "unhealthy")).count();
    let cpu: f32 = list.iter().map(|c| c.cpu).sum();
    let mem: u64 = list.iter().map(|c| c.mem_used).sum();
    w::tiles(
        ui,
        &[
            ("Running", running.to_string(), format!("of {} containers", list.len()), None),
            (
                "Need attention",
                failing.to_string(),
                "restarting, failed or unhealthy".into(),
                (failing > 0).then_some((w::CRITICAL, "see Suggestions")),
            ),
            ("Container CPU", format!("{cpu:.0}%"), format!("of {}% ({} cores)", s.cores.len() * 100, s.cores.len()), None),
            ("Container memory", fmt_bytes(mem), format!("of {} host memory", fmt_bytes(s.mem_total)), None),
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
                Some(Series::new(name.clone(), slot, history.iter().map(|p| [p.0 as f64, f(p)]).collect()))
            })
            .collect()
    };
    let uncharted = state.container_history.len().saturating_sub(slots.len());
    let hint = if uncharted > 0 { format!("{} heaviest; {uncharted} more in the table", slots.len()) } else { String::new() };
    ui.columns(2, |cols| {
        w::card(&mut cols[0], |ui| {
            w::section(ui, "CPU by container", if hint.is_empty() { "100% = one core" } else { &hint });
            w::chart(ui, "docker-cpu", range, series(|p| p.1 as f64), ChartOptions::new(200.0, w::fmt_pct));
        });
        w::card(&mut cols[1], |ui| {
            w::section(ui, "Memory by container", &hint);
            w::chart(ui, "docker-mem", range, series(|p| p.2 as f64 / GIB), ChartOptions::new(200.0, w::fmt_gib));
        });
    });

    // Running containers first, busiest on top.
    let mut rows: Vec<&Container> = list.iter().collect();
    rows.sort_by(|a, b| b.has_stats.cmp(&a.has_stats).then(b.cpu.total_cmp(&a.cpu)).then(a.name.cmp(&b.name)));
    w::card(ui, |ui| {
        ScrollArea::horizontal().show(ui, |ui| {
            container_table(ui, &rows, s);
        });
    });
}

fn container_table(ui: &mut Ui, rows: &[&Container], s: &Sample) {
    {
        egui::Grid::new("containers").num_columns(9).striped(true).spacing([18.0, 8.0]).show(ui, |ui| {
            for head in ["State", "Name", "Image", "CPU", "Memory", "", "Net I/O", "PIDs", "Status"] {
                ui.label(w::muted(head));
            }
            ui.end_row();
            for c in rows {
                let (color, label) = container_status(c);
                w::status(ui, color, label);
                ui.label(RichText::new(&c.name).strong());
                let image: String = c.image.chars().take(36).collect();
                ui.label(w::muted(image)).on_hover_text(&c.image);
                if c.has_stats {
                    ui.label(format!("{:.1}%", c.cpu));
                    let limited = c.mem_limit > 0 && c.mem_limit < s.mem_total.saturating_sub(s.mem_total / 20);
                    if limited {
                        ui.label(format!("{} / {}", fmt_bytes(c.mem_used), fmt_bytes(c.mem_limit)));
                        w::meter(ui, c.mem_used as f32 / c.mem_limit as f32, 80.0);
                    } else {
                        ui.label(fmt_bytes(c.mem_used));
                        ui.label(w::muted("no limit"));
                    }
                    ui.label(w::muted(&c.net));
                    ui.label(c.pids.to_string());
                } else {
                    for _ in 0..5 {
                        ui.label(w::muted("–"));
                    }
                }
                ui.label(w::muted(&c.status));
                ui.end_row();
            }
        });
    }
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
