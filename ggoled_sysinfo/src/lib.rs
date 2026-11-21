//! System info for the OLED: CPU/GPU load and temperatures, memory, and FPS,
//! plus detecting when a game is in the foreground.
//!
//! Sources, each optional (missing ones are simply left out):
//! - CPU load, RAM: the OS (sysinfo crate)
//! - NVIDIA GPU: NVML, the monitoring library that ships with the driver
//! - CPU temperature/power: LibreHardwareMonitor's local web server, if running
//! - FPS and "is a game running": RivaTuner Statistics Server's shared memory
//!   (Windows), plus a configurable list of game programs

#[cfg(windows)]
mod win;

use ggoled_draw::TextRenderer;
use ggoled_lib::Bitmap;
use ggoled_ticker::{BIG_INK_H, BIG_INK_TOP, SMALL_INK_H, SMALL_INK_TOP, blit, font5x7, spread};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub const WIDTH: usize = 128;
/// Bytes per GB as Task Manager counts them
const GIB: f32 = 1024.0 * 1024.0 * 1024.0;
pub const HEIGHT: usize = 64;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SysInfoConfig {
    /// Show system info while a game is in the foreground.
    pub auto_when_gaming: bool,
    /// Programs that count as games (e.g. "Warframe.x64.exe"), in addition to
    /// anything RTSS is measuring in the foreground.
    pub game_programs: Vec<String>,
    /// Programs never treated as games, even if RTSS measures them.
    pub ignore_programs: Vec<String>,
    /// Keep showing system info this long after the game leaves the foreground
    /// (avoids flicker when alt-tabbing).
    pub linger_secs: u32,
    /// LibreHardwareMonitor's web server (for CPU temperature)
    pub lhm_url: String,
}
impl Default for SysInfoConfig {
    fn default() -> Self {
        Self {
            auto_when_gaming: true,
            game_programs: vec![],
            ignore_programs: [
                "explorer.exe",
                "chrome.exe",
                "msedge.exe",
                "firefox.exe",
                "brave.exe",
                "opera.exe",
                "discord.exe",
                "spotify.exe",
                "steam.exe",
                "steamwebhelper.exe",
                "code.exe",
                "obs64.exe",
                "nvidia app.exe",
                "ggoled_app.exe",
            ]
            .map(String::from)
            .to_vec(),
            linger_secs: 10,
            lhm_url: "http://localhost:8085/data.json".into(),
        }
    }
}

/// Latest readings. `None` = not available.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cpu_load: Option<f32>,
    pub cpu_temp: Option<f32>,
    pub cpu_power: Option<f32>,
    pub gpu_load: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub gpu_power: Option<f32>,
    /// GB in binary units (GiB), as Windows' Task Manager shows them
    pub vram_used: Option<f32>,
    pub vram_total: Option<f32>,
    pub ram_used: Option<f32>,
    pub ram_total: Option<f32>,
    pub fps: Option<f32>,
    /// The game in the foreground, if any (program name)
    pub game: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum SourceStatus {
    #[default]
    NotChecked,
    Ok,
    Unavailable(String),
}

/// Which sources are working (for the settings window).
#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub nvidia: SourceStatus,
    pub lhm: SourceStatus,
    pub rtss: SourceStatus,
}

struct Shared {
    config: SysInfoConfig,
    snapshot: Snapshot,
    sources: Sources,
    /// Collect readings (CPU/GPU/LHM); game detection runs regardless
    active: bool,
    gaming_until: Option<Instant>,
    /// A game is running, in front or not (for waiting before disruptive changes)
    running_until: Option<Instant>,
}

/// Collects readings in a background thread, once a second.
pub struct SysInfo {
    shared: Arc<Mutex<Shared>>,
}

impl SysInfo {
    pub fn new(config: SysInfoConfig) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            config,
            snapshot: Snapshot::default(),
            sources: Sources::default(),
            active: false,
            gaming_until: None,
            running_until: None,
        }));
        let thread_shared = shared.clone();
        thread::spawn(move || collector(thread_shared));
        Self { shared }
    }
    pub fn set_config(&self, config: &SysInfoConfig) {
        self.shared.lock().config = config.clone();
    }
    /// Collect CPU/GPU readings (only needed while system info is shown).
    pub fn set_active(&self, active: bool) {
        self.shared.lock().active = active;
    }
    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().snapshot.clone()
    }
    pub fn sources(&self) -> Sources {
        self.shared.lock().sources.clone()
    }
    /// Whether a game is running, even in the background: RTSS is measuring
    /// frames for a program that isn't excluded, or a game is in front. Stays
    /// true for a few seconds after, so a loading screen doesn't count as closed.
    pub fn game_running(&self) -> bool {
        self.shared.lock().running_until.is_some_and(|t| Instant::now() < t)
    }
    /// Whether a game is (or very recently was) in the foreground.
    pub fn gaming(&self) -> bool {
        self.shared.lock().gaming_until.is_some_and(|t| Instant::now() < t)
    }
}

fn collector(shared: Arc<Mutex<Shared>>) {
    let mut sys = sysinfo::System::new().ok();
    let nvml = nvml_wrapper::Nvml::init();
    {
        let mut s = shared.lock();
        s.sources.nvidia = match &nvml {
            Ok(_) => SourceStatus::Ok,
            Err(e) => SourceStatus::Unavailable(format!("NVIDIA driver library not found ({e})")),
        };
    }
    let nvml = nvml.ok();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(800)))
        .build()
        .into();
    let mut lhm_retry_at = Instant::now();
    let mut rtss_seen: HashMap<u32, (u32, Instant)> = HashMap::new(); // pid -> (time1, when it last changed)

    loop {
        let (config, active) = {
            let s = shared.lock();
            (s.config.clone(), s.active)
        };
        let mut snap = Snapshot::default();
        let mut sources = shared.lock().sources.clone();

        // Game detection: what's in the foreground, and is RTSS measuring it?
        let foreground = foreground_program();
        let rtss = rtss_apps();
        sources.rtss = match &rtss {
            Some(_) => SourceStatus::Ok,
            None if cfg!(windows) => SourceStatus::Unavailable("RTSS isn't running".into()),
            None => SourceStatus::Unavailable("Windows only".into()),
        };
        let now = Instant::now();
        if let Some(apps) = &rtss {
            for app in apps {
                let entry = rtss_seen
                    .entry(app.pid)
                    .or_insert((app.time1, now - Duration::from_secs(60)));
                if entry.0 != app.time1 {
                    *entry = (app.time1, now);
                }
            }
            rtss_seen.retain(|pid, _| apps.iter().any(|a| a.pid == *pid));
        }
        let mut game = None;
        if let Some((pid, exe)) = &foreground {
            let ignored = config.ignore_programs.iter().any(|p| same_program(p, exe));
            let listed = config.game_programs.iter().any(|p| same_program(p, exe));
            let measured = rtss.as_ref().and_then(|apps| apps.iter().find(|a| a.pid == *pid));
            let updating = rtss_seen
                .get(pid)
                .is_some_and(|(_, changed)| now.duration_since(*changed) < Duration::from_secs(3));
            if !ignored && (listed || (measured.is_some() && updating)) {
                game = Some(exe.clone());
                if updating {
                    snap.fps = measured.and_then(|a| a.fps());
                }
            }
        }
        snap.game = game.clone();
        // Any game RTSS is drawing frames for, in front or not
        let background_game = rtss.as_ref().is_some_and(|apps| {
            apps.iter().any(|a| {
                let exe = a.name.rsplit(['\\', '/']).next().unwrap_or(&a.name);
                let updating = rtss_seen
                    .get(&a.pid)
                    .is_some_and(|(_, changed)| now.duration_since(*changed) < Duration::from_secs(3));
                updating && !config.ignore_programs.iter().any(|p| same_program(p, exe))
            })
        });

        if active {
            if let Some(sys) = &mut sys {
                sys.refresh_cpu_usage();
                sys.refresh_memory();
                snap.cpu_load = Some(sys.global_cpu_usage());
                snap.ram_used = Some(sys.used_memory() as f32 / GIB);
                snap.ram_total = Some(sys.total_memory() as f32 / GIB);
            }
            if let Some(nvml) = &nvml
                && let Ok(dev) = nvml.device_by_index(0)
            {
                use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
                snap.gpu_temp = dev.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f32);
                snap.gpu_load = dev.utilization_rates().ok().map(|u| u.gpu as f32);
                snap.gpu_power = dev.power_usage().ok().map(|mw| mw as f32 / 1000.0);
                if let Ok(mem) = dev.memory_info() {
                    snap.vram_used = Some(mem.used as f32 / GIB);
                    snap.vram_total = Some(mem.total as f32 / GIB);
                }
            }
            if now >= lhm_retry_at {
                match read_lhm(&agent, &config.lhm_url) {
                    Ok((temp, power)) => {
                        snap.cpu_temp = temp;
                        snap.cpu_power = power;
                        sources.lhm = match temp {
                            Some(_) => SourceStatus::Ok,
                            None => SourceStatus::Unavailable("no CPU temperature sensor found".into()),
                        };
                    }
                    Err(e) => {
                        sources.lhm = SourceStatus::Unavailable(e);
                        lhm_retry_at = now + Duration::from_secs(15); // don't keep knocking
                    }
                }
            }
        }

        {
            let mut s = shared.lock();
            s.snapshot = snap;
            s.sources = sources;
            if game.is_some() && config.auto_when_gaming {
                s.gaming_until = Some(now + Duration::from_secs(config.linger_secs as u64));
            }
            if game.is_some() || background_game {
                s.running_until = Some(now + Duration::from_secs(15));
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// "Warframe.x64.exe" matches "warframe.x64.exe" and "Warframe.x64".
fn same_program(configured: &str, exe: &str) -> bool {
    let c = configured.trim().to_ascii_lowercase();
    let e = exe.to_ascii_lowercase();
    c == e || format!("{c}.exe") == e
}

// ---------------------------------------------------------------------------
// Platform pieces

#[derive(Debug, Clone)]
pub struct RtssApp {
    pub pid: u32,
    pub name: String,
    pub time0: u32,
    pub time1: u32,
    pub frames: u32,
}
impl RtssApp {
    pub fn fps(&self) -> Option<f32> {
        let dt = self.time1.wrapping_sub(self.time0);
        (dt > 0 && dt < 10_000).then(|| self.frames as f32 * 1000.0 / dt as f32)
    }
}

#[cfg(windows)]
fn foreground_program() -> Option<(u32, String)> {
    win::foreground_program()
}
#[cfg(not(windows))]
fn foreground_program() -> Option<(u32, String)> {
    None
}
#[cfg(windows)]
fn rtss_apps() -> Option<Vec<RtssApp>> {
    win::rtss_apps()
}
#[cfg(not(windows))]
fn rtss_apps() -> Option<Vec<RtssApp>> {
    None
}

/// LibreHardwareMonitor's data.json: a tree of nodes; sensors have "SensorId",
/// "Text", "Type" and a "Value" like "61,0 °C" (decimal comma in some locales).
fn read_lhm(agent: &ureq::Agent, url: &str) -> Result<(Option<f32>, Option<f32>), String> {
    let body = agent
        .get(url)
        .call()
        .map_err(|_| "LibreHardwareMonitor's web server isn't reachable".to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("unexpected data: {e}"))?;
    let mut sensors = vec![];
    collect_sensors(&json, &mut sensors);
    Ok((
        pick_cpu_sensor(&sensors, "/temperature/"),
        pick_cpu_sensor(&sensors, "/power/"),
    ))
}

struct LhmSensor {
    id: String,
    text: String,
    value: Option<f32>,
}

fn collect_sensors(node: &serde_json::Value, out: &mut Vec<LhmSensor>) {
    if let Some(id) = node.get("SensorId").and_then(|v| v.as_str()) {
        out.push(LhmSensor {
            id: id.to_string(),
            text: node.get("Text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            value: node.get("Value").and_then(|v| v.as_str()).and_then(parse_lhm_value),
        });
    }
    if let Some(children) = node.get("Children").and_then(|c| c.as_array()) {
        for child in children {
            collect_sensors(child, out);
        }
    }
}

/// "61,0 °C" -> 61.0, "45.3 W" -> 45.3
fn parse_lhm_value(s: &str) -> Option<f32> {
    let num: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.' || *c == '-')
        .collect();
    num.replace(',', ".").parse().ok()
}

/// The main CPU sensor of a kind: "CPU Package" (Intel) or "Core (Tctl/Tdie)"
/// (AMD) if present, else the highest CPU reading of that kind.
fn pick_cpu_sensor(sensors: &[LhmSensor], kind: &str) -> Option<f32> {
    let cpu: Vec<&LhmSensor> = sensors
        .iter()
        .filter(|s| (s.id.contains("cpu/")) && s.id.contains(kind) && s.value.is_some())
        .collect();
    let preferred = [
        "CPU Package",
        "Package",
        "Core (Tctl/Tdie)",
        "Core (Tctl)",
        "Tdie",
        "CPU Total",
    ];
    for name in preferred {
        if let Some(s) = cpu.iter().find(|s| s.text.eq_ignore_ascii_case(name)) {
            return s.value;
        }
    }
    cpu.iter().filter_map(|s| s.value).reduce(f32::max)
}

// ---------------------------------------------------------------------------
// Rendering

pub struct Fonts {
    pub small: TextRenderer,
    pub big: TextRenderer,
}
impl Default for Fonts {
    fn default() -> Self {
        Self {
            small: TextRenderer::new_pixel_operator_sized(16.0),
            big: TextRenderer::new_pixel_operator_sized(32.0),
        }
    }
}

fn gb(v: f32) -> String {
    if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") }
}

fn load_temp(load: Option<f32>, temp: Option<f32>) -> String {
    let load = load.map(|l| format!("{l:.0}%")).unwrap_or_else(|| "--".into());
    match temp {
        Some(t) => format!("{load}  {t:.0}°C"),
        None => load,
    }
}

/// The system info screen. With FPS (a game measured by RTSS): large FPS, CPU
/// and GPU rows, and memory in small text. Otherwise CPU, GPU, VRAM and RAM rows.
/// Content stays `margin` px from every edge (OLED shift).
pub fn render(s: &Snapshot, fonts: &Fonts, margin: usize) -> Bitmap {
    let mut b = Bitmap::new(WIDTH, HEIGHT, false);
    let (left, right) = (margin as isize, (WIDTH - margin) as isize);
    let row = |b: &mut Bitmap, y_ink: isize, label: &str, value: &str| {
        let y = y_ink - SMALL_INK_TOP;
        let l = fonts.small.render_line(label);
        blit(b, &l, left, y);
        let v = fonts.small.render_line(value);
        blit(b, &v, right - v.w as isize, y);
    };
    let cpu = load_temp(s.cpu_load, s.cpu_temp);
    let gpu = load_temp(s.gpu_load, s.gpu_temp);

    if let Some(fps) = s.fps {
        let tops = spread(
            &[BIG_INK_H, SMALL_INK_H, SMALL_INK_H, font5x7::GLYPH_H as isize],
            margin,
        );
        let big = fonts.big.render_line(&format!("{fps:.0}"));
        blit(&mut b, &big, left, tops[0] - BIG_INK_TOP);
        // "FPS" label, bottom-aligned with the big digits
        let label = fonts.small.render_line("FPS");
        blit(
            &mut b,
            &label,
            left + big.w as isize + 4,
            tops[0] + BIG_INK_H - SMALL_INK_TOP - 9,
        );
        if let Some(w) = s.gpu_power {
            let p = fonts.small.render_line(&format!("{w:.0} W"));
            blit(
                &mut b,
                &p,
                right - p.w as isize,
                tops[0] + BIG_INK_H - SMALL_INK_TOP - 9,
            );
        }
        row(&mut b, tops[1], "CPU", &cpu);
        row(&mut b, tops[2], "GPU", &gpu);
        // used/total in whole GB, e.g. "VRAM 7/16  RAM 19/64"
        let pair = |used: Option<f32>, total: Option<f32>| match (used, total) {
            (Some(u), Some(t)) => Some(format!("{:.0}/{:.0}", u, t)),
            (Some(u), None) => Some(format!("{u:.0}")),
            _ => None,
        };
        let mut mem = vec![];
        if let Some(v) = pair(s.vram_used, s.vram_total) {
            mem.push(format!("VRAM {v}"));
        }
        if let Some(r) = pair(s.ram_used, s.ram_total) {
            mem.push(format!("RAM {r}"));
        }
        let mut text = mem.join("  ");
        if font5x7::text_width(&text) > WIDTH - 2 * margin {
            text = mem.join(" ");
        }
        let m = font5x7::render(&text);
        blit(&mut b, &m, (WIDTH as isize - m.w as isize) / 2, tops[3]);
    } else {
        let tops = spread(&[SMALL_INK_H; 4], margin);
        row(&mut b, tops[0], "CPU", &cpu);
        row(&mut b, tops[1], "GPU", &gpu);
        let vram = match (s.vram_used, s.vram_total) {
            (Some(u), Some(t)) => format!("{}/{} GB", gb(u), gb(t)),
            _ => "--".into(),
        };
        row(&mut b, tops[2], "VRAM", &vram);
        let ram = match (s.ram_used, s.ram_total) {
            (Some(u), Some(t)) => format!("{}/{} GB", gb(u), gb(t)),
            _ => "--".into(),
        };
        row(&mut b, tops[3], "RAM", &ram);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample(fps: Option<f32>) -> Snapshot {
        Snapshot {
            cpu_load: Some(34.0),
            cpu_temp: Some(61.4),
            cpu_power: Some(88.0),
            gpu_load: Some(98.0),
            gpu_temp: Some(72.0),
            gpu_power: Some(245.0),
            vram_used: Some(7.1),
            vram_total: Some(16.0),
            ram_used: Some(19.4),
            ram_total: Some(63.7),
            fps,
            game: fps.map(|_| "Warframe.x64.exe".into()),
        }
    }

    fn ink_bounds(b: &Bitmap) -> (usize, usize, usize, usize) {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..b.h {
            for x in 0..b.w {
                if b.data[y * b.w + x] {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
        }
        (x0, y0, x1, y1)
    }

    #[test]
    fn stays_inside_margins() {
        let fonts = Fonts::default();
        for fps in [Some(144.0), Some(1234.0), None] {
            for margin in [0, 1, 4] {
                let (x0, y0, x1, y1) = ink_bounds(&render(&sample(fps), &fonts, margin));
                assert!(x0 >= margin && y0 >= margin, "fps {fps:?} margin {margin}: {x0},{y0}");
                assert!(
                    x1 < WIDTH - margin && y1 < HEIGHT - margin,
                    "fps {fps:?} margin {margin}: {x1},{y1}"
                );
            }
        }
        // nothing available at all still renders
        render(&Snapshot::default(), &fonts, 4);
    }

    #[test]
    fn lhm_parsing() {
        assert_eq!(parse_lhm_value("61,0 °C"), Some(61.0));
        assert_eq!(parse_lhm_value("45.3 W"), Some(45.3));
        assert_eq!(parse_lhm_value(""), None);
        let json: serde_json::Value = serde_json::from_str(
            r#"{"id":0,"Text":"Sensor","Children":[{"id":1,"Text":"Intel Core i7","Children":[
                {"id":2,"Text":"Temperatures","Children":[
                    {"id":3,"Text":"CPU Core #1","Value":"58,0 °C","SensorId":"/intelcpu/0/temperature/0","Type":"Temperature","Children":[]},
                    {"id":4,"Text":"CPU Package","Value":"63,0 °C","SensorId":"/intelcpu/0/temperature/8","Type":"Temperature","Children":[]}]},
                {"id":5,"Text":"Powers","Children":[
                    {"id":6,"Text":"CPU Package","Value":"88,5 W","SensorId":"/intelcpu/0/power/0","Type":"Power","Children":[]}]}]},
              {"id":7,"Text":"NVIDIA","Children":[
                    {"id":8,"Text":"GPU Core","Value":"72,0 °C","SensorId":"/gpu-nvidia/0/temperature/0","Type":"Temperature","Children":[]}]}]}"#,
        )
        .unwrap();
        let mut sensors = vec![];
        collect_sensors(&json, &mut sensors);
        assert_eq!(pick_cpu_sensor(&sensors, "/temperature/"), Some(63.0));
        assert_eq!(pick_cpu_sensor(&sensors, "/power/"), Some(88.5));
    }

    #[test]
    fn program_matching() {
        assert!(same_program("Warframe.x64.exe", "warframe.x64.exe"));
        assert!(same_program("Warframe.x64", "Warframe.x64.exe"));
        assert!(!same_program("chrome", "chromedriver.exe"));
    }

    #[test]
    fn rtss_fps() {
        let app = RtssApp {
            pid: 1,
            name: "game.exe".into(),
            time0: 10_000,
            time1: 11_000,
            frames: 144,
        };
        assert_eq!(app.fps(), Some(144.0));
    }
}
