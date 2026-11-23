//! gridwatch - a desktop dashboard that watches Linux and macOS machines over SSH.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod app;
mod collector;
mod demo;
mod hardware;
mod instance;
#[cfg(windows)]
mod local_windows;
mod model;
mod notify;
mod parse;
mod secrets;
mod ssh;
mod store;
mod suggest;
mod tray;
mod update;
mod widgets;

use eframe::egui;
use std::path::PathBuf;

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Command line options.
#[derive(Default)]
pub struct Args {
    /// Show simulated servers instead of the configured ones.
    pub demo: bool,
    /// Save a PNG of every tab into this directory, then exit.
    pub screenshot_dir: Option<PathBuf>,
    /// Seconds to wait for data before the first screenshot.
    pub screenshot_delay: f32,
}

const HELP: &str = "gridwatch - monitor Linux and macOS machines over SSH

USAGE: gridwatch [OPTIONS]

  --demo               show simulated servers (no SSH connection, nothing is saved)
  --screenshot <DIR>   save a PNG of every tab into DIR, then exit
  --delay <SECONDS>    wait this long for data before the first screenshot
  --update             install the newest version, if there is one, and exit
  -V, --version        print the version
  -h, --help           print this help";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        screenshot_delay: 1.0,
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--demo" => args.demo = true,
            "--screenshot" => args.screenshot_dir = Some(it.next().ok_or("--screenshot needs a directory")?.into()),
            "--delay" => {
                args.screenshot_delay = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--delay needs a number of seconds")?;
            }
            "--update" => {
                return Err(match update::check() {
                    Ok(Some(release)) => match update::install(&release) {
                        Ok(()) => format!("gridwatch {} is installed.", release.version),
                        Err(e) => format!("Update to {} failed: {e:#}", release.version),
                    },
                    Ok(None) => concat!("gridwatch ", env!("CARGO_PKG_VERSION"), " is the newest version.").to_string(),
                    Err(e) => format!("Cannot look for updates: {e:#}"),
                });
            }
            "-V" | "--version" => return Err(concat!("gridwatch ", env!("CARGO_PKG_VERSION")).to_string()),
            "-h" | "--help" => return Err(HELP.to_string()),
            other => return Err(format!("unknown option {other}\n\n{HELP}")),
        }
    }
    Ok(args)
}

/// The icon of the window and the taskbar, from the PNG that is also packaged.
fn icon() -> egui::IconData {
    match image::load_from_memory(include_bytes!("../packaging/icon.png")) {
        Ok(picture) => {
            let picture = picture.to_rgba8();
            let (width, height) = picture.dimensions();
            egui::IconData {
                rgba: picture.into_raw(),
                width,
                height,
            }
        }
        Err(_) => egui::IconData::default(),
    }
}

fn main() -> eframe::Result {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            println!("{msg}");
            return Ok(());
        }
    };
    // One running copy is enough: starting gridwatch again brings the first one to the front.
    if !args.demo
        && args.screenshot_dir.is_none()
        && let Ok(paths) = store::Paths::resolve()
        && instance::wake_running(&paths.instance)
    {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    std::thread::spawn(update::sync_bundle_version);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("gridwatch")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native(
        "gridwatch",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)?))),
    )
}
