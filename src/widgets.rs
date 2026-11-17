//! Theme and the small building blocks every tab uses: cards, tiles, meters, charts.

use chrono::{Local, TimeZone};
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Sense, Stroke, Ui, Vec2};
use egui_plot::{GridInput, GridMark, HLine, HoverPosition, Line, Plot, PlotPoints};

// Dark palette. Series colours are assigned in this fixed order and identify
// an entity; status colours are reserved for state and always carry a label.
pub const PAGE: Color32 = Color32::from_rgb(0x0d, 0x0d, 0x0d);
pub const SURFACE: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x19);
pub const GRID: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x2a);
pub const AXIS: Color32 = Color32::from_rgb(0x38, 0x38, 0x35);
pub const TEXT: Color32 = Color32::from_rgb(0xf0, 0xef, 0xec);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xc3, 0xc2, 0xb7);
pub const MUTED: Color32 = Color32::from_rgb(0x89, 0x87, 0x81);
pub const SERIES: [Color32; 8] = [
    Color32::from_rgb(0x39, 0x87, 0xe5), // blue
    Color32::from_rgb(0xd9, 0x59, 0x26), // orange
    Color32::from_rgb(0x19, 0x9e, 0x70), // aqua
    Color32::from_rgb(0xc9, 0x85, 0x00), // yellow
    Color32::from_rgb(0xd5, 0x51, 0x81), // magenta
    Color32::from_rgb(0x00, 0x83, 0x00), // green
    Color32::from_rgb(0x90, 0x85, 0xe9), // violet
    Color32::from_rgb(0xe6, 0x67, 0x67), // red
];
pub const GOOD: Color32 = Color32::from_rgb(0x0c, 0xa3, 0x0c);
pub const WARNING: Color32 = Color32::from_rgb(0xfa, 0xb2, 0x19);
pub const CRITICAL: Color32 = Color32::from_rgb(0xd0, 0x3b, 0x3b);

pub fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PAGE;
    v.window_fill = SURFACE;
    v.extreme_bg_color = SURFACE;
    v.faint_bg_color = Color32::from_rgb(0x21, 0x21, 0x20);
    v.hyperlink_color = SERIES[0];
    v.selection.bg_fill = Color32::from_rgb(0x18, 0x4f, 0x95);
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.fg_stroke.color = TEXT;
    v.widgets.noninteractive.bg_stroke.color = GRID;
    v.widgets.inactive.fg_stroke.color = TEXT_SECONDARY;
    v.widgets.inactive.weak_bg_fill = GRID;
    v.widgets.inactive.bg_fill = GRID;
    v.widgets.hovered.weak_bg_fill = AXIS;
    v.widgets.hovered.bg_fill = AXIS;
    v.window_stroke = Stroke::new(1.0, AXIS);
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.global_style_mut(|style| {
        style.interaction.selectable_labels = false;
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
    });
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(MUTED)
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    Frame::new()
        .fill(SURFACE)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
}

/// Title row above a chart or table.
pub fn section(ui: &mut Ui, title: &str, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        if !hint.is_empty() {
            ui.label(muted(hint));
        }
    });
}

/// A coloured dot followed by a label, so state is never shown by colour alone.
pub fn status(ui: &mut Ui, color: Color32, label: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.5, color);
        ui.label(RichText::new(label).color(TEXT_SECONDARY));
    });
}

/// Headline number with a label and a line of context.
pub fn tile(ui: &mut Ui, label: &str, value: &str, sub: &str, state: Option<(Color32, &str)>) {
    card(ui, |ui| {
        ui.set_min_height(74.0);
        ui.label(muted(label));
        ui.label(RichText::new(value).size(24.0).strong());
        match state {
            Some((color, text)) => status(ui, color, text),
            None => {
                ui.label(muted(sub));
            }
        }
    });
}

pub fn tiles(ui: &mut Ui, items: &[(&str, String, String, Option<(Color32, &str)>)]) {
    ui.columns(items.len(), |cols| {
        for (col, (label, value, sub, state)) in cols.iter_mut().zip(items) {
            tile(col, label, value, sub, *state);
        }
    });
}

/// State for a utilisation percentage; `None` while it is unremarkable.
pub fn level(pct: f32, warn: f32, crit: f32) -> Option<(Color32, &'static str)> {
    if pct >= crit {
        Some((CRITICAL, "critical"))
    } else if pct >= warn {
        Some((WARNING, "high"))
    } else {
        None
    }
}

/// Horizontal meter. The fill turns to a status colour past the thresholds;
/// the number next to it carries the value.
pub fn meter(ui: &mut Ui, fraction: f32, width: f32) {
    let color = level(fraction * 100.0, 85.0, 95.0).map(|s| s.0).unwrap_or(SERIES[0]);
    bar(ui, fraction, width, color);
}

/// Horizontal bar without any status meaning.
pub fn bar(ui: &mut Ui, fraction: f32, width: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 8.0), Sense::hover());
    let fraction = fraction.clamp(0.0, 1.0);
    ui.painter().rect_filled(rect, CornerRadius::same(4), GRID);
    if fraction > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * fraction).max(4.0));
        ui.painter().rect_filled(fill, CornerRadius::same(4), color);
    }
}

pub struct Series {
    pub name: String,
    pub color: Color32,
    pub points: Vec<[f64; 2]>,
}

impl Series {
    pub fn new(name: impl Into<String>, slot: usize, points: Vec<[f64; 2]>) -> Self {
        Self {
            name: name.into(),
            color: SERIES[slot % SERIES.len()],
            points,
        }
    }
}

pub fn fmt_pct(v: f64) -> String {
    format!("{v:.0}%")
}

pub fn fmt_plain(v: f64) -> String {
    if v.fract().abs() < 1e-9 { format!("{v:.0}") } else { format!("{v:.1}") }
}

pub fn fmt_temp(v: f64) -> String {
    format!("{v:.0} °C")
}

pub fn fmt_gib(v: f64) -> String {
    if v >= 10.0 { format!("{v:.0} GiB") } else { format!("{v:.1} GiB") }
}

pub fn fmt_mbit(v: f64) -> String {
    if v >= 10.0 { format!("{v:.0} Mbit/s") } else { format!("{v:.1} Mbit/s") }
}

fn local_time(ts: f64, pattern: &str) -> String {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|t| t.format(pattern).to_string())
        .unwrap_or_default()
}

/// Grid lines on round wall-clock times instead of powers of ten.
fn time_marks(input: GridInput) -> Vec<GridMark> {
    const STEPS: [f64; 12] = [
        10.0, 30.0, 60.0, 300.0, 900.0, 1800.0, 3600.0, 10800.0, 21600.0, 43200.0, 86400.0, 172800.0,
    ];
    let (lo, hi) = input.bounds;
    let step = STEPS.iter().copied().find(|s| (hi - lo) / s <= 7.0).unwrap_or(172800.0);
    let offset = Local::now().offset().local_minus_utc() as f64;
    let mut value = ((lo + offset) / step).ceil() * step - offset;
    let mut marks = Vec::new();
    while value <= hi {
        marks.push(GridMark { value, step_size: step });
        value += step;
    }
    marks
}

pub struct ChartOptions {
    pub height: f32,
    /// Fixed upper bound of the y axis (e.g. 100 for percentages).
    pub y_max: Option<f64>,
    pub format: fn(f64) -> String,
    /// Fill the area under the line; only sensible for a single series.
    pub fill: bool,
    /// Hide axes and legend for small multiples.
    pub compact: bool,
    /// Optional labelled reference line.
    pub reference: Option<(f64, &'static str)>,
}

impl ChartOptions {
    pub fn new(height: f32, format: fn(f64) -> String) -> Self {
        Self {
            height,
            y_max: None,
            format,
            fill: false,
            compact: false,
            reference: None,
        }
    }
}

/// Time series chart. `range` is the visible time window in unix seconds.
pub fn chart(ui: &mut Ui, id: &str, range: (i64, i64), series: Vec<Series>, opt: ChartOptions) {
    let (start, end) = (range.0 as f64, range.1 as f64);
    let span = end - start;
    // Do not draw a line across a period without data (server offline, app closed).
    let gap = (span / 40.0).max(45.0);
    let format = opt.format;
    let hover_pattern = if span > 86400.0 { "%a %H:%M" } else { "%H:%M:%S" };

    let mut plot = Plot::new(id)
        .height(opt.height)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .show_background(false)
        .show_axes([!opt.compact, !opt.compact])
        .show_grid([false, true])
        .grid_color(GRID)
        .set_margin_fraction(Vec2::new(0.0, 0.06))
        .include_x(start)
        .include_x(end)
        .include_y(0.0)
        .x_grid_spacer(time_marks)
        .x_axis_formatter(move |mark, _| local_time(mark.value, if span > 86400.0 { "%a %H:%M" } else { "%H:%M" }))
        .y_axis_formatter(move |mark, _| format(mark.value))
        .y_axis_min_width(52.0)
        .link_cursor(egui::Id::unique("gridwatch-time"), [true, false])
        .label_formatter(move |pos| match pos {
            HoverPosition::NearDataPoint { plot_name, position, .. } => Some(format!(
                "{plot_name}\n{}\n{}",
                format(position.y),
                local_time(position.x, hover_pattern)
            )),
            HoverPosition::Elsewhere { .. } => None,
        });
    if let Some(max) = opt.y_max {
        plot = plot.include_y(max);
    }
    // The legend sits above the plot so it can never cover the data.
    if series.len() > 1 && !opt.compact {
        ui.horizontal_wrapped(|ui| {
            for s in &series {
                // Swatch and name are one widget, so they always wrap together.
                let mut name: String = s.name.chars().take(30).collect();
                if s.name.chars().count() > 30 {
                    name.push('…');
                }
                let font = egui::TextStyle::Body.resolve(ui.style());
                let galley = ui.painter().layout_no_wrap(name, font, TEXT_SECONDARY);
                let size = Vec2::new(14.0 + 6.0 + galley.size().x + 10.0, galley.size().y);
                let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
                let y = rect.center().y;
                ui.painter().line_segment(
                    [egui::pos2(rect.left(), y), egui::pos2(rect.left() + 14.0, y)],
                    Stroke::new(3.0, s.color),
                );
                ui.painter().galley(egui::pos2(rect.left() + 20.0, rect.top()), galley, TEXT_SECONDARY);
                response.on_hover_text(&s.name);
            }
        });
    }

    plot.show(ui, |plot_ui| {
        if let Some((value, name)) = opt.reference {
            plot_ui.hline(HLine::new(name, value).color(MUTED).width(1.0).style(egui_plot::LineStyle::dashed_dense()));
        }
        for s in series {
            let mut segment: Vec<[f64; 2]> = Vec::new();
            let mut flush = |segment: &mut Vec<[f64; 2]>| {
                if segment.is_empty() {
                    return;
                }
                let mut line = Line::new(s.name.clone(), PlotPoints::new(std::mem::take(segment)))
                    .color(s.color)
                    .width(2.0);
                if opt.fill {
                    line = line.fill(0.0).fill_alpha(0.12);
                }
                plot_ui.line(line);
            };
            for p in &s.points {
                if p[0] < start {
                    continue;
                }
                if segment.last().is_some_and(|last| p[0] - last[0] > gap) {
                    flush(&mut segment);
                }
                segment.push(*p);
            }
            flush(&mut segment);
        }
    });
}
