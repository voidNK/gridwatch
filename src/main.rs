//! gridwatch - a desktop dashboard that watches Linux and macOS machines over SSH.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod collector;
mod demo;
mod model;
mod notify;
mod parse;
mod ssh;
mod store;
mod suggest;
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
            "-h" | "--help" => return Err(HELP.to_string()),
            other => return Err(format!("unknown option {other}\n\n{HELP}")),
        }
    }
    Ok(args)
}

fn main() -> eframe::Result {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            println!("{msg}");
            return Ok(());
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("gridwatch")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "gridwatch",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)?))),
    )
}
