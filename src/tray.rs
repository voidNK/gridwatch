//! The icon in the notification area (Windows) or menu bar (macOS). It shows the worst
//! state of all servers and brings the window back after it was closed to the tray.
//! Linux desktops have no common tray any more, so there the icon is left out.

/// What the icon says about the servers as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Nothing measured yet.
    Unknown,
    Good,
    Warning,
    Critical,
}

/// What the user asked for with a click on the icon or its menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Left click: show the window, or hide it if it is in front.
    Toggle,
    Show,
    ToggleAlerts,
    Quit,
}

#[cfg(any(windows, target_os = "macos"))]
mod native {
    use super::{Action, Level};
    use eframe::egui;
    use std::sync::mpsc;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct Tray {
        icon: TrayIcon,
        alerts: CheckMenuItem,
        actions: mpsc::Receiver<Action>,
        shown: Option<(Level, String, bool)>,
    }

    /// The application icon with a dot in the colour of the state, as RGBA pixels.
    fn picture(level: Level) -> Option<Icon> {
        const SIZE: u32 = 64;
        let source = image::load_from_memory(include_bytes!("../packaging/icon.png")).ok()?;
        let mut picture =
            image::imageops::resize(&source.to_rgba8(), SIZE, SIZE, image::imageops::FilterType::Triangle);
        let dot = match level {
            Level::Unknown => Some([150, 150, 145]),
            // All well: the plain icon says it.
            Level::Good => None,
            Level::Warning => Some([250, 178, 25]),
            Level::Critical => Some([221, 59, 59]),
        };
        if let Some([r, g, b]) = dot {
            let (cx, cy, radius) = (46.0f32, 46.0f32, 17.0f32);
            for (x, y, pixel) in picture.enumerate_pixels_mut() {
                let distance = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                if distance <= radius - 3.0 {
                    *pixel = image::Rgba([r, g, b, 255]);
                } else if distance <= radius {
                    // A dark ring keeps the dot apart from the lights behind it.
                    *pixel = image::Rgba([20, 20, 20, 255]);
                }
            }
        }
        Icon::from_rgba(picture.into_raw(), SIZE, SIZE).ok()
    }

    impl Tray {
        /// Must be called on the main thread. Returns `None` if the system has no tray.
        pub fn new(ctx: &egui::Context, alerts_on: bool) -> Option<Self> {
            let show = MenuItem::with_id("show", "Show gridwatch", true, None);
            let alerts = CheckMenuItem::with_id("alerts", "Alerts", true, alerts_on, None);
            let quit = MenuItem::with_id("quit", "Quit", true, None);
            let menu = Menu::new();
            menu.append_items(&[&show, &alerts, &PredefinedMenuItem::separator(), &quit])
                .ok()?;
            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("gridwatch")
                .with_icon(picture(Level::Unknown)?)
                .build()
                .ok()?;

            // Clicks arrive on the event loop; hand them to the next frame and ask for one.
            let (sender, actions) = mpsc::channel();
            let (clicks, repaint) = (sender.clone(), ctx.clone());
            TrayIconEvent::set_event_handler(Some(move |event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    let _ = clicks.send(Action::Toggle);
                    repaint.request_repaint();
                }
            }));
            let repaint = ctx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = match event.id().as_ref() {
                    "show" => Action::Show,
                    "alerts" => Action::ToggleAlerts,
                    "quit" => Action::Quit,
                    _ => return,
                };
                let _ = sender.send(action);
                repaint.request_repaint();
            }));
            Some(Self {
                icon,
                alerts,
                actions,
                shown: None,
            })
        }

        /// What was clicked since the last frame.
        pub fn poll(&mut self) -> Vec<Action> {
            self.actions.try_iter().collect()
        }

        /// Show the state of the servers; cheap when nothing changed.
        pub fn update(&mut self, level: Level, tooltip: &str, alerts_on: bool) {
            let now = (level, tooltip.to_string(), alerts_on);
            if self.shown.as_ref() == Some(&now) {
                return;
            }
            if self.shown.as_ref().map(|s| s.0) != Some(level) {
                let _ = self.icon.set_icon(picture(level));
            }
            let _ = self.icon.set_tooltip(Some(tooltip));
            self.alerts.set_checked(alerts_on);
            self.shown = Some(now);
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            TrayIconEvent::set_event_handler::<fn(TrayIconEvent)>(None);
            MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod native {
    use super::{Action, Level};
    use eframe::egui;

    /// Stands in where there is no tray: it can never be created.
    pub struct Tray;

    impl Tray {
        pub fn new(_ctx: &egui::Context, _alerts_on: bool) -> Option<Self> {
            None
        }

        pub fn poll(&mut self) -> Vec<Action> {
            Vec::new()
        }

        pub fn update(&mut self, _level: Level, _tooltip: &str, _alerts_on: bool) {}
    }
}

pub use native::Tray;
