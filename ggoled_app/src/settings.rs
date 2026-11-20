//! The settings window. Runs as its own process (`ggoled_app --settings`),
//! started from the tray. It edits the config file; the tray app notices the
//! file changed and applies it, so nothing here talks to the device directly.

use crate::config::*;
use crate::schedule::{self, DAY_KEYS, DisplayMode, Segment};
use eframe::egui::{self, Color32, RichText, Ui};
use ggoled_ticker::{GridHeader, TickerMode};
use std::time::{Duration, Instant, SystemTime};

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ggoled settings")
            .with_inner_size([620.0, 680.0])
            .with_min_inner_size([520.0, 420.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "ggoled settings",
        options,
        Box::new(|_cc| Ok(Box::new(SettingsApp::new()))),
    ) {
        crate::show_warning(&format!("Couldn't open the settings window: {e}"));
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Display,
    Ticker,
    Schedule,
    Scarlett,
}

struct SettingsApp {
    /// What's in the file
    saved: Config,
    /// What's being edited
    edit: Config,
    tab: Tab,
    load_error: Option<String>,
    status: Option<(String, Instant)>,
    file_mtime: Option<SystemTime>,
    last_file_check: Instant,
    new_symbol: String,
    new_compact: String,
    scarlett_command: String,
}

fn file_mtime() -> Option<SystemTime> {
    std::fs::metadata(Config::path()).and_then(|m| m.modified()).ok()
}

impl SettingsApp {
    fn new() -> Self {
        let (config, load_error) = match Config::load() {
            Ok((config, _)) => (config, None),
            Err(e) => (Config::default(), Some(e)),
        };
        Self {
            scarlett_command: join_command(&config.scarlett.command),
            saved: config.clone(),
            edit: config,
            tab: Tab::Display,
            load_error,
            status: None,
            file_mtime: file_mtime(),
            last_file_check: Instant::now(),
            new_symbol: String::new(),
            new_compact: String::new(),
        }
    }

    fn dirty(&self) -> bool {
        self.edit != self.saved
    }

    fn save(&mut self) {
        match self.edit.save() {
            Ok(()) => {
                self.saved = self.edit.clone();
                self.file_mtime = file_mtime();
                self.load_error = None;
                self.status = Some(("Saved. ggoled applies it within a second.".into(), Instant::now()));
            }
            Err(e) => self.status = Some((format!("Couldn't save: {e}"), Instant::now())),
        }
    }

    /// Pick up changes made elsewhere (tray menu, hand edits) when nothing is
    /// being edited here.
    fn check_file(&mut self) {
        if self.last_file_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_file_check = Instant::now();
        let mtime = file_mtime();
        if mtime == self.file_mtime {
            return;
        }
        self.file_mtime = mtime;
        if self.dirty() {
            return; // keep the user's edits; saving will overwrite
        }
        if let Ok((config, _)) = Config::load() {
            self.scarlett_command = join_command(&config.scarlett.command);
            self.saved = config.clone();
            self.edit = config;
            self.load_error = None;
        }
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.check_file();
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        let validation = self.edit.validate();

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (tab, name) in [
                    (Tab::Display, "Display"),
                    (Tab::Ticker, "Ticker"),
                    (Tab::Schedule, "Schedule"),
                    (Tab::Scarlett, "Scarlett"),
                ] {
                    ui.selectable_value(&mut self.tab, tab, RichText::new(name).size(15.0));
                }
            });
            ui.add_space(2.0);
        });

        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_save = self.dirty() && validation.is_ok();
                if ui.add_enabled(can_save, egui::Button::new("Save")).clicked() {
                    self.save();
                }
                if ui
                    .add_enabled(self.dirty(), egui::Button::new("Undo changes"))
                    .clicked()
                {
                    self.edit = self.saved.clone();
                    self.scarlett_command = join_command(&self.edit.scarlett.command);
                }
                match &validation {
                    Err(e) => {
                        ui.colored_label(Color32::from_rgb(230, 90, 80), e);
                    }
                    Ok(_) if self.dirty() => {
                        ui.weak("Unsaved changes");
                    }
                    Ok(_) => {
                        if let Some((msg, t)) = &self.status
                            && t.elapsed() < Duration::from_secs(6)
                        {
                            ui.label(msg);
                        }
                    }
                }
            });
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(e) = &self.load_error {
                ui.colored_label(
                    Color32::from_rgb(230, 90, 80),
                    format!("{e}\n\nShowing defaults; saving will replace the file."),
                );
                ui.separator();
            }
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| match self.tab {
                    Tab::Display => display_tab(ui, &mut self.edit),
                    Tab::Ticker => ticker_tab(ui, &mut self.edit, &mut self.new_symbol, &mut self.new_compact),
                    Tab::Schedule => schedule_tab(ui, &mut self.edit),
                    Tab::Scarlett => scarlett_tab(ui, &mut self.edit, &mut self.scarlett_command),
                });
        });
    }
}

// ---------------------------------------------------------------------------
// Shared widgets

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).strong().size(15.0));
    ui.add_space(2.0);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).weak());
}

fn combo<T: PartialEq + Copy>(ui: &mut Ui, id: &str, value: &mut T, options: &[(T, &str)]) {
    let current = options.iter().find(|(v, _)| v == value).map(|(_, n)| *n).unwrap_or("?");
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(190.0)
        .show_ui(ui, |ui| {
            for (v, name) in options {
                ui.selectable_value(value, *v, *name);
            }
        });
}

const DISPLAY_OPTIONS: [(DisplayMode, &str); 4] = [
    (DisplayMode::Normal, "Clock & media"),
    (DisplayMode::Ticker, "Price ticker"),
    (DisplayMode::Blank, "Blank"),
    (DisplayMode::Stock, "SteelSeries screen"),
];
const TIME_OPTIONS: [(ConfigTimeMode, &str); 3] = [
    (ConfigTimeMode::Off, "Off"),
    (ConfigTimeMode::H24, "24-hour"),
    (ConfigTimeMode::H12, "12-hour"),
];

/// Seconds as a draggable value shown in a friendly unit.
fn seconds(ui: &mut Ui, value: &mut u32, range: std::ops::RangeInclusive<u32>) {
    ui.add(
        egui::DragValue::new(value)
            .range(range)
            .speed(1.0)
            .custom_formatter(|v, _| format_secs(v as u32))
            .custom_parser(parse_secs),
    );
}

fn format_secs(s: u32) -> String {
    match s {
        0 => "0 s".into(),
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{} min", s / 60),
        s if s >= 60 => format!("{} min {} s", s / 60, s % 60),
        s => format!("{s} s"),
    }
}

/// Accepts "90", "90s", "5m", "5 min", "1h", "1 min 30 s".
fn parse_secs(text: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut num = String::new();
    let mut any = false;
    let text = text.trim().to_ascii_lowercase();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        let n: f64 = num.parse().ok()?;
        num.clear();
        let mult = match c {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        // skip the rest of the unit word ("min", "sec", "hours")
        while chars.peek().is_some_and(|c| c.is_alphabetic()) {
            chars.next();
        }
        total += n * mult;
        any = true;
    }
    if !num.is_empty() {
        total += num.parse::<f64>().ok()?;
        any = true;
    }
    any.then_some(total)
}

// ---------------------------------------------------------------------------
// Tabs

fn display_tab(ui: &mut Ui, c: &mut Config) {
    heading(ui, "Screen");
    egui::Grid::new("display")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Show");
            combo(ui, "display", &mut c.display, &DISPLAY_OPTIONS);
            ui.end_row();

            ui.label("Clock");
            combo(ui, "time", &mut c.time_mode, &TIME_OPTIONS);
            ui.end_row();

            ui.label("Now playing");
            ui.checkbox(&mut c.show_media, "Show what's playing");
            ui.end_row();

            ui.label("Volume changes");
            ui.horizontal(|ui| {
                ui.label("Show the SteelSeries volume screen for");
                seconds(ui, &mut c.volume_overlay_secs, 0..=30);
            });
            ui.end_row();
        });
    hint(
        ui,
        "A schedule entry can change what's shown; these are the settings outside of it.",
    );

    heading(ui, "Screensaver and burn-in");
    egui::Grid::new("saver")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Screensaver");
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.idle_timeout, "Blank the screen after");
                ui.add_enabled_ui(c.idle_timeout, |ui| seconds(ui, &mut c.idle_timeout_secs, 5..=86400));
                ui.label("without keyboard/mouse input");
            });
            ui.end_row();

            ui.label("Burn-in protection");
            combo(
                ui,
                "shift",
                &mut c.oled_shift,
                &[
                    (ConfigShiftMode::Off, "Off"),
                    (ConfigShiftMode::Low, "Low (moves up to 1 px)"),
                    (ConfigShiftMode::High, "High (moves up to 4 px)"),
                ],
            );
            ui.end_row();
        });

    heading(ui, "Other");
    egui::Grid::new("other")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Connection status");
            combo(
                ui,
                "notify",
                &mut c.status_notify,
                &[
                    (StatusNotifyMode::Off, "Never show"),
                    (StatusNotifyMode::Notify, "Show briefly on change"),
                    (StatusNotifyMode::Always, "Always show"),
                    (StatusNotifyMode::WhenConnected, "Show while connected"),
                    (StatusNotifyMode::WhenDisconnected, "Show while disconnected"),
                ],
            );
            ui.end_row();

            ui.label("When ggoled quits");
            combo(
                ui,
                "quit",
                &mut c.quit_mode,
                &[
                    (ConfigQuitMode::Screensaver, "Leave the screen blank"),
                    (ConfigQuitMode::StockUi, "Return to the SteelSeries screen"),
                ],
            );
            ui.end_row();

            ui.label("Font");
            ui.vertical(|ui| {
                let mut custom = c.font.is_some();
                if ui
                    .checkbox(&mut custom, "Use a custom font for the clock and media")
                    .changed()
                {
                    c.font = custom.then(|| ConfigFont {
                        path: Default::default(),
                        size: 16.0,
                    });
                }
                if let Some(font) = &mut c.font {
                    ui.horizontal(|ui| {
                        let mut path = font.path.to_string_lossy().to_string();
                        ui.label("File");
                        if ui
                            .add(egui::TextEdit::singleline(&mut path).desired_width(260.0))
                            .changed()
                        {
                            font.path = path.into();
                        }
                        ui.label("Size");
                        ui.add(egui::DragValue::new(&mut font.size).range(6.0..=64.0));
                    });
                    hint(ui, "Path to a .ttf file. Takes effect when ggoled restarts.");
                }
            });
            ui.end_row();
        });
}

/// An editable list of symbols with display names and reordering.
fn symbol_list(
    ui: &mut Ui,
    id: &str,
    symbols: &mut Vec<String>,
    c_labels: &mut std::collections::BTreeMap<String, String>,
    defaults: &ggoled_ticker::TickerConfig,
    new: &mut String,
) {
    let mut action: Option<(usize, i32)> = None; // (index, -1 up / +1 down / 0 remove)
    egui::Grid::new(id)
        .num_columns(3)
        .spacing([10.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Symbol").weak());
            ui.label(RichText::new("Shown as").weak());
            ui.label("");
            ui.end_row();
            let n = symbols.len();
            for (i, sym) in symbols.iter().enumerate() {
                ui.label(RichText::new(sym).monospace());
                let mut name = c_labels.get(sym).cloned().unwrap_or_default();
                let placeholder = {
                    let mut d = defaults.clone();
                    d.labels.clear();
                    d.name_for(sym)
                };
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut name)
                            .hint_text(placeholder)
                            .desired_width(110.0),
                    )
                    .changed()
                {
                    if name.trim().is_empty() {
                        c_labels.remove(sym);
                    } else {
                        c_labels.insert(sym.clone(), name);
                    }
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(i > 0, egui::Button::new("Up").small())
                        .on_hover_text("Move up")
                        .clicked()
                    {
                        action = Some((i, -1));
                    }
                    if ui
                        .add_enabled(i + 1 < n, egui::Button::new("Down").small())
                        .on_hover_text("Move down")
                        .clicked()
                    {
                        action = Some((i, 1));
                    }
                    if ui.small_button("Remove").clicked() {
                        action = Some((i, 0));
                    }
                });
                ui.end_row();
            }
        });
    match action {
        Some((i, 0)) => {
            let sym = symbols.remove(i);
            c_labels.remove(&sym);
        }
        Some((i, -1)) => symbols.swap(i, i - 1),
        Some((i, _)) => symbols.swap(i, i + 1),
        None => {}
    }
    ui.horizontal(|ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(new)
                .hint_text("e.g. ETH-USD")
                .desired_width(140.0),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let sym = new.trim().to_ascii_uppercase();
        let ok = !sym.is_empty() && !symbols.contains(&sym);
        if (ui.add_enabled(ok, egui::Button::new("Add")).clicked() || (enter && ok)) && ok {
            symbols.push(sym);
            new.clear();
        }
    });
}

fn ticker_tab(ui: &mut Ui, c: &mut Config, new_symbol: &mut String, new_compact: &mut String) {
    let t = &mut c.ticker;
    heading(ui, "Layout");
    egui::Grid::new("tlayout")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Layout");
            combo(
                ui,
                "tmode",
                &mut t.mode,
                &[
                    (TickerMode::Cycle, "One at a time (large)"),
                    (TickerMode::Grid, "All at once (list)"),
                    (TickerMode::Scroll, "Scrolling"),
                ],
            );
            ui.end_row();

            ui.label("Switch every");
            ui.horizontal(|ui| {
                seconds(ui, &mut t.cycle_secs, 2..=300);
                ui.weak("(one-at-a-time: per asset; list: per page)");
            });
            ui.end_row();

            ui.label("Update prices every");
            seconds(ui, &mut t.refresh_secs, ggoled_ticker::MIN_REFRESH_SECS..=3600);
            ui.end_row();

            ui.label("List top row");
            combo(
                ui,
                "theader",
                &mut t.grid_header,
                &[
                    (GridHeader::Time, "Time"),
                    (GridHeader::Media, "What's playing (else time)"),
                    (GridHeader::None, "Nothing"),
                ],
            );
            ui.end_row();
        });
    hint(ui, "The list's top row is used only when there's a spare row.");

    let defaults = t.clone();
    heading(ui, "Assets");
    hint(
        ui,
        "Yahoo Finance symbols: stocks as-is (MSTR, SPY), crypto as pairs (BTC-USD), indexes with ^ (^GSPC).",
    );
    symbol_list(ui, "symbols", &mut t.symbols, &mut t.labels, &defaults, new_symbol);

    heading(ui, "Index row");
    hint(
        ui,
        "Shown together on one row with only their % change in the list layout (up to 4), and like the others elsewhere.",
    );
    symbol_list(
        ui,
        "compact",
        &mut t.compact_symbols,
        &mut t.labels,
        &defaults,
        new_compact,
    );
    if t.compact_symbols.len() > 4 {
        ui.colored_label(
            Color32::from_rgb(230, 170, 60),
            "Only the first 4 fit on the index row.",
        );
    }
}

/// How an entry's days are chosen in the UI.
#[derive(PartialEq, Clone, Copy)]
enum DaysChoice {
    Daily,
    Weekdays,
    Weekends,
    Custom,
}

fn days_choice(days: &[String]) -> DaysChoice {
    let norm: Vec<String> = days.iter().map(|d| d.trim().to_ascii_lowercase()).collect();
    match norm.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice() {
        [] | ["daily"] => DaysChoice::Daily,
        ["weekdays"] => DaysChoice::Weekdays,
        ["weekends"] => DaysChoice::Weekends,
        _ => DaysChoice::Custom,
    }
}

fn days_editor(ui: &mut Ui, id: usize, days: &mut Vec<String>, weekend: u8) {
    let current = days_choice(days);
    let mut choice = current;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("days", id))
            .selected_text(match choice {
                DaysChoice::Daily => "Every day",
                DaysChoice::Weekdays => "Weekdays",
                DaysChoice::Weekends => "Weekends",
                DaysChoice::Custom => "Specific days",
            })
            .width(130.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut choice, DaysChoice::Daily, "Every day");
                ui.selectable_value(&mut choice, DaysChoice::Weekdays, "Weekdays");
                ui.selectable_value(&mut choice, DaysChoice::Weekends, "Weekends");
                ui.selectable_value(&mut choice, DaysChoice::Custom, "Specific days");
            });
        let mask = schedule::days_mask(days, weekend).unwrap_or(0);
        if choice != current {
            *days = match choice {
                DaysChoice::Daily => vec!["daily".into()],
                DaysChoice::Weekdays => vec!["weekdays".into()],
                DaysChoice::Weekends => vec!["weekends".into()],
                // start from whatever days the previous choice covered
                DaysChoice::Custom => DAY_KEYS
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, d)| d.to_string())
                    .collect(),
            };
        }
        if choice == DaysChoice::Custom {
            let mask = schedule::days_mask(days, weekend).unwrap_or(0);
            let mut new_mask = mask;
            for (i, key) in DAY_KEYS.iter().enumerate() {
                let mut on = mask & (1 << i) != 0;
                let label = format!("{}{}", key[..1].to_ascii_uppercase(), &key[1..]);
                if ui.toggle_value(&mut on, label).changed() {
                    new_mask ^= 1 << i;
                }
            }
            if new_mask != mask {
                *days = DAY_KEYS
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| new_mask & (1 << i) != 0)
                    .map(|(_, d)| d.to_string())
                    .collect();
            }
        } else {
            ui.weak(schedule::days_label(mask));
        }
    });
}

/// "HH:MM" as two drag values. Leaves unparseable text alone until edited.
fn time_editor(ui: &mut Ui, start: &mut String) {
    let (mut h, mut m) = chrono::NaiveTime::parse_from_str(start.trim(), "%H:%M")
        .map(|t| (chrono::Timelike::hour(&t), chrono::Timelike::minute(&t)))
        .unwrap_or((0, 0));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let a = ui.add(
            egui::DragValue::new(&mut h)
                .range(0..=23)
                .custom_formatter(|v, _| format!("{v:02}")),
        );
        ui.label(":");
        let b = ui.add(
            egui::DragValue::new(&mut m)
                .range(0..=59)
                .speed(0.5)
                .custom_formatter(|v, _| format!("{v:02}")),
        );
        if a.changed() || b.changed() {
            *start = format!("{h:02}:{m:02}");
        }
    });
}

/// A per-entry setting that can be left to the normal settings.
fn optional<T: Clone>(ui: &mut Ui, label: &str, value: &mut Option<T>, default: T, edit: impl FnOnce(&mut Ui, &mut T)) {
    let mut set = value.is_some();
    if ui.checkbox(&mut set, label).changed() {
        *value = set.then(|| default.clone());
    }
    match value {
        Some(v) => edit(ui, v),
        None => {
            ui.weak("unchanged");
        }
    }
    ui.end_row();
}

fn schedule_tab(ui: &mut Ui, c: &mut Config) {
    heading(ui, "Schedule");
    ui.checkbox(&mut c.schedule_enabled, "Follow the schedule");
    ui.horizontal(|ui| {
        ui.label("Weekend days");
        let mut mask = schedule::parse_weekend(&c.weekend_days).unwrap_or(schedule::SAT_SUN);
        let before = mask;
        ui.selectable_value(&mut mask, schedule::SAT_SUN, "Sat–Sun");
        ui.selectable_value(&mut mask, schedule::FRI_SUN, "Fri–Sun");
        if mask != schedule::SAT_SUN && mask != schedule::FRI_SUN {
            ui.weak(format!("custom: {}", schedule::days_label(mask)));
        }
        if mask != before {
            c.weekend_days = if mask == schedule::FRI_SUN {
                "fri-sun"
            } else {
                "sat-sun"
            }
            .into();
        }
    });
    hint(
        ui,
        "Each entry starts at its time on its days and lasts until the next entry starts. Settings it doesn't change use the Display tab's.",
    );

    // What the schedule says right now, as a sanity check
    if let Ok(timings) = c.validate()
        && let Some(i) = schedule::active(&timings, chrono::Local::now())
    {
        let name = c.schedule.get(i).map(|s| s.name.clone()).unwrap_or_default();
        ui.label(format!(
            "Right now: {}",
            if name.is_empty() {
                format!("entry #{}", i + 1)
            } else {
                name
            }
        ));
    }

    heading(ui, "When this PC is locked");
    let mut names: Vec<String> = vec![];
    for seg in &c.schedule {
        if !seg.name.is_empty() && !names.contains(&seg.name) {
            names.push(seg.name.clone());
        }
    }
    ui.horizontal(|ui| {
        ui.label("Use");
        egui::ComboBox::from_id_salt("lock_entry")
            .selected_text(if c.lock_entry.is_empty() {
                "the schedule as usual".to_string()
            } else {
                c.lock_entry.clone()
            })
            .width(190.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut c.lock_entry, String::new(), "the schedule as usual");
                for name in &names {
                    ui.selectable_value(&mut c.lock_entry, name.clone(), name);
                }
            });
        ui.label("while locked");
    });
    ui.add_enabled_ui(!c.lock_entry.is_empty(), |ui| {
        ui.checkbox(&mut c.lock_scarlett, "Include its Scarlett LED timeout");
    });
    hint(
        ui,
        "Applies 5 seconds after locking, and everything returns to normal when you unlock (a hold stays). \
         With the Scarlett included, its audio drops out for a few seconds after you unlock while the timeout changes back.",
    );

    let weekend = schedule::parse_weekend(&c.weekend_days).unwrap_or(schedule::SAT_SUN);
    let lock_entry = &mut c.lock_entry;
    let mut action: Option<(usize, i32)> = None; // -1 up, +1 down, 0 delete, 2 duplicate
    let n = c.schedule.len();
    for (i, seg) in c.schedule.iter_mut().enumerate() {
        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let old_name = seg.name.clone();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut seg.name)
                            .hint_text("Name")
                            .desired_width(140.0),
                    )
                    .changed()
                    && !old_name.is_empty()
                    && *lock_entry == old_name
                {
                    *lock_entry = seg.name.clone(); // keep the lock setting pointing at it
                }
                ui.label("starts at");
                time_editor(ui, &mut seg.start);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Delete").clicked() {
                        action = Some((i, 0));
                    }
                    if ui.small_button("Duplicate").clicked() {
                        action = Some((i, 2));
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("Down").small()).clicked() {
                        action = Some((i, 1));
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("Up").small()).clicked() {
                        action = Some((i, -1));
                    }
                });
            });
            days_editor(ui, i, &mut seg.days, weekend);
            ui.add_space(4.0);
            egui::Grid::new(("seg", i))
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    optional(ui, "Show", &mut seg.display, DisplayMode::Normal, |ui, v| {
                        combo(ui, &format!("sd{i}"), v, &DISPLAY_OPTIONS)
                    });
                    optional(ui, "Clock", &mut seg.time_mode, ConfigTimeMode::H24, |ui, v| {
                        combo(ui, &format!("st{i}"), v, &TIME_OPTIONS)
                    });
                    optional(ui, "Now playing", &mut seg.show_media, true, |ui, v| {
                        ui.checkbox(v, "Show");
                    });
                    optional(ui, "Screensaver", &mut seg.screensaver, true, |ui, v| {
                        ui.checkbox(v, "On");
                    });
                    optional(ui, "Screensaver delay", &mut seg.screensaver_secs, 60, |ui, v| {
                        seconds(ui, v, 5..=86400)
                    });
                    optional(ui, "Brightness", &mut seg.brightness, 5, |ui, v| {
                        ui.add(egui::Slider::new(v, 1..=10));
                    });
                    optional(
                        ui,
                        "Scarlett LED timeout",
                        &mut seg.scarlett_led_timeout,
                        1800,
                        |ui, v| seconds(ui, v, 0..=86400),
                    );
                });
        });
    }
    match action {
        Some((i, 0)) => {
            let removed = c.schedule.remove(i);
            if removed.name == c.lock_entry && !c.schedule.iter().any(|s| s.name == removed.name) {
                c.lock_entry.clear();
            }
        }
        Some((i, 2)) => {
            let copy = c.schedule[i].clone();
            c.schedule.insert(i + 1, copy);
        }
        Some((i, -1)) => c.schedule.swap(i, i - 1),
        Some((i, 1)) => c.schedule.swap(i, i + 1),
        _ => {}
    }
    ui.add_space(8.0);
    if ui.button("Add entry").clicked() {
        c.schedule.push(Segment {
            name: format!("Entry {}", c.schedule.len() + 1),
            days: vec!["daily".into()],
            start: "08:00".into(),
            ..Default::default()
        });
    }
}

fn scarlett_tab(ui: &mut Ui, c: &mut Config, command: &mut String) {
    heading(ui, "Scarlett LED timeout");
    hint(
        ui,
        "Lets schedule entries set a Focusrite Scarlett 2i2 4th Gen's front-panel LED timeout. \
         Needs the one-time setup in scripts/scarlett (see its README).",
    );
    ui.add_space(4.0);
    ui.checkbox(&mut c.scarlett.enabled, "Apply schedule entries' LED timeout");
    ui.add_space(4.0);
    ui.label("Command to run");
    if ui
        .add(
            egui::TextEdit::singleline(command)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        )
        .changed()
    {
        c.scarlett.command = split_command(command);
    }
    hint(
        ui,
        "{seconds} is replaced with the timeout, which is also written to ggoled_scarlett_led.txt next to the config file. \
         The default starts the scheduled task the setup script creates.",
    );
    if ui.small_button("Use default").clicked() {
        c.scarlett.command = ScarlettConfig::default().command;
        *command = join_command(&c.scarlett.command);
    }
}

/// Split a command line on spaces, keeping "quoted parts" together.
fn split_command(text: &str) -> Vec<String> {
    let mut args = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for ch in text.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        args.push(cur);
    }
    args
}

fn join_command(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.contains(char::is_whitespace) || a.is_empty() {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trip() {
        let cmd = ScarlettConfig::default().command;
        assert_eq!(split_command(&join_command(&cmd)), cmd);
        assert_eq!(split_command(r#"a "b c"  d"#), vec!["a", "b c", "d"]);
    }

    #[test]
    fn secs() {
        assert_eq!(format_secs(1800), "30 min");
        assert_eq!(format_secs(90), "1 min 30 s");
        assert_eq!(format_secs(3600), "1 h");
        assert_eq!(parse_secs("5 min"), Some(300.0));
        assert_eq!(parse_secs("1h"), Some(3600.0));
        assert_eq!(parse_secs("1 min 30 s"), Some(90.0));
        assert_eq!(parse_secs("45"), Some(45.0));
        assert_eq!(parse_secs("abc"), None);
    }
}
