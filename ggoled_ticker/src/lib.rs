//! Price ticker for the 128x64 OLED: fetches quotes in the background and renders
//! them in a few layouts.
//!
//! Data comes from Yahoo Finance's public chart endpoint (no API key; works for
//! stocks like "MSTR" and crypto pairs like "BTC-USD"). It's unofficial, so the
//! fetching is kept separate from the rendering in case it needs replacing.

use ggoled_draw::TextRenderer;
use ggoled_lib::Bitmap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum TickerMode {
    /// One asset at a time: large price, change, and today's trend line.
    #[default]
    Cycle,
    /// Up to four assets at once, one per row (pages if there are more).
    Grid,
    /// All assets on one line scrolling across the screen.
    Scroll,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TickerConfig {
    /// Yahoo Finance symbols, e.g. "BTC-USD", "MSTR", "^GSPC"
    pub symbols: Vec<String>,
    pub mode: TickerMode,
    /// Seconds per asset (Cycle) or per page (Grid)
    pub cycle_secs: u32,
    /// Seconds between price updates
    pub refresh_secs: u32,
}
impl Default for TickerConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTC-USD".into(), "MSTR".into()],
            mode: TickerMode::Cycle,
            cycle_secs: 6,
            refresh_secs: 60,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Quote {
    pub symbol: String,
    pub price: Option<f64>,
    pub prev_close: Option<f64>,
    /// Today's prices, oldest first (for the trend line)
    pub intraday: Vec<f64>,
    pub updated: Option<Instant>,
    pub error: Option<String>,
}
impl Quote {
    /// Short name for display: "BTC-USD" -> "BTC", "^GSPC" -> "GSPC"
    pub fn label(&self) -> String {
        let s = self.symbol.trim_start_matches('^');
        let s = s.strip_suffix("-USD").unwrap_or(s);
        s.to_string()
    }
    pub fn change_pct(&self) -> Option<f64> {
        match (self.price, self.prev_close) {
            (Some(p), Some(c)) if c != 0.0 => Some((p - c) / c * 100.0),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Fetching

#[derive(Deserialize)]
struct ChartResponse {
    chart: Chart,
}
#[derive(Deserialize)]
struct Chart {
    result: Option<Vec<ChartResult>>,
    error: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct ChartResult {
    meta: ChartMeta,
    indicators: Option<Indicators>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChartMeta {
    regular_market_price: Option<f64>,
    chart_previous_close: Option<f64>,
    previous_close: Option<f64>,
}
#[derive(Deserialize)]
struct Indicators {
    quote: Vec<IndicatorQuote>,
}
#[derive(Deserialize)]
struct IndicatorQuote {
    close: Option<Vec<Option<f64>>>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent("Mozilla/5.0 (ggoled)")
        .build()
        .into()
}

pub fn fetch_quote(agent: &ureq::Agent, symbol: &str) -> anyhow::Result<Quote> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?range=1d&interval=5m",
        symbol.replace('^', "%5E")
    );
    let body = agent.get(&url).call()?.body_mut().read_to_string()?;
    let resp: ChartResponse = serde_json::from_str(&body)?;
    if let Some(err) = resp.chart.error
        && !err.is_null()
    {
        anyhow::bail!("{}", err.get("description").and_then(|d| d.as_str()).unwrap_or("error"));
    }
    let result = resp
        .chart
        .result
        .and_then(|mut r| r.pop())
        .ok_or_else(|| anyhow::anyhow!("no data for {symbol}"))?;
    let intraday = result
        .indicators
        .and_then(|i| i.quote.into_iter().next())
        .and_then(|q| q.close)
        .map(|c| c.into_iter().flatten().collect())
        .unwrap_or_default();
    Ok(Quote {
        symbol: symbol.to_string(),
        price: result.meta.regular_market_price,
        prev_close: result.meta.chart_previous_close.or(result.meta.previous_close),
        intraday,
        updated: Some(Instant::now()),
        error: None,
    })
}

/// Fetches quotes in a background thread. `quotes()` returns the latest data
/// for the configured symbols, in order; `generation()` changes whenever new
/// data arrives, so callers know when to redraw.
pub struct Ticker {
    shared: Arc<Mutex<Shared>>,
    wake: mpsc::Sender<()>,
}
struct Shared {
    config: TickerConfig,
    quotes: Vec<Quote>,
    generation: u64,
    active: bool,
}
impl Default for Ticker {
    fn default() -> Self {
        Self::new(TickerConfig::default())
    }
}
impl Ticker {
    pub fn new(config: TickerConfig) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            quotes: config
                .symbols
                .iter()
                .map(|s| Quote {
                    symbol: s.clone(),
                    ..Default::default()
                })
                .collect(),
            config,
            generation: 0,
            active: false,
        }));
        let (wake, wake_rx) = mpsc::channel();
        let thread_shared = shared.clone();
        thread::spawn(move || {
            let agent = agent();
            loop {
                let (symbols, refresh, active) = {
                    let s = thread_shared.lock();
                    (s.config.symbols.clone(), s.config.refresh_secs.max(15), s.active)
                };
                if active {
                    for symbol in &symbols {
                        let result = fetch_quote(&agent, symbol);
                        let mut s = thread_shared.lock();
                        // the symbol list may have changed while we were fetching
                        if let Some(q) = s.quotes.iter_mut().find(|q| &q.symbol == symbol) {
                            match result {
                                Ok(new) => *q = new,
                                Err(e) => q.error = Some(e.to_string()),
                            }
                            s.generation += 1;
                        }
                    }
                }
                match wake_rx.recv_timeout(Duration::from_secs(refresh as u64)) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Self { shared, wake }
    }

    /// Only fetch while the ticker is actually being shown.
    pub fn set_active(&self, active: bool) {
        let mut s = self.shared.lock();
        if s.active != active {
            s.active = active;
            drop(s);
            if active {
                _ = self.wake.send(());
            }
        }
    }

    pub fn set_config(&self, config: &TickerConfig) {
        let mut s = self.shared.lock();
        if s.config == *config {
            return;
        }
        let old = std::mem::take(&mut s.quotes);
        s.quotes = config
            .symbols
            .iter()
            .map(|sym| {
                old.iter().find(|q| &q.symbol == sym).cloned().unwrap_or(Quote {
                    symbol: sym.clone(),
                    ..Default::default()
                })
            })
            .collect();
        s.config = config.clone();
        s.generation += 1;
        drop(s);
        _ = self.wake.send(());
    }

    pub fn quotes(&self) -> Vec<Quote> {
        self.shared.lock().quotes.clone()
    }
    pub fn generation(&self) -> u64 {
        self.shared.lock().generation
    }
}

// ---------------------------------------------------------------------------
// Formatting

fn with_commas(int_part: u64) -> String {
    let digits = int_part.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "59,074" / "1,234" / "80.82" / "0.4213"
pub fn format_price(price: f64) -> String {
    let abs = price.abs();
    let sign = if price < 0.0 { "-" } else { "" };
    if abs >= 1000.0 {
        format!("{sign}{}", with_commas(abs.round() as u64))
    } else if abs >= 1.0 {
        format!("{sign}{abs:.2}")
    } else {
        format!("{sign}{abs:.4}")
    }
}

/// "+1.2%" / "-4.6%" / "+12%"
pub fn format_change(pct: f64) -> String {
    let sign = if pct >= 0.0 { "+" } else { "-" };
    let abs = pct.abs();
    if abs >= 100.0 {
        format!("{sign}{abs:.0}%")
    } else {
        format!("{sign}{abs:.1}%")
    }
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

fn set_px(b: &mut Bitmap, x: isize, y: isize) {
    if x >= 0 && y >= 0 && (x as usize) < b.w && (y as usize) < b.h {
        b.data.set(y as usize * b.w + x as usize, true);
    }
}

/// Today's trend as a line inside the box (x, y, w, h), with a dotted line at
/// the previous close.
fn draw_trend(b: &mut Bitmap, q: &Quote, x: isize, y: isize, w: usize, h: usize) {
    let pts = &q.intraday;
    if pts.len() < 2 || w < 2 || h < 2 {
        return;
    }
    let mut lo = pts.iter().cloned().fold(f64::INFINITY, f64::min);
    let mut hi = pts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if let Some(c) = q.prev_close {
        lo = lo.min(c);
        hi = hi.max(c);
    }
    let span = (hi - lo).max(f64::EPSILON);
    let to_y = |v: f64| y + ((hi - v) / span * (h - 1) as f64).round() as isize;
    if let Some(c) = q.prev_close {
        let cy = to_y(c);
        for px in (0..w).step_by(3) {
            set_px(b, x + px as isize, cy);
        }
    }
    let mut prev: Option<isize> = None;
    for px in 0..w {
        // sample the series across the width
        let i = px * (pts.len() - 1) / (w - 1);
        let py = to_y(pts[i]);
        // connect vertically to the previous column so steep moves stay continuous
        let (a, bnd) = match prev {
            Some(p) if p < py => (p + 1, py),
            Some(p) if p > py => (py, p - 1),
            _ => (py, py),
        };
        for yy in a..=bnd {
            set_px(b, x + px as isize, yy);
        }
        prev = Some(py);
    }
}

fn blit(b: &mut Bitmap, src: &Bitmap, x: isize, y: isize) {
    b.blit(src, x, y, false);
}

/// One asset filling the screen: label and change on top, big price, trend line.
pub fn render_cycle(q: &Quote, fonts: &Fonts) -> Bitmap {
    let mut b = Bitmap::new(WIDTH, HEIGHT, false);
    let label = fonts.small.render_line(&q.label());
    blit(&mut b, &label, 0, -2);
    if let Some(pct) = q.change_pct() {
        let ch = fonts.small.render_line(&format_change(pct));
        blit(&mut b, &ch, (WIDTH - ch.w) as isize, -2);
    }
    match q.price {
        Some(price) => {
            let text = format_price(price);
            let mut big = fonts.big.render_line(&text);
            // very large numbers: drop to the small font rather than clip
            if big.w > WIDTH {
                big = fonts.small.render_line(&text);
            }
            let y = if big.h > 20 { 10 } else { 20 };
            blit(&mut b, &big, ((WIDTH - big.w.min(WIDTH)) / 2) as isize, y);
            draw_trend(&mut b, q, 0, 46, WIDTH, 18);
        }
        None => {
            let msg = if q.error.is_some() { "no data" } else { "loading..." };
            let m = fonts.small.render_line(msg);
            blit(&mut b, &m, ((WIDTH - m.w.min(WIDTH)) / 2) as isize, 24);
        }
    }
    b
}

pub const GRID_ROWS: usize = 4;

/// Up to four assets, one per row: label, price, change.
pub fn render_grid(quotes: &[Quote], fonts: &Fonts) -> Bitmap {
    let mut b = Bitmap::new(WIDTH, HEIGHT, false);
    let row_h = HEIGHT / GRID_ROWS;
    for (row, q) in quotes.iter().take(GRID_ROWS).enumerate() {
        let y = (row * row_h) as isize - 2;
        let label = fonts.small.render_line(&q.label());
        blit(&mut b, &label, 0, y);
        let mut right = WIDTH as isize;
        if let Some(pct) = q.change_pct() {
            let ch = fonts.small.render_line(&format_change(pct));
            right -= ch.w as isize;
            blit(&mut b, &ch, right, y);
            right -= 5;
        }
        let price_text = q.price.map(format_price).unwrap_or_else(|| "--".into());
        let price = fonts.small.render_line(&price_text);
        let px = right - price.w as isize;
        if px > label.w as isize + 3 {
            blit(&mut b, &price, px, y);
        }
    }
    b
}

/// One long line ("BTC 59,074 -4.6%   MSTR 80.82 -6.2%") for scrolling.
pub fn render_scroll(quotes: &[Quote], fonts: &Fonts) -> Bitmap {
    fonts.big.render_line(&scroll_text(quotes))
}

/// The text `render_scroll` draws (compare it to avoid restarting the scroll
/// when nothing visible changed).
pub fn scroll_text(quotes: &[Quote]) -> String {
    quotes
        .iter()
        .map(|q| {
            let mut s = q.label();
            s += " ";
            s += &q.price.map(format_price).unwrap_or_else(|| "--".into());
            if let Some(pct) = q.change_pct() {
                s += " ";
                s += &format_change(pct);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("    ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_price(59073.56), "59,074");
        assert_eq!(format_price(1234.4), "1,234");
        assert_eq!(format_price(80.82), "80.82");
        assert_eq!(format_price(0.42131), "0.4213");
        assert_eq!(format_price(1_234_567.0), "1,234,567");
        assert_eq!(format_change(-4.567), "-4.6%");
        assert_eq!(format_change(1.0), "+1.0%");
        assert_eq!(format_change(123.4), "+123%");
    }

    #[test]
    fn labels() {
        let q = |s: &str| Quote {
            symbol: s.into(),
            ..Default::default()
        };
        assert_eq!(q("BTC-USD").label(), "BTC");
        assert_eq!(q("MSTR").label(), "MSTR");
        assert_eq!(q("^GSPC").label(), "GSPC");
    }

    #[test]
    fn renders_without_panicking() {
        let fonts = Fonts::default();
        let q = Quote {
            symbol: "BTC-USD".into(),
            price: Some(59073.56),
            prev_close: Some(61904.76),
            intraday: (0..100).map(|i| 60000.0 + (i as f64 * 0.3).sin() * 1500.0).collect(),
            ..Default::default()
        };
        let empty = Quote::default();
        for quote in [&q, &empty] {
            let b = render_cycle(quote, &fonts);
            assert_eq!((b.w, b.h), (WIDTH, HEIGHT));
        }
        let b = render_grid(&[q.clone(), empty.clone(), q.clone(), q.clone(), q.clone()], &fonts);
        assert_eq!((b.w, b.h), (WIDTH, HEIGHT));
        assert!(render_scroll(&[q, empty], &fonts).w > WIDTH);
    }
}
