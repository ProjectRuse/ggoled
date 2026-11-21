//! Settings, stored as TOML in the user's config directory. Shared by the tray
//! app and the settings window.

use crate::schedule::{self, DisplayMode, Segment, Timing};
use ggoled_draw::ShiftMode;
use ggoled_sysinfo::SysInfoConfig;
use ggoled_ticker::TickerConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Debug)]
pub enum ConfigTimeMode {
    Off,
    #[default]
    H24,
    H12,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Debug)]
pub enum ConfigShiftMode {
    Off,
    Low,
    #[default]
    High,
}
impl ConfigShiftMode {
    pub fn to_api(self) -> ShiftMode {
        match self {
            ConfigShiftMode::Off => ShiftMode::Off,
            ConfigShiftMode::Low => ShiftMode::Low,
            ConfigShiftMode::High => ShiftMode::High,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Debug)]
pub enum ConfigQuitMode {
    #[default]
    Screensaver,
    StockUi,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusNotifyMode {
    Off,
    #[default]
    Notify,
    Always,
    WhenConnected,
    WhenDisconnected,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct ConfigFont {
    pub path: PathBuf,
    pub size: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    pub font: Option<ConfigFont>,
    pub time_mode: ConfigTimeMode,
    pub show_media: bool,
    pub idle_timeout: bool,
    /// Seconds without keyboard/mouse input before the screensaver blanks the OLED.
    pub idle_timeout_secs: u32,
    /// Seconds to leave the SteelSeries volume screen up after a volume change (0 = off).
    pub volume_overlay_secs: u32,
    pub oled_shift: ConfigShiftMode,
    pub status_notify: StatusNotifyMode,
    pub quit_mode: ConfigQuitMode,
    /// What the screen shows (schedule entries can override this).
    pub display: DisplayMode,
    pub ticker: TickerConfig,
    pub sysinfo: SysInfoConfig,
    /// Follow the [[schedule]] entries below.
    pub schedule_enabled: bool,
    /// Days that "weekends" means in the schedule (the rest are "weekdays"),
    /// e.g. "sat-sun", "fri-sun", or "fri,sat,sun".
    pub weekend_days: String,
    pub scarlett: ScarlettConfig,
    /// Name of a schedule entry to use while the PC is locked (empty = none).
    pub lock_entry: String,
    /// Whether that entry's Scarlett LED timeout applies too (changing it briefly
    /// interrupts the Scarlett's audio, e.g. right after unlocking).
    pub lock_scarlett: bool,
    // Skipped when empty: "schedule = []" would clash with [[schedule]] entries
    // added to the file later.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schedule: Vec<Segment>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct ScarlettConfig {
    /// Apply each schedule entry's scarlett_led_timeout by running `command`.
    pub enabled: bool,
    /// Program and arguments to run. "{seconds}" is replaced with the timeout.
    /// The value is also written to ggoled_scarlett_led.txt next to the config file.
    pub command: Vec<String>,
}
impl Default for ScarlettConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: vec![
                "schtasks".into(),
                "/run".into(),
                "/tn".into(),
                "ggoled Scarlett LEDs".into(),
            ],
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: None,
            time_mode: ConfigTimeMode::default(),
            show_media: true,
            idle_timeout: true,
            idle_timeout_secs: 60,
            volume_overlay_secs: 3,
            oled_shift: ConfigShiftMode::default(),
            status_notify: StatusNotifyMode::default(),
            quit_mode: ConfigQuitMode::default(),
            display: DisplayMode::Normal,
            ticker: TickerConfig::default(),
            sysinfo: SysInfoConfig::default(),
            schedule_enabled: false,
            weekend_days: "sat-sun".into(),
            scarlett: ScarlettConfig::default(),
            lock_entry: String::new(),
            lock_scarlett: true,
            schedule: vec![],
        }
    }
}
impl Config {
    pub fn path() -> PathBuf {
        directories::BaseDirs::new()
            .unwrap()
            .config_dir()
            .join("ggoled_app.toml")
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let text = toml::to_string(self)?;
        std::fs::write(Self::path(), text)?;
        Ok(())
    }
    /// Load the config. A missing file gives the defaults; a broken file is an
    /// error (rather than silently resetting, which would later overwrite it).
    pub fn load() -> Result<(Config, Vec<Timing>), String> {
        let text = match std::fs::read_to_string(Self::path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Config::default(), vec![])),
            Err(e) => return Err(format!("Couldn't read {}: {e}", Self::path().display())),
        };
        let problem = |e: String| format!("Problem in {}:\n\n{e}", Self::path().display());
        let conf: Config = toml::from_str(&text).map_err(|e| problem(e.to_string()))?;
        let timings = conf.validate().map_err(problem)?;
        Ok((conf, timings))
    }
    /// Check the parts serde can't (schedule times, days, weekend setting).
    pub fn validate(&self) -> Result<Vec<Timing>, String> {
        let weekend = schedule::parse_weekend(&self.weekend_days)?;
        let timings = schedule::parse(&self.schedule, weekend)?;
        if !self.lock_entry.is_empty() && self.lock_entry_index().is_none() {
            return Err(format!(
                "lock_entry: there's no schedule entry named \"{}\"",
                self.lock_entry
            ));
        }
        Ok(timings)
    }
    /// The schedule entry to use while locked, if set (first entry with that name).
    pub fn lock_entry_index(&self) -> Option<usize> {
        if self.lock_entry.is_empty() {
            return None;
        }
        self.schedule.iter().position(|s| s.name == self.lock_entry)
    }
}
