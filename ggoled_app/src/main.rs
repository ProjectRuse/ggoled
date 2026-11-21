#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod os;
mod schedule;
mod settings;

use chrono::{DateTime, Local, TimeDelta, Timelike};
use config::*;
use ggoled_draw::{DrawDevice, DrawEvent, DrawLayer, LayerId, TextRenderer, bitmap_from_memory};
use ggoled_lib::{Bitmap, Device};
use ggoled_ticker::{GridHeader, Ticker, TickerMode};
use os::{Media, OSFeatures, OSImpl};
use rfd::{MessageDialog, MessageLevel};
use schedule::{DisplayMode, Segment, Timing};
use sdl3_sys::everything as sdl;
use std::hash::{Hash, Hasher};
use std::{
    ffi::{CStr, CString},
    fmt::Debug,
    os::raw::c_void,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread::sleep,
    time::{Duration, Instant},
};

const NOTIF_DUR: Duration = Duration::from_secs(5);

fn show_warning(text: &str) {
    MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("ggoled")
        .set_description(text)
        .show();
}

/// Open a file with the system's default editor.
fn open_in_editor(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let res = std::process::Command::new("notepad").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open").arg("-t").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let res = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = res {
        show_warning(&format!("Couldn't open {}: {e}", path.display()));
    }
}

/// Settings in effect right now: the tray settings, with the active schedule
/// entry's values layered on top.
struct Effective {
    time_mode: ConfigTimeMode,
    show_media: bool,
    screensaver: bool,
    screensaver_secs: u32,
    display: DisplayMode,
    brightness: Option<u8>,
    scarlett_led_timeout: Option<u32>,
}
impl Effective {
    fn new(config: &Config, seg: Option<&Segment>) -> Self {
        Self {
            time_mode: seg.and_then(|s| s.time_mode).unwrap_or(config.time_mode),
            show_media: seg.and_then(|s| s.show_media).unwrap_or(config.show_media),
            screensaver: seg.and_then(|s| s.screensaver).unwrap_or(config.idle_timeout),
            screensaver_secs: seg.and_then(|s| s.screensaver_secs).unwrap_or(config.idle_timeout_secs),
            display: seg.and_then(|s| s.display).unwrap_or(config.display),
            brightness: seg.and_then(|s| s.brightness),
            scarlett_led_timeout: seg.and_then(|s| s.scarlett_led_timeout),
        }
    }
}

/// Keeps the Scarlett's LED timeout in line with the schedule by running an
/// external command (see ScarlettConfig). Remembers the last value it applied,
/// across restarts, so it only runs when something actually changes.
struct ScarlettSync {
    applied: Option<u32>,
    last_attempt: Option<Instant>,
}
impl ScarlettSync {
    fn value_path() -> PathBuf {
        Config::path().with_file_name("ggoled_scarlett_led.txt")
    }
    fn applied_path() -> PathBuf {
        Config::path().with_file_name("ggoled_scarlett_applied.txt")
    }
    fn new() -> Self {
        let applied = std::fs::read_to_string(Self::applied_path())
            .ok()
            .and_then(|t| t.trim().parse().ok());
        Self {
            applied,
            last_attempt: None,
        }
    }
    fn forget(&mut self) {
        self.applied = None;
        self.last_attempt = None;
        _ = std::fs::remove_file(Self::applied_path());
    }
    /// Call about once a second. `busy` (media playing, a game running) defers the
    /// change, since applying it briefly interrupts the Scarlett's audio.
    fn update(&mut self, config: &ScarlettConfig, want: Option<u32>, busy: bool) {
        let Some(want) = want else { return };
        if !config.enabled || config.command.is_empty() || self.applied == Some(want) || busy {
            return;
        }
        // The scheduled task ignores a second start while one is running; the script
        // re-reads the value when it finishes, so a short gap is enough.
        if self.last_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
            return;
        }
        self.last_attempt = Some(Instant::now());
        if let Err(e) = std::fs::write(Self::value_path(), want.to_string()) {
            eprintln!("scarlett: couldn't write value file: {e}");
            return;
        }
        let args: Vec<String> = config
            .command
            .iter()
            .map(|a| a.replace("{seconds}", &want.to_string()))
            .collect();
        let mut cmd = std::process::Command::new(&args[0]);
        cmd.args(&args[1..]);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.spawn() {
            Ok(_) => {
                self.applied = Some(want);
                _ = std::fs::write(Self::applied_path(), want.to_string());
            }
            Err(e) => eprintln!("scarlett: couldn't run {:?}: {e}", args[0]),
        }
    }
}

// unwrap an error and show a MessageDialog if it fails
pub fn dialog_unwrap<T, E: Debug>(res: Result<T, E>) -> T {
    match res {
        Ok(v) => v,
        Err(e) => {
            let str = format!("Error: {:?}", e);
            MessageDialog::new()
                .set_level(MessageLevel::Error)
                .set_title("ggoled")
                .set_description(&str)
                .show();
            panic!("dialog_unwrap: {}", str);
        }
    }
}

struct Icon {
    _pixels: Vec<u8>,
    surf: *mut sdl::SDL_Surface,
}
impl Icon {
    fn load(buf: &[u8]) -> Self {
        let pixels = image::load_from_memory(buf)
            .unwrap()
            .resize(32, 32, image::imageops::FilterType::Lanczos3)
            .to_rgba8()
            .into_vec();
        let surf = unsafe {
            sdl::SDL_CreateSurfaceFrom(
                32,
                32,
                sdl::SDL_PixelFormat::RGBA32,
                pixels.as_ptr() as *mut c_void,
                32 * 4,
            )
        };
        assert!(!surf.is_null());
        Self { _pixels: pixels, surf }
    }
}
impl Drop for Icon {
    fn drop(&mut self) {
        unsafe { sdl::SDL_DestroySurface(self.surf) };
    }
}

fn menu_check(menu: *mut sdl::SDL_TrayMenu, title: &'static CStr, checked: bool) -> *mut sdl::SDL_TrayEntry {
    let checked = if checked {
        sdl::SDL_TRAYENTRY_CHECKED
    } else {
        sdl::SDL_TrayEntryFlags(0)
    };
    unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, title.as_ptr(), sdl::SDL_TRAYENTRY_CHECKBOX | checked) }
}

extern "C" fn c_menu_callback(userdata: *mut c_void, _entry: *mut sdl::SDL_TrayEntry) {
    #[allow(clippy::borrowed_box)] // needs to be boxed
    let f: &Box<dyn Fn()> = unsafe { &*(userdata as *mut Box<dyn Fn()>) };
    f();
}

fn menu_callback(entry: *mut sdl::SDL_TrayEntry, f: impl Fn()) {
    let f: Box<Box<dyn Fn()>> = Box::new(Box::new(f));
    let f = Box::leak(f) as *mut Box<dyn Fn()> as *mut c_void;
    unsafe { sdl::SDL_SetTrayEntryCallback(entry, Some(c_menu_callback), f) };
}

#[derive(Clone, Copy)]
enum MenuEvent {
    ToggleCheck,
    SetTimeMode(ConfigTimeMode),
    SetDisplay(DisplayMode),
    SetTickerMode(TickerMode),
    SetShiftMode(ConfigShiftMode),
    SetStatusNotifyMode(StatusNotifyMode),
    SetQuitMode(ConfigQuitMode),
    SetIdleDelay(u32),
    SetVolumeOverlay(u32),
    SetWeekend(u8),
    ToggleSchedule,
    /// Some(i): hold schedule entry i until the next entry starts. None: end the hold.
    Hold(Option<usize>),
    EditConfig,
    ReloadConfig,
    ReapplyScarlett,
    OpenSettings,
    Quit,
}

fn bind_menu_event(entry: *mut sdl::SDL_TrayEntry, tx: &mpsc::Sender<MenuEvent>, event: MenuEvent) {
    let tx = tx.clone();
    menu_callback(entry, move || {
        let _ = tx.send(event);
    });
}

struct RadioMenu<T> {
    entries: Vec<(*mut sdl::SDL_TrayEntry, T)>,
}
impl<T: Copy + PartialEq> RadioMenu<T> {
    fn new(
        menu: *mut sdl::SDL_TrayMenu,
        title: &'static CStr,
        options: &[(&'static CStr, T)],
        initial: T,
        tx: &mpsc::Sender<MenuEvent>,
        into_event: fn(T) -> MenuEvent,
    ) -> Self {
        let submenu_entry = unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, title.as_ptr(), sdl::SDL_TRAYENTRY_SUBMENU) };
        let submenu = unsafe { sdl::SDL_CreateTraySubmenu(submenu_entry) };
        let entries: Vec<_> = options
            .iter()
            .map(|(label, value)| {
                let entry = menu_check(submenu, label, *value == initial);
                bind_menu_event(entry, tx, into_event(*value));
                (entry, *value)
            })
            .collect();
        Self { entries }
    }
    fn update_checked(&self, value: T) {
        for (entry, v) in &self.entries {
            unsafe { sdl::SDL_SetTrayEntryChecked(*entry, *v == value) };
        }
    }
}

fn menu_check_dyn(menu: *mut sdl::SDL_TrayMenu, pos: i32, label: &CStr, checked: bool) -> *mut sdl::SDL_TrayEntry {
    let checked = if checked {
        sdl::SDL_TRAYENTRY_CHECKED
    } else {
        sdl::SDL_TrayEntryFlags(0)
    };
    unsafe { sdl::SDL_InsertTrayEntryAt(menu, pos, label.as_ptr(), sdl::SDL_TRAYENTRY_CHECKBOX | checked) }
}

/// Name for an entry, with its days added when several entries share a name
/// (e.g. "Day (Mon–Thu)" and "Day (Fri–Sun)").
fn segment_label(schedule: &[Segment], timings: &[Timing], i: usize) -> String {
    let name = match schedule.get(i) {
        Some(seg) if !seg.name.is_empty() => seg.name.clone(),
        _ => return format!("entry #{}", i + 1),
    };
    let shared = schedule.iter().filter(|s| s.name == name).count() > 1;
    match timings.get(i) {
        Some(t) if shared => format!("{name} ({})", schedule::days_label(t.days)),
        _ => name,
    }
}

/// Position of the first "Hold" entry inside the Schedule submenu.
const HOLD_MENU_POS: i32 = 2;

/// (Re)create the "Hold: <name>" entries in the Schedule submenu.
fn rebuild_hold_entries(
    menu: *mut sdl::SDL_TrayMenu,
    entries: &mut Vec<*mut sdl::SDL_TrayEntry>,
    schedule: &[Segment],
    timings: &[Timing],
    tx: &mpsc::Sender<MenuEvent>,
) {
    for entry in entries.drain(..) {
        unsafe { sdl::SDL_RemoveTrayEntry(entry) };
    }
    for i in 0..schedule.len() {
        let label = CString::new(format!("Hold: {}", segment_label(schedule, timings, i)).replace('\0', "")).unwrap();
        let entry = menu_check_dyn(menu, HOLD_MENU_POS + i as i32, &label, false);
        bind_menu_event(entry, tx, MenuEvent::Hold(Some(i)));
        entries.push(entry);
    }
}

const SCHEDULE_EXAMPLE: &str = r##"
# ---------------------------------------------------------------------------
# Schedule example (remove the leading "# " to use it, then use
# Schedule > Reload config in the tray menu, and turn on "Follow schedule").
#
# Each entry starts at `start` on its `days` and lasts until the next entry
# starts. Anything an entry leaves out uses the normal tray settings.
#   days:     "daily", "weekdays", "weekends", "mon", ..., or ranges like "fri-sun" (list)
#             "weekends" follows Schedule > Weekend days in the tray (Sat-Sun or Fri-Sun)
#   display:  "Normal" (time/media), "Ticker" (prices), "Blank" (dark), "Stock" (SteelSeries UI)
#   time_mode: "Off", "H24", "H12"      show_media: true/false
#   screensaver: true/false             screensaver_secs: seconds
#   brightness: 1-10                    scarlett_led_timeout: seconds
#
# [[schedule]]
# name = "Night"
# days = ["daily"]
# start = "00:00"
# display = "Blank"
# brightness = 1
# scarlett_led_timeout = 5
#
# [[schedule]]
# name = "Day"
# days = ["weekdays"]
# start = "08:00"
# display = "Normal"
# brightness = 5
# scarlett_led_timeout = 1800
#
# [[schedule]]
# name = "Day"
# days = ["weekends"]
# start = "10:00"
# display = "Normal"
# brightness = 5
# scarlett_led_timeout = 1800
"##;

fn config_file_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(Config::path()).and_then(|m| m.modified()).ok()
}

fn main() {
    // `ggoled_app --settings` opens the settings window instead (the tray starts it this way)
    if std::env::args().any(|a| a == "--settings") {
        settings::run();
        return;
    }

    // Initial loading
    let (mut config, mut timings, mut config_save_blocked) = match Config::load() {
        Ok((config, timings)) => (config, timings, false),
        Err(e) => {
            show_warning(&format!(
                "{e}\n\nUsing default settings until this is fixed. Your file won't be overwritten; \
                 fix it and use Schedule > Reload config."
            ));
            (Config::default(), vec![], true)
        }
    };
    let mut os = OSImpl::new();

    // Create tray icon with menu
    unsafe { sdl::SDL_SetHint(sdl::SDL_HINT_VIDEO_ALLOW_SCREENSAVER, c"1".as_ptr()) };
    unsafe { sdl::SDL_SetHint(sdl::SDL_HINT_MAC_BACKGROUND_APP, c"1".as_ptr()) }; // no dock icon on macOS
    assert!(unsafe { sdl::SDL_Init(sdl::SDL_INIT_VIDEO) });
    let icon = Icon::load(include_bytes!("../assets/ggoled.png"));
    let icon_error = Icon::load(include_bytes!("../assets/ggoled_error.png"));
    let tray = unsafe { sdl::SDL_CreateTray(icon_error.surf, c"ggoled".as_ptr()) };
    assert!(!tray.is_null());
    let menu = unsafe { sdl::SDL_CreateTrayMenu(tray) };
    assert!(!menu.is_null());
    let (menu_tx, menu_rx) = mpsc::channel::<MenuEvent>();

    // CI builds include the commit they were built from, to tell test builds apart
    let build = option_env!("GITHUB_SHA").map(|sha| format!(" ({})", &sha[..sha.len().min(7)]));
    let version_label = format!(
        "ggoled_app - v{}{}\0",
        env!("CARGO_PKG_VERSION"),
        build.unwrap_or_default()
    );
    unsafe {
        let tm_version = sdl::SDL_InsertTrayEntryAt(menu, -1, version_label.as_ptr().cast(), sdl::SDL_TRAYENTRY_BUTTON);
        sdl::SDL_SetTrayEntryEnabled(tm_version, false);
    }
    let tm_status =
        unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, c"Schedule: off".as_ptr(), sdl::SDL_TRAYENTRY_BUTTON) };
    unsafe { sdl::SDL_SetTrayEntryEnabled(tm_status, false) };
    let tm_settings =
        unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, c"Settings...".as_ptr(), sdl::SDL_TRAYENTRY_BUTTON) };
    bind_menu_event(tm_settings, &menu_tx, MenuEvent::OpenSettings);

    // Schedule submenu
    let sched_entry = unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, c"Schedule".as_ptr(), sdl::SDL_TRAYENTRY_SUBMENU) };
    let sched_menu = unsafe { sdl::SDL_CreateTraySubmenu(sched_entry) };
    let tm_sched_enabled = menu_check(sched_menu, c"Follow schedule", config.schedule_enabled);
    bind_menu_event(tm_sched_enabled, &menu_tx, MenuEvent::ToggleSchedule);
    unsafe { sdl::SDL_InsertTrayEntryAt(sched_menu, -1, std::ptr::null(), sdl::SDL_TRAYENTRY_BUTTON) }; // separator
    // "Hold: <name>" entries go here (HOLD_MENU_POS)
    let tm_resume =
        unsafe { sdl::SDL_InsertTrayEntryAt(sched_menu, -1, c"End hold".as_ptr(), sdl::SDL_TRAYENTRY_BUTTON) };
    bind_menu_event(tm_resume, &menu_tx, MenuEvent::Hold(None));
    unsafe { sdl::SDL_InsertTrayEntryAt(sched_menu, -1, std::ptr::null(), sdl::SDL_TRAYENTRY_BUTTON) }; // separator
    let tm_edit =
        unsafe { sdl::SDL_InsertTrayEntryAt(sched_menu, -1, c"Edit config file".as_ptr(), sdl::SDL_TRAYENTRY_BUTTON) };
    bind_menu_event(tm_edit, &menu_tx, MenuEvent::EditConfig);
    let tm_reload =
        unsafe { sdl::SDL_InsertTrayEntryAt(sched_menu, -1, c"Reload config".as_ptr(), sdl::SDL_TRAYENTRY_BUTTON) };
    bind_menu_event(tm_reload, &menu_tx, MenuEvent::ReloadConfig);
    let tm_scarlett = unsafe {
        sdl::SDL_InsertTrayEntryAt(
            sched_menu,
            -1,
            c"Re-apply Scarlett LEDs".as_ptr(),
            sdl::SDL_TRAYENTRY_BUTTON,
        )
    };
    bind_menu_event(tm_scarlett, &menu_tx, MenuEvent::ReapplyScarlett);
    let weekend_mask = |config: &Config| schedule::parse_weekend(&config.weekend_days).unwrap_or(schedule::SAT_SUN);
    let tm_weekend_radio = RadioMenu::new(
        sched_menu,
        c"Weekend days",
        &[(c"Sat–Sun", schedule::SAT_SUN), (c"Fri–Sun", schedule::FRI_SUN)],
        weekend_mask(&config),
        &menu_tx,
        MenuEvent::SetWeekend,
    );
    let mut hold_entries: Vec<*mut sdl::SDL_TrayEntry> = vec![];
    rebuild_hold_entries(sched_menu, &mut hold_entries, &config.schedule, &timings, &menu_tx);
    unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, std::ptr::null(), sdl::SDL_TRAYENTRY_BUTTON) }; // separator

    let tm_display_radio = RadioMenu::new(
        menu,
        c"Display",
        &[
            (c"Clock && media", DisplayMode::Normal),
            (c"Price ticker", DisplayMode::Ticker),
            (c"System info", DisplayMode::SystemInfo),
            (c"Blank", DisplayMode::Blank),
            (c"SteelSeries screen", DisplayMode::Stock),
        ],
        config.display,
        &menu_tx,
        MenuEvent::SetDisplay,
    );
    let tm_ticker_radio = RadioMenu::new(
        menu,
        c"Ticker layout",
        &[
            (c"One at a time (large)", TickerMode::Cycle),
            (c"All at once (list)", TickerMode::Grid),
            (c"Scrolling", TickerMode::Scroll),
        ],
        config.ticker.mode,
        &menu_tx,
        MenuEvent::SetTickerMode,
    );

    let tm_time_radio = RadioMenu::new(
        menu,
        c"Time",
        &[
            (c"Off", ConfigTimeMode::Off),
            (c"24-hour", ConfigTimeMode::H24),
            (c"12-hour (AM/PM)", ConfigTimeMode::H12),
        ],
        config.time_mode,
        &menu_tx,
        MenuEvent::SetTimeMode,
    );

    let tm_media_check = menu_check(menu, c"Show playing media", config.show_media);
    if os.supports_media() {
        bind_menu_event(tm_media_check, &menu_tx, MenuEvent::ToggleCheck);
    } else {
        unsafe {
            config.show_media = false;
            sdl::SDL_SetTrayEntryChecked(tm_media_check, false);
            sdl::SDL_SetTrayEntryEnabled(tm_media_check, false);
        }
    }

    let tm_status_notify = RadioMenu::new(
        menu,
        c"Connection status icon",
        &[
            (c"Off", StatusNotifyMode::Off),
            (c"Notify", StatusNotifyMode::Notify),
            (c"Always (burn-in risk)", StatusNotifyMode::Always),
            (c"When connected (burn-in risk)", StatusNotifyMode::WhenConnected),
            (c"When disconnected (burn-in risk)", StatusNotifyMode::WhenDisconnected),
        ],
        config.status_notify,
        &menu_tx,
        MenuEvent::SetStatusNotifyMode,
    );

    let tm_idle_check = menu_check(menu, c"Screensaver when idle", config.idle_timeout);
    if os.supports_idle() {
        bind_menu_event(tm_idle_check, &menu_tx, MenuEvent::ToggleCheck);
    } else {
        unsafe {
            config.idle_timeout = false;
            sdl::SDL_SetTrayEntryChecked(tm_idle_check, false);
            sdl::SDL_SetTrayEntryEnabled(tm_idle_check, false);
        }
    }

    let tm_idle_delay_radio = RadioMenu::new(
        menu,
        c"Screensaver delay",
        &[
            (c"30 seconds", 30),
            (c"1 minute", 60),
            (c"2 minutes", 120),
            (c"5 minutes", 300),
            (c"10 minutes", 600),
            (c"15 minutes", 900),
            (c"30 minutes", 1800),
            (c"1 hour", 3600),
        ],
        config.idle_timeout_secs,
        &menu_tx,
        MenuEvent::SetIdleDelay,
    );
    os.set_idle_timeout(config.idle_timeout_secs.saturating_mul(1000));

    let tm_volume_radio = RadioMenu::new(
        menu,
        c"Show volume screen for",
        &[
            (c"Off", 0),
            (c"2 seconds", 2),
            (c"3 seconds", 3),
            (c"5 seconds", 5),
            (c"10 seconds", 10),
        ],
        config.volume_overlay_secs,
        &menu_tx,
        MenuEvent::SetVolumeOverlay,
    );

    let tm_shift_radio = RadioMenu::new(
        menu,
        c"OLED screen shift",
        &[
            (c"Off", ConfigShiftMode::Off),
            (c"Low", ConfigShiftMode::Low),
            (c"High", ConfigShiftMode::High),
        ],
        config.oled_shift,
        &menu_tx,
        MenuEvent::SetShiftMode,
    );

    let tm_quit_mode_radio = RadioMenu::new(
        menu,
        c"On app quit",
        &[
            (c"Screensaver", ConfigQuitMode::Screensaver),
            (c"Stock UI", ConfigQuitMode::StockUi),
        ],
        config.quit_mode,
        &menu_tx,
        MenuEvent::SetQuitMode,
    );

    unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, std::ptr::null(), sdl::SDL_TRAYENTRY_BUTTON) }; // separator
    let tm_quit = unsafe { sdl::SDL_InsertTrayEntryAt(menu, -1, c"Quit".as_ptr(), sdl::SDL_TRAYENTRY_BUTTON) };
    bind_menu_event(tm_quit, &menu_tx, MenuEvent::Quit);

    // Load icons
    let icon_hs_connect =
        Arc::new(bitmap_from_memory(include_bytes!("../assets/headset_connected.png"), 0x80).unwrap());
    let icon_hs_disconnect =
        Arc::new(bitmap_from_memory(include_bytes!("../assets/headset_disconnected.png"), 0x80).unwrap());

    let notif_update = |dev: &mut DrawDevice,
                        layer: &mut Option<LayerId>,
                        mode: StatusNotifyMode,
                        connected: bool,
                        expiry: DateTime<Local>| {
        if let Some(id) = layer.take() {
            dev.remove_layer(id);
        }
        let show = match mode {
            StatusNotifyMode::Off => false,
            StatusNotifyMode::Notify => Local::now() < expiry,
            StatusNotifyMode::Always => true,
            StatusNotifyMode::WhenConnected => connected,
            StatusNotifyMode::WhenDisconnected => !connected,
        };
        if show {
            *layer = Some(
                dev.add_layer(ggoled_draw::DrawLayer::Image {
                    bitmap: (if connected {
                        &icon_hs_connect
                    } else {
                        &icon_hs_disconnect
                    })
                    .clone(),
                    x: 4,
                    y: 4,
                }),
            );
        }
    };

    // State
    let mut last_time = Local::now() - TimeDelta::seconds(1);
    let mut last_media: Option<Media> = None;
    let mut time_layers: Vec<LayerId> = vec![];
    let mut media_layers: Vec<LayerId> = vec![];
    let mut notif_layer: Option<LayerId> = None;
    let mut notif_expiry = Local::now();
    let mut is_connected = false;
    let mut last_volume: Option<u8> = None;
    let mut volume_hold_until = Instant::now();
    let mut screen_blank = false;
    let mut stock_ui = false;
    let mut hold: Option<(usize, Option<DateTime<Local>>)> = None;
    let mut display_override: Option<(DisplayMode, Option<DateTime<Local>>)> = None;
    let mut shown_display: Option<DisplayMode> = None;
    let mut settings_child: Option<std::process::Child> = None;
    let mut locked_since: Option<Instant> = None;
    let sysinfo = ggoled_sysinfo::SysInfo::new(config.sysinfo.clone());
    let sysinfo_fonts = ggoled_sysinfo::Fonts::default();
    let mut sysinfo_layer: Option<LayerId> = None;
    let mut config_mtime = config_file_mtime();
    let mut last_config_check = Instant::now();
    let mut eff = Effective::new(&config, None);
    let mut last_active: Option<Option<usize>> = None;
    let mut applied_brightness: Option<u8> = None;
    let mut last_status = String::new();
    let mut scarlett = ScarlettSync::new();
    let ticker = Ticker::new(config.ticker.clone());
    let ticker_fonts = ggoled_ticker::Fonts::default();
    let mut ticker_layer: Option<LayerId> = None;
    let mut ticker_key: Option<(u64, TickerMode, usize)> = None;
    let mut ticker_prefetched: Option<i64> = None;
    let mut ticker_header_layer: Option<LayerId> = None;
    let mut ticker_header_text: Option<String> = None;

    // Wait for connect
    let mut last_connect = Instant::now() - Duration::from_secs(1);
    let dev = loop {
        let mut event = sdl::SDL_Event::default();
        while unsafe { sdl::SDL_PollEvent(&mut event) } {
            if sdl::SDL_EventType(unsafe { event.r#type }) == sdl::SDL_EVENT_QUIT {
                return;
            }
        }
        while let Ok(event) = menu_rx.try_recv() {
            if matches!(event, MenuEvent::Quit) {
                return;
            }
        }
        if last_connect.elapsed() >= Duration::from_secs(1) {
            last_connect = Instant::now();
            if let Ok(d) = Device::connect() {
                break d;
            }
        }
        sleep(Duration::from_millis(10));
    };
    unsafe { sdl::SDL_SetTrayIcon(tray, icon.surf) };
    let mut dev = DrawDevice::new(dev, 30);
    if let Some(font) = &config.font {
        dev.texter = dialog_unwrap(TextRenderer::load_from_file(&font.path, font.size));
    }
    dev.probe();

    // Go!
    dev.set_shift_mode(config.oled_shift.to_api());
    dev.play();
    'main: loop {
        // Window event loop
        // TODO: handle system going to sleep
        let mut event = sdl::SDL_Event::default();
        while unsafe { sdl::SDL_PollEvent(&mut event) } {
            let event_type = sdl::SDL_EventType(unsafe { event.r#type });
            if event_type == sdl::SDL_EVENT_QUIT {
                break 'main;
            }
        }

        // Handle tray menu events
        // Apply changes to the config file made elsewhere (settings window, hand edits)
        if last_config_check.elapsed() >= Duration::from_secs(1) {
            last_config_check = Instant::now();
            let mtime = config_file_mtime();
            if mtime != config_mtime {
                config_mtime = mtime;
                _ = menu_tx.send(MenuEvent::ReloadConfig);
            }
        }

        let mut config_updated = false;
        let mut refresh = false; // re-evaluate now, but nothing to save
        while let Ok(event) = menu_rx.try_recv() {
            match event {
                MenuEvent::ToggleCheck => {
                    config_updated = true;
                    config.show_media = unsafe { sdl::SDL_GetTrayEntryChecked(tm_media_check) };
                    config.idle_timeout = unsafe { sdl::SDL_GetTrayEntryChecked(tm_idle_check) };
                }
                MenuEvent::SetTimeMode(mode) => {
                    config_updated = true;
                    config.time_mode = mode;
                    tm_time_radio.update_checked(mode);
                }
                MenuEvent::SetStatusNotifyMode(mode) => {
                    config_updated = true;
                    config.status_notify = mode;
                    tm_status_notify.update_checked(mode);
                    notif_update(&mut dev, &mut notif_layer, mode, is_connected, notif_expiry);
                }
                MenuEvent::SetShiftMode(mode) => {
                    config_updated = true;
                    config.oled_shift = mode;
                    tm_shift_radio.update_checked(mode);
                    dev.set_shift_mode(config.oled_shift.to_api());
                }
                MenuEvent::SetQuitMode(mode) => {
                    config_updated = true;
                    config.quit_mode = mode;
                    tm_quit_mode_radio.update_checked(mode);
                }
                MenuEvent::SetIdleDelay(secs) => {
                    config_updated = true;
                    config.idle_timeout_secs = secs;
                    tm_idle_delay_radio.update_checked(secs);
                    os.set_idle_timeout(secs.saturating_mul(1000));
                }
                MenuEvent::SetVolumeOverlay(secs) => {
                    config_updated = true;
                    config.volume_overlay_secs = secs;
                    tm_volume_radio.update_checked(secs);
                }
                MenuEvent::SetDisplay(display) => {
                    config_updated = true;
                    config.display = display;
                    tm_display_radio.update_checked(display);
                    // If the current schedule entry sets the display, picking one here
                    // overrides it until the next entry starts (like a hold).
                    let entry_sets_display = last_active
                        .flatten()
                        .and_then(|i| config.schedule.get(i))
                        .is_some_and(|seg| seg.display.is_some());
                    display_override =
                        entry_sets_display.then(|| (display, schedule::next_boundary(&timings, Local::now())));
                }
                MenuEvent::SetTickerMode(mode) => {
                    config_updated = true;
                    config.ticker.mode = mode;
                    tm_ticker_radio.update_checked(mode);
                    ticker.set_config(&config.ticker);
                }
                MenuEvent::SetWeekend(mask) => {
                    tm_weekend_radio.update_checked(mask);
                    let spec = if mask == schedule::FRI_SUN {
                        "fri-sun"
                    } else {
                        "sat-sun"
                    };
                    match schedule::parse(&config.schedule, mask) {
                        Ok(new_timings) => {
                            config.weekend_days = spec.into();
                            timings = new_timings;
                            hold = None;
                            config_updated = true;
                            rebuild_hold_entries(sched_menu, &mut hold_entries, &config.schedule, &timings, &menu_tx);
                        }
                        Err(e) => show_warning(&e),
                    }
                }
                MenuEvent::ToggleSchedule => {
                    config_updated = true;
                    config.schedule_enabled = unsafe { sdl::SDL_GetTrayEntryChecked(tm_sched_enabled) };
                }
                MenuEvent::Hold(which) => {
                    hold = which.map(|i| (i, schedule::next_boundary(&timings, Local::now())));
                    refresh = true;
                }
                MenuEvent::EditConfig => {
                    let path = Config::path();
                    if !path.exists() && !config_save_blocked {
                        dialog_unwrap(config.save());
                    }
                    if config.schedule.is_empty()
                        && let Ok(text) = std::fs::read_to_string(&path)
                        && !text.contains("[[schedule]]")
                    {
                        _ = std::fs::write(&path, text + SCHEDULE_EXAMPLE);
                    }
                    open_in_editor(&path);
                }
                MenuEvent::ReloadConfig => match Config::load() {
                    Ok((new_config, new_timings)) => {
                        // Keep an active hold/override unless the schedule itself changed
                        let schedule_changed = new_config.schedule != config.schedule || new_timings != timings;
                        config = new_config;
                        timings = new_timings;
                        config_save_blocked = false;
                        if schedule_changed {
                            hold = None;
                            display_override = None;
                        }
                        refresh = true; // just loaded it; nothing to write back
                        tm_time_radio.update_checked(config.time_mode);
                        tm_status_notify.update_checked(config.status_notify);
                        tm_idle_delay_radio.update_checked(config.idle_timeout_secs);
                        tm_volume_radio.update_checked(config.volume_overlay_secs);
                        tm_shift_radio.update_checked(config.oled_shift);
                        tm_quit_mode_radio.update_checked(config.quit_mode);
                        tm_weekend_radio.update_checked(weekend_mask(&config));
                        tm_display_radio.update_checked(config.display);
                        tm_ticker_radio.update_checked(config.ticker.mode);
                        ticker.set_config(&config.ticker);
                        sysinfo.set_config(&config.sysinfo);
                        unsafe {
                            sdl::SDL_SetTrayEntryChecked(tm_media_check, config.show_media);
                            sdl::SDL_SetTrayEntryChecked(tm_idle_check, config.idle_timeout);
                            sdl::SDL_SetTrayEntryChecked(tm_sched_enabled, config.schedule_enabled);
                        }
                        dev.set_shift_mode(config.oled_shift.to_api());
                        rebuild_hold_entries(sched_menu, &mut hold_entries, &config.schedule, &timings, &menu_tx);
                    }
                    Err(e) => show_warning(&format!("{e}\n\nKept the previous settings.")),
                },
                MenuEvent::OpenSettings => {
                    let running = settings_child
                        .as_mut()
                        .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
                    if !running {
                        match std::env::current_exe()
                            .and_then(|exe| std::process::Command::new(exe).arg("--settings").spawn())
                        {
                            Ok(child) => settings_child = Some(child),
                            Err(e) => show_warning(&format!("Couldn't open settings: {e}")),
                        }
                    }
                }
                MenuEvent::ReapplyScarlett => scarlett.forget(),
                MenuEvent::Quit => break 'main,
            }
        }
        if config_updated || refresh {
            for (i, entry) in hold_entries.iter().enumerate() {
                unsafe { sdl::SDL_SetTrayEntryChecked(*entry, hold.is_some_and(|(h, _)| h == i)) };
            }
        }
        if config_updated && !config_save_blocked {
            dialog_unwrap(config.save());
            config_mtime = config_file_mtime();
        }

        let mut force_redraw = config_updated || refresh;

        // Handle events
        while let Some(event) = dev.try_event() {
            #[cfg(debug_assertions)]
            println!("event: {:?}", event);
            match event {
                DrawEvent::DeviceDisconnected => unsafe { sdl::SDL_SetTrayIcon(tray, icon_error.surf) },
                DrawEvent::DeviceReconnected => {
                    unsafe { sdl::SDL_SetTrayIcon(tray, icon.surf) }
                    dev.probe(); // re-probe when base station reappears
                    applied_brightness = None;
                }
                #[allow(clippy::single_match)]
                DrawEvent::DeviceEvent(event) => match event {
                    ggoled_lib::DeviceEvent::HeadsetConnection { wireless, .. } => {
                        if wireless != is_connected {
                            is_connected = wireless;
                            notif_expiry = Local::now() + NOTIF_DUR;
                            notif_update(
                                &mut dev,
                                &mut notif_layer,
                                config.status_notify,
                                is_connected,
                                notif_expiry,
                            );
                            force_redraw = true;
                        }
                    }
                    ggoled_lib::DeviceEvent::Volume { volume } => {
                        // The first reading comes from our own probe at startup/reconnect;
                        // only react to actual changes.
                        if let Some(prev) = last_volume
                            && prev != volume
                            && config.volume_overlay_secs > 0
                        {
                            volume_hold_until = Instant::now() + Duration::from_secs(config.volume_overlay_secs as u64);
                        }
                        last_volume = Some(volume);
                    }
                    _ => {}
                },
            }
        }

        // Update layers every second
        let time = Local::now();
        if time.second() != last_time.second() || force_redraw {
            last_time = time;

            // Remove expired notifications in Notify mode
            if config.status_notify == StatusNotifyMode::Notify
                && time >= notif_expiry
                && let Some(layer) = notif_layer.take()
            {
                dev.remove_layer(layer);
            }

            // Work out which schedule entry (if any) is in effect
            if let Some((_, Some(until))) = hold
                && time >= until
            {
                hold = None;
                for entry in &hold_entries {
                    unsafe { sdl::SDL_SetTrayEntryChecked(*entry, false) };
                }
            }
            // Locked PC: use the chosen entry, after 5 s (a UAC prompt also briefly
            // looks locked). Unlocking takes effect immediately.
            let lock_idx = config.lock_entry_index();
            let lock_state = if lock_idx.is_some() {
                os.is_locked()
            } else {
                Some(false)
            };
            if lock_state == Some(true) {
                locked_since.get_or_insert_with(Instant::now);
            } else {
                locked_since = None;
            }
            let locked = locked_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(5));

            let active_idx = match (locked, hold) {
                (true, _) => lock_idx,
                (false, Some((i, _))) => Some(i),
                (false, None) if config.schedule_enabled => schedule::active(&timings, time),
                (false, None) => None,
            };
            if let Some((_, Some(until))) = display_override
                && time >= until
            {
                display_override = None;
            }
            eff = Effective::new(&config, active_idx.and_then(|i| config.schedule.get(i)));
            if let Some((display, _)) = display_override
                && !locked
            {
                eff.display = display;
            }
            // A game in the foreground shows system info (unless locked)
            let gaming = !locked && config.sysinfo.auto_when_gaming && sysinfo.gaming();
            if gaming {
                eff.display = DisplayMode::SystemInfo;
            }
            if locked && !config.lock_scarlett {
                eff.scarlett_led_timeout = None; // leave the Scarlett as it is
            }
            if last_active != Some(active_idx) {
                last_active = Some(active_idx);
                last_media = None; // relayout with the new settings
            }
            os.set_idle_timeout(eff.screensaver_secs.saturating_mul(1000));
            if let Some(b) = eff.brightness
                && applied_brightness != Some(b)
            {
                dev.set_brightness(b);
                applied_brightness = Some(b);
            }

            // Tray status line
            let until_str = |t: Option<DateTime<Local>>| match t {
                Some(t) => format!("until {}", t.format("%a %H:%M")),
                None => "until ended".to_string(),
            };
            let status = match (hold, active_idx) {
                _ if locked => format!(
                    "Locked: {}",
                    segment_label(&config.schedule, &timings, active_idx.unwrap_or(0))
                ),
                (Some((i, until)), _) => {
                    format!(
                        "Holding: {} ({})",
                        segment_label(&config.schedule, &timings, i),
                        until_str(until)
                    )
                }
                (None, Some(i)) => format!(
                    "Now: {} ({})",
                    segment_label(&config.schedule, &timings, i),
                    until_str(schedule::next_boundary(&timings, time))
                ),
                (None, None) if config.schedule_enabled => "Schedule: no entries".to_string(),
                (None, None) => "Schedule: off".to_string(),
            };
            let status = match display_override {
                Some(_) if !locked => format!("{status} · display set manually"),
                _ => status,
            };
            let status = match (gaming, sysinfo.snapshot().game) {
                (true, Some(game)) => format!("{status} · game: {}", game.trim_end_matches(".exe")),
                (true, None) => format!("{status} · game"),
                _ => status,
            };
            let status = match lock_state {
                None => format!("{status} · can't detect locking"),
                _ => status,
            };
            // Keep the Display menu showing what's actually on screen
            if shown_display != Some(eff.display) {
                shown_display = Some(eff.display);
                tm_display_radio.update_checked(eff.display);
            }
            if status != last_status {
                if let Ok(c) = CString::new(status.clone()) {
                    unsafe { sdl::SDL_SetTrayEntryLabel(tm_status, c.as_ptr()) };
                }
                last_status = status;
            }

            // Media (fetched every second: also used to avoid interrupting playback)
            let playing_media = os.get_media();
            // Changing the Scarlett briefly cuts its audio: wait for media and games to finish
            scarlett.update(
                &config.scarlett,
                eff.scarlett_led_timeout,
                playing_media.is_some() || sysinfo.game_running(),
            );

            // Check if idle
            let screensaver_active = eff.screensaver && os.is_idle();
            screen_blank = screensaver_active || eff.display == DisplayMode::Blank;
            let show_ticker = !screen_blank && eff.display == DisplayMode::Ticker;
            ticker.set_active(show_ticker);
            let show_sysinfo = !screen_blank && eff.display == DisplayMode::SystemInfo;
            sysinfo.set_active(show_sysinfo);
            if !show_sysinfo && let Some(id) = sysinfo_layer.take() {
                dev.remove_layer(id);
            }
            if !show_ticker {
                if let Some(id) = ticker_layer.take() {
                    dev.remove_layer(id);
                    ticker_key = None;
                }
                if let Some(id) = ticker_header_layer.take() {
                    dev.remove_layer(id);
                    ticker_header_text = None;
                }
            }
            if screen_blank {
                dev.clear_layers(); // clear screen when idle
                notif_layer = None;
                ticker_layer = None;
                ticker_key = None;
                ticker_header_layer = None;
                ticker_header_text = None;
                sysinfo_layer = None;
                last_media = None; // reset media so we check again when not idle
            } else if show_sysinfo {
                // System info replaces the clock and media
                if notif_layer.is_none() {
                    notif_update(
                        &mut dev,
                        &mut notif_layer,
                        config.status_notify,
                        is_connected,
                        notif_expiry,
                    );
                }
                dev.remove_layers(&time_layers);
                dev.remove_layers(&media_layers);
                time_layers.clear();
                media_layers.clear();
                last_media = None;
                let margin = match config.oled_shift {
                    ConfigShiftMode::Off => 0,
                    ConfigShiftMode::Low => 1,
                    ConfigShiftMode::High => 4,
                };
                let bitmap = Arc::new(ggoled_sysinfo::render(&sysinfo.snapshot(), &sysinfo_fonts, margin));
                dev.pause();
                if let Some(id) = sysinfo_layer.take() {
                    dev.remove_layer(id);
                }
                sysinfo_layer = Some(dev.add_layer(DrawLayer::Image { bitmap, x: 0, y: 0 }));
                dev.play();
            } else if show_ticker {
                // Price ticker replaces the clock and media
                if notif_layer.is_none() {
                    notif_update(
                        &mut dev,
                        &mut notif_layer,
                        config.status_notify,
                        is_connected,
                        notif_expiry,
                    );
                }
                dev.remove_layers(&time_layers);
                dev.remove_layers(&media_layers);
                time_layers.clear();
                media_layers.clear();
                last_media = None;

                let quotes = ticker.quotes();
                let tcfg = &config.ticker;
                // One at a time: refresh a second before each switch, so every asset
                // appears with a current price
                let cycle = tcfg.cycle_secs.max(1) as i64;
                if tcfg.mode == TickerMode::Cycle && cycle >= 2 {
                    let next_step = (time.timestamp() + 1) / cycle;
                    if (time.timestamp() + 1) % cycle == 0 && ticker_prefetched != Some(next_step) {
                        ticker_prefetched = Some(next_step);
                        ticker.request_refresh();
                    }
                }
                let margin = match config.oled_shift {
                    ConfigShiftMode::Off => 0,
                    ConfigShiftMode::Low => 1,
                    ConfigShiftMode::High => 4,
                };
                let (main_q, compact_q) = ticker.split_quotes();
                let (grid_rows, grid_pages) = ggoled_ticker::grid_pages(main_q.len(), !compact_q.is_empty());
                let step = time.timestamp().max(0) as usize / tcfg.cycle_secs.max(1) as usize;
                let page = match tcfg.mode {
                    TickerMode::Cycle => step % quotes.len().max(1),
                    TickerMode::Grid => step % grid_pages,
                    TickerMode::Scroll => 0,
                };

                // List layout: top row (time or media) when there's a spare row
                let mut header_text: Option<String> = None;
                let mut header_scroll: Option<String> = None;
                if tcfg.mode == TickerMode::Grid
                    && grid_pages == 1
                    && ggoled_ticker::grid_has_header_room(main_q.len(), !compact_q.is_empty())
                {
                    let time_text = match eff.time_mode {
                        ConfigTimeMode::Off => None,
                        ConfigTimeMode::H24 => Some(time.format("%H:%M:%S").to_string()),
                        ConfigTimeMode::H12 => Some(time.format("%l:%M:%S %p").to_string().trim().to_string()),
                    };
                    let media_text = playing_media.as_ref().map(|m| match m.artist.is_empty() {
                        true => m.title.clone(),
                        false => format!("{} - {}", m.title, m.artist),
                    });
                    match (tcfg.grid_header, media_text) {
                        (GridHeader::Media, Some(m)) => {
                            if ticker_fonts.small.render_line(&m).w > ggoled_ticker::WIDTH - 2 * margin {
                                header_scroll = Some(m);
                            } else {
                                header_text = Some(m);
                            }
                        }
                        (GridHeader::None, _) => {}
                        _ => header_text = time_text,
                    }
                }
                // Scrolling header text lives in its own layer, only rebuilt when it changes
                if header_scroll.as_deref() != ticker_header_text.as_deref() {
                    if let Some(id) = ticker_header_layer.take() {
                        dev.remove_layer(id);
                    }
                    if let Some(text) = &header_scroll {
                        let bitmap = Arc::new(ticker_fonts.small.render_line(text));
                        let y = ggoled_ticker::grid_header_y(main_q.len(), !compact_q.is_empty(), margin);
                        ticker_header_layer = Some(dev.add_layer(DrawLayer::Scroll { bitmap, y }));
                    }
                    ticker_header_text = header_scroll.clone();
                }

                // What's on screen; rebuild only when it changes (rebuilding restarts scrolling)
                let content = {
                    let mut h = std::hash::DefaultHasher::new();
                    match tcfg.mode {
                        TickerMode::Scroll => ggoled_ticker::scroll_text(&quotes).hash(&mut h),
                        _ => ticker.generation().hash(&mut h),
                    }
                    (&header_text, &header_scroll, margin).hash(&mut h);
                    h.finish()
                };
                let key = (content, tcfg.mode, page);
                if ticker_key != Some(key) {
                    ticker_key = Some(key);
                    dev.pause();
                    if let Some(id) = ticker_layer.take() {
                        dev.remove_layer(id);
                    }
                    if !quotes.is_empty() {
                        let layer = match tcfg.mode {
                            TickerMode::Cycle => DrawLayer::Image {
                                bitmap: Arc::new(ggoled_ticker::render_cycle(&quotes[page], &ticker_fonts, margin)),
                                x: 0,
                                y: 0,
                            },
                            TickerMode::Grid => {
                                let header = match (&header_text, &header_scroll) {
                                    (Some(t), _) => ggoled_ticker::Header::Text(t),
                                    (None, Some(_)) => ggoled_ticker::Header::Reserved,
                                    _ => ggoled_ticker::Header::None,
                                };
                                let start = (page * grid_rows).min(main_q.len());
                                DrawLayer::Image {
                                    bitmap: Arc::new(ggoled_ticker::render_grid(
                                        &main_q[start..],
                                        &compact_q,
                                        header,
                                        &ticker_fonts,
                                        margin,
                                    )),
                                    x: 0,
                                    y: 0,
                                }
                            }
                            TickerMode::Scroll => {
                                let bitmap = Arc::new(ggoled_ticker::render_scroll(&quotes, &ticker_fonts));
                                let y = (ggoled_ticker::HEIGHT as isize - bitmap.h as isize) / 2;
                                if bitmap.w >= ggoled_ticker::WIDTH {
                                    DrawLayer::Scroll { bitmap, y }
                                } else {
                                    DrawLayer::Image { bitmap, x: 0, y }
                                }
                            }
                        };
                        ticker_layer = Some(dev.add_layer(layer));
                    }
                    dev.play();
                }
            } else {
                if let Some(id) = ticker_layer.take() {
                    dev.remove_layer(id);
                    ticker_key = None;
                }
                if let Some(id) = ticker_header_layer.take() {
                    dev.remove_layer(id);
                    ticker_header_text = None;
                }
                // Update notifications
                if notif_layer.is_none() {
                    notif_update(
                        &mut dev,
                        &mut notif_layer,
                        config.status_notify,
                        is_connected,
                        notif_expiry,
                    );
                }

                // Fetch media once a second (before pausing screen)
                let media = if eff.show_media { playing_media } else { None };

                dev.pause();

                // Time
                dev.remove_layers(&time_layers);
                if eff.time_mode != ConfigTimeMode::Off {
                    let time_str = match eff.time_mode {
                        ConfigTimeMode::Off => unreachable!(),
                        ConfigTimeMode::H24 => time.format("%H:%M:%S").to_string(),
                        ConfigTimeMode::H12 => time.format("%l:%M:%S %p").to_string(),
                    };
                    time_layers = dev.add_text(&time_str, None, if media.is_some() { Some(8) } else { None });
                }

                // Media
                if media != last_media {
                    dev.remove_layers(&media_layers);
                    if let Some(m) = &media {
                        media_layers = dev.add_text(
                            &format!("{}\n{}", m.title, m.artist),
                            None,
                            Some(8 + dev.font_line_height() as isize),
                        );
                    }
                    last_media = media;
                }

                dev.play();
            }
        }

        // Decide who owns the screen: us, or the SteelSeries UI.
        // - Right after a volume change, let the stock volume screen show.
        // - If we have nothing to draw (and aren't blanking for the screensaver),
        //   hand the screen back instead of drawing an empty black frame over it.
        let want_stock = Instant::now() < volume_hold_until
            || eff.display == DisplayMode::Stock
            || (!screen_blank && dev.layer_count() == 0);
        if want_stock != stock_ui {
            stock_ui = want_stock;
            dev.set_stock_ui(want_stock);
        }

        sleep(Duration::from_millis(10));
    }
    let dev = dev.stop();
    match config.quit_mode {
        ConfigQuitMode::Screensaver => dev.draw(&Bitmap::new(dev.width, dev.height, false), 0, 0),
        ConfigQuitMode::StockUi => dev.return_to_ui(),
    }
    .unwrap();
}
