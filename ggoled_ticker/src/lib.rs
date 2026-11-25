//! Price ticker for the 128x64 OLED: fetches quotes in the background and renders
//! them in a few layouts.
//!
//! Data comes from Yahoo Finance's public chart endpoint (no API key; works for
//! stocks like "MSTR" and crypto pairs like "BTC-USD"). It's unofficial, so the
//! fetching is kept separate from the rendering in case it needs replacing.

pub mod font5x7;

use ggoled_draw::TextRenderer;
use ggoled_lib::Bitmap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

pub const MIN_REFRESH_SECS: u32 = 5;
/// Data older than this gets a "?" after its label.
const STALE_AFTER: Duration = Duration::from_secs(120);

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

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum GridHeader {
    /// The time (when it's enabled)
    #[default]
    Time,
    /// What's playing, or the time when nothing is
    Media,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TickerConfig {
    /// Yahoo Finance symbols, e.g. "BTC-USD", "MSTR", "^GSPC"
    pub symbols: Vec<String>,
    /// Shown together on one row with only their % change in the list layout
    /// (e.g. indexes); shown like the others in the other layouts.
    pub compact_symbols: Vec<String>,
    /// In the list layout, what the top row shows when there's a spare row.
    pub grid_header: GridHeader,
    /// Display names, e.g. { "^GSPC" = "S&P" }. Defaults: "-USD" and "^" dropped,
    /// common indexes named S&P, DOW, NAS.
    pub labels: BTreeMap<String, String>,
    pub mode: TickerMode,
    /// Seconds per asset (Cycle) or per page (Grid)
    pub cycle_secs: u32,
    /// Seconds between price updates (minimum 5). In the one-at-a-time layout
    /// prices are also refreshed just before each switch.
    pub refresh_secs: u32,
}
impl Default for TickerConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTC-USD".into(), "MSTR".into()],
            compact_symbols: vec!["^GSPC".into(), "^DJI".into(), "^IXIC".into()],
            grid_header: GridHeader::Time,
            labels: BTreeMap::new(),
            mode: TickerMode::Cycle,
            cycle_secs: 6,
            refresh_secs: 10,
        }
    }
}
impl TickerConfig {
    /// Every symbol to fetch: the main list, then the compact ones (no duplicates).
    pub fn all_symbols(&self) -> Vec<String> {
        let mut all = self.symbols.clone();
        for s in &self.compact_symbols {
            if !all.contains(s) {
                all.push(s.clone());
            }
        }
        all
    }
    pub fn name_for(&self, symbol: &str) -> String {
        if let Some(name) = self.labels.get(symbol) {
            return name.clone();
        }
        match symbol {
            "^GSPC" => "S&P".into(),
            "^DJI" => "DOW".into(),
            "^IXIC" => "NAS".into(),
            "^RUT" => "RUT".into(),
            _ => {
                let s = symbol.trim_start_matches('^');
                s.strip_suffix("-USD").unwrap_or(s).to_string()
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Quote {
    pub symbol: String,
    /// Display name (from the config); empty = derived from the symbol
    pub name: String,
    pub price: Option<f64>,
    pub prev_close: Option<f64>,
    /// Today's prices, oldest first (for the trend line)
    pub intraday: Vec<f64>,
    pub updated: Option<Instant>,
    pub error: Option<String>,
    /// Pre-market or after-hours price (stocks outside regular trading hours)
    pub session: Session,
    /// Unix time of the latest data point
    pub last_trade: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Session {
    #[default]
    Regular,
    /// Pre-market; change is measured from the last regular close
    Pre,
    /// After hours; change is from today's close
    Post,
    /// Overnight session (8pm-4am ET); change is from the last close
    Overnight,
}
impl Session {
    /// Short tag shown after the symbol
    pub fn tag(self) -> Option<&'static str> {
        match self {
            Session::Regular => None,
            Session::Pre => Some("PRE"),
            Session::Post => Some("AH"),
            Session::Overnight => Some("OVN"),
        }
    }
}
impl Quote {
    /// Short name for display: "BTC-USD" -> "BTC", "^GSPC" -> "GSPC"
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let s = self.symbol.trim_start_matches('^');
        let s = s.strip_suffix("-USD").unwrap_or(s);
        s.to_string()
    }
    /// Label with "?" appended when the price is old (e.g. connection or rate-limit trouble).
    pub fn display_label(&self) -> String {
        let stale = self.price.is_some() && self.updated.is_none_or(|t| t.elapsed() > STALE_AFTER);
        if stale { self.label() + "?" } else { self.label() }
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
    timestamp: Option<Vec<i64>>,
    indicators: Option<Indicators>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChartMeta {
    regular_market_price: Option<f64>,
    regular_market_time: Option<i64>,
    chart_previous_close: Option<f64>,
    previous_close: Option<f64>,
    current_trading_period: Option<TradingPeriods>,
}
#[derive(Deserialize)]
struct TradingPeriods {
    regular: Option<TradingPeriod>,
}
#[derive(Deserialize)]
struct TradingPeriod {
    start: i64,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SparkEntry {
    timestamp: Option<Vec<i64>>,
    close: Option<Vec<Option<f64>>>,
    chart_previous_close: Option<f64>,
    previous_close: Option<f64>,
}

#[derive(Debug)]
pub struct RateLimited;
impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "rate limited by Yahoo")
    }
}
impl std::error::Error for RateLimited {}

fn get(agent: &ureq::Agent, url: &str) -> anyhow::Result<String> {
    // Yahoo's responses are cacheable for ~45 s; ask for fresh data, and make the
    // URL unique so no cache along the way can answer with an old copy.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let url = format!("{url}&_={nonce}");
    match agent.get(&url).header("Cache-Control", "no-cache").call() {
        Ok(mut resp) => Ok(resp.body_mut().read_to_string()?),
        Err(ureq::Error::StatusCode(429)) => Err(RateLimited.into()),
        Err(e) => Err(e.into()),
    }
}

/// All symbols in one request (Yahoo's "spark" endpoint). Symbols Yahoo doesn't
/// know are simply missing from the result.
pub fn fetch_all(agent: &ureq::Agent, symbols: &[String]) -> anyhow::Result<Vec<Quote>> {
    let mut quotes = vec![];
    for chunk in symbols.chunks(20) {
        let list = chunk.join(",").replace('^', "%5E");
        let url = format!("https://query1.finance.yahoo.com/v8/finance/spark?symbols={list}&range=1d&interval=5m");
        let mut map: std::collections::HashMap<String, SparkEntry> = serde_json::from_str(&get(agent, &url)?)?;
        for symbol in chunk {
            let Some(entry) = map.remove(symbol) else { continue };
            let closes = entry.close.unwrap_or_default();
            let last_trade = entry
                .timestamp
                .unwrap_or_default()
                .iter()
                .zip(&closes)
                .rev()
                .find(|(_, c)| c.is_some())
                .map(|(t, _)| *t);
            let intraday: Vec<f64> = closes.into_iter().flatten().collect();
            quotes.push(Quote {
                symbol: symbol.clone(),
                name: String::new(),
                price: intraday.last().copied(),
                prev_close: entry.chart_previous_close.or(entry.previous_close),
                intraday,
                updated: Some(Instant::now()),
                error: None,
                session: Session::Regular,
                last_trade,
            });
        }
    }
    Ok(quotes)
}

/// Whether a symbol is a stock or ETF that can trade before/after hours (not an
/// index "^...", crypto pair "BTC-USD", or currency/futures "EURUSD=X", "ES=F").
pub fn has_extended_hours(symbol: &str) -> bool {
    !symbol.starts_with('^') && !symbol.contains('-') && !symbol.contains('=')
}

/// Yahoo's quote endpoint needs a session cookie and a "crumb" token, obtained
/// the way a browser does (no account). Kept between requests; refreshed when
/// Yahoo stops accepting it.
pub struct YahooSession {
    agent: ureq::Agent,
    crumb: Option<String>,
}
impl Default for YahooSession {
    fn default() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(15)))
                .user_agent("Mozilla/5.0 (ggoled)")
                .http_status_as_error(false)
                .build()
                .into(),
            crumb: None,
        }
    }
}

/// The newest price for a stock outside regular hours.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedPrice {
    pub price: f64,
    /// Unix time of that price
    pub time: i64,
    /// Last regular-session close, which the change is measured from
    pub reference: f64,
    pub session: Session,
}

impl YahooSession {
    fn crumb(&mut self) -> anyhow::Result<String> {
        if let Some(c) = &self.crumb {
            return Ok(c.clone());
        }
        // sets the session cookie (the response itself is a 404)
        _ = self.agent.get("https://fc.yahoo.com").call();
        let mut resp = self
            .agent
            .get("https://query1.finance.yahoo.com/v1/test/getcrumb")
            .call()?;
        if resp.status() == 429 {
            return Err(RateLimited.into());
        }
        let crumb = resp.body_mut().read_to_string()?.trim().to_string();
        if resp.status() != 200 || crumb.is_empty() || crumb.contains(['{', '<', ' ']) {
            anyhow::bail!("couldn't get a Yahoo session token");
        }
        self.crumb = Some(crumb.clone());
        Ok(crumb)
    }

    /// Pre-market, after-hours or overnight prices for these stocks, whichever is
    /// newest, if newer than the regular session. One request for all of them.
    pub fn extended_prices(
        &mut self,
        symbols: &[String],
    ) -> anyhow::Result<std::collections::HashMap<String, ExtendedPrice>> {
        for attempt in 0..2 {
            let crumb = self.crumb()?;
            // unique URL so no cache answers with an old price (as in `get`)
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let url = format!(
                "https://query1.finance.yahoo.com/v7/finance/quote?symbols={}&crumb={}&fields=regularMarketPrice,regularMarketTime,preMarketPrice,preMarketTime,postMarketPrice,postMarketTime,overnightMarketPrice,overnightMarketTime&overnightPrice=true&_={nonce}",
                symbols.join(","),
                crumb
            );
            let mut resp = self.agent.get(&url).header("Cache-Control", "no-cache").call()?;
            match resp.status().as_u16() {
                200 => {}
                401 | 403 if attempt == 0 => {
                    self.crumb = None; // expired; get a new one and retry
                    continue;
                }
                429 => return Err(RateLimited.into()),
                code => anyhow::bail!("Yahoo quote request failed ({code})"),
            }
            let json: serde_json::Value = serde_json::from_str(&resp.body_mut().read_to_string()?)?;
            let results = json["quoteResponse"]["result"].as_array().cloned().unwrap_or_default();
            let mut out = std::collections::HashMap::new();
            for r in results {
                let Some(symbol) = r["symbol"].as_str() else { continue };
                if let Some(ext) = pick_extended(&r) {
                    out.insert(symbol.to_string(), ext);
                }
            }
            return Ok(out);
        }
        anyhow::bail!("Yahoo didn't accept the session token")
    }
}

/// From a quote result, the newest out-of-hours price (if newer than the
/// regular session's).
fn pick_extended(r: &serde_json::Value) -> Option<ExtendedPrice> {
    let reference = r["regularMarketPrice"].as_f64()?;
    let regular_time = r["regularMarketTime"].as_i64().unwrap_or(0);
    [
        ("preMarket", Session::Pre),
        ("postMarket", Session::Post),
        ("overnightMarket", Session::Overnight),
    ]
    .into_iter()
    .filter_map(|(key, session)| {
        let price = r[format!("{key}Price")].as_f64()?;
        let time = r[format!("{key}Time")].as_i64()?;
        (time > regular_time).then_some((time, price, session))
    })
    .max_by_key(|(time, _, _)| *time)
    .map(|(time, price, session)| ExtendedPrice {
        price,
        time,
        reference,
        session,
    })
}

/// A stock's quote including pre-market and after-hours trading. If the newest
/// trade is outside regular hours, that's the price shown, with the change
/// measured from the last regular-session close.
pub fn fetch_extended(agent: &ureq::Agent, symbol: &str) -> anyhow::Result<Quote> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?range=1d&interval=1m&includePrePost=true",
        symbol.replace('^', "%5E")
    );
    let resp: ChartResponse = serde_json::from_str(&get(agent, &url)?)?;
    let result = resp
        .chart
        .result
        .and_then(|mut r| r.pop())
        .ok_or_else(|| anyhow::anyhow!("no data for {symbol}"))?;
    let closes = result
        .indicators
        .and_then(|i| i.quote.into_iter().next())
        .and_then(|q| q.close)
        .unwrap_or_default();
    let points: Vec<(i64, f64)> = result
        .timestamp
        .unwrap_or_default()
        .into_iter()
        .zip(closes)
        .filter_map(|(t, c)| c.map(|c| (t, c)))
        .collect();
    let meta = &result.meta;
    let mut quote = Quote {
        symbol: symbol.to_string(),
        name: String::new(),
        price: meta.regular_market_price,
        prev_close: meta.chart_previous_close.or(meta.previous_close),
        intraday: points.iter().map(|(_, c)| *c).collect(),
        updated: Some(Instant::now()),
        error: None,
        session: Session::Regular,
        last_trade: points.last().map(|(t, _)| *t).or(meta.regular_market_time),
    };
    if let (Some(&(t, price)), Some(reg_time), Some(reg_price)) =
        (points.last(), meta.regular_market_time, meta.regular_market_price)
        && t > reg_time + 60
    {
        let regular_start = meta
            .current_trading_period
            .as_ref()
            .and_then(|p| p.regular.as_ref())
            .map(|r| r.start);
        quote.session = match regular_start {
            Some(start) if t < start => Session::Pre,
            _ => Session::Post,
        };
        quote.price = Some(price);
        quote.prev_close = Some(reg_price);
    }
    Ok(quote)
}

pub fn fetch_quote(agent: &ureq::Agent, symbol: &str) -> anyhow::Result<Quote> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?range=1d&interval=5m",
        symbol.replace('^', "%5E")
    );
    let body = get(agent, &url)?;
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
        name: String::new(),
        price: result.meta.regular_market_price,
        prev_close: result.meta.chart_previous_close.or(result.meta.previous_close),
        intraday,
        updated: Some(Instant::now()),
        error: None,
        session: Session::Regular,
        last_trade: result.meta.regular_market_time,
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
/// Quote slots for every configured symbol, keeping existing data where possible.
fn build_quotes(config: &TickerConfig, old: &[Quote]) -> Vec<Quote> {
    config
        .all_symbols()
        .into_iter()
        .map(|sym| {
            let name = config.name_for(&sym);
            match old.iter().find(|q| q.symbol == sym) {
                Some(q) => Quote { name, ..q.clone() },
                None => Quote {
                    symbol: sym,
                    name,
                    ..Default::default()
                },
            }
        })
        .collect()
}

impl Ticker {
    pub fn new(config: TickerConfig) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            quotes: build_quotes(&config, &[]),
            config,
            generation: 0,
            active: false,
        }));
        let (wake, wake_rx) = mpsc::channel();
        let thread_shared = shared.clone();
        thread::spawn(move || {
            let agent = agent();
            let mut last_fetch: Option<Instant> = None;
            let mut backoff = Duration::ZERO;
            let mut pending = false;
            let mut yahoo = YahooSession::default();
            let mut ext_prices: std::collections::HashMap<String, ExtendedPrice> = Default::default();
            let mut quote_retry_at: Option<Instant> = None;
            let mut quote_backoff = Duration::ZERO;
            loop {
                let (symbols, refresh, active) = {
                    let s = thread_shared.lock();
                    (
                        s.config.all_symbols(),
                        s.config.refresh_secs.max(MIN_REFRESH_SECS),
                        s.active,
                    )
                };
                let min_gap = Duration::from_secs(MIN_REFRESH_SECS as u64).max(backoff);
                let due = last_fetch.is_none_or(|t| t.elapsed() >= min_gap);
                if active && due && !symbols.is_empty() {
                    last_fetch = Some(Instant::now());
                    pending = false;
                    // one request for everything; per-symbol requests only as a fallback
                    let mut result = match fetch_all(&agent, &symbols) {
                        Err(e) if e.is::<RateLimited>() => Err(e),
                        Err(_) => Ok(symbols.iter().filter_map(|s| fetch_quote(&agent, s).ok()).collect()),
                        ok => ok,
                    };
                    // Stocks whose regular-session data has gone quiet (market closed):
                    // get pre-market/after-hours prices, at most once a minute each
                    if let Ok(quotes) = &mut result {
                        let now_unix = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        let quiet: Vec<String> = quotes
                            .iter()
                            .filter(|q| {
                                has_extended_hours(&q.symbol) && q.last_trade.is_none_or(|t| now_unix - t > 7 * 60)
                            })
                            .map(|q| q.symbol.clone())
                            .collect();
                        // Pre-market / after hours: the chart data has the latest trade,
                        // live, without a session token (one request per stock).
                        let mut need_quote = vec![];
                        for sym in &quiet {
                            let recent = |q: &Quote| q.last_trade.is_some_and(|t| now_unix - t < 15 * 60);
                            match fetch_extended(&agent, sym) {
                                Ok(q) if q.session != Session::Regular && recent(&q) => {
                                    if let (Some(price), Some(reference)) = (q.price, q.prev_close) {
                                        ext_prices.insert(
                                            sym.clone(),
                                            ExtendedPrice {
                                                price,
                                                time: q.last_trade.unwrap_or(0),
                                                reference,
                                                session: q.session,
                                            },
                                        );
                                    }
                                }
                                // nothing recent: overnight session (or the chart failed)
                                _ => need_quote.push(sym.clone()),
                            }
                        }
                        // Overnight: only the quote endpoint has it. It needs a session
                        // token; if Yahoo refuses, keep the last prices and wait before
                        // asking again (5 min, doubling up to 30) rather than every refresh.
                        let waiting = quote_retry_at.is_some_and(|t: Instant| Instant::now() < t);
                        if !need_quote.is_empty() && !waiting {
                            match yahoo.extended_prices(&need_quote) {
                                Ok(prices) => {
                                    quote_backoff = Duration::ZERO;
                                    ext_prices.extend(prices);
                                }
                                Err(_) => {
                                    quote_backoff =
                                        (quote_backoff * 2).clamp(Duration::from_secs(300), Duration::from_secs(1800));
                                    quote_retry_at = Some(Instant::now() + quote_backoff);
                                }
                            }
                        }
                        if quiet.is_empty() {
                            ext_prices.clear();
                        }
                        for q in quotes.iter_mut() {
                            if quiet.contains(&q.symbol)
                                && let Some(ext) = ext_prices.get(&q.symbol)
                            {
                                q.price = Some(ext.price);
                                q.prev_close = Some(ext.reference);
                                q.session = ext.session;
                            }
                        }
                    }
                    let mut s = thread_shared.lock();
                    match result {
                        Ok(new) => {
                            backoff = Duration::ZERO;
                            for q in s.quotes.iter_mut() {
                                match new.iter().find(|n| n.symbol == q.symbol) {
                                    Some(n) => {
                                        let name = std::mem::take(&mut q.name);
                                        *q = Quote { name, ..n.clone() };
                                    }
                                    None => q.error = Some("no data".into()),
                                }
                            }
                        }
                        Err(e) => {
                            if e.is::<RateLimited>() {
                                backoff = (backoff * 2).clamp(Duration::from_secs(60), Duration::from_secs(600));
                            }
                            for q in s.quotes.iter_mut() {
                                q.error = Some(e.to_string());
                            }
                        }
                    }
                    s.generation += 1;
                }
                // Sleep until the next refresh, or until woken (refresh request, config
                // change, ticker shown). A request that arrives too soon after the last
                // fetch is kept and served as soon as the minimum gap has passed.
                let interval = if pending {
                    min_gap
                } else {
                    Duration::from_secs(refresh as u64).max(backoff)
                };
                let wait = match last_fetch {
                    Some(t) => interval.saturating_sub(t.elapsed()).max(Duration::from_millis(200)),
                    None => interval,
                };
                match wake_rx.recv_timeout(wait) {
                    Ok(()) => pending = true,
                    Err(RecvTimeoutError::Timeout) => {}
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
        s.quotes = build_quotes(config, &old);
        s.config = config.clone();
        s.generation += 1;
        drop(s);
        _ = self.wake.send(());
    }

    /// Fetch now if allowed (at most every MIN_REFRESH_SECS, longer if Yahoo is
    /// rate limiting).
    pub fn request_refresh(&self) {
        _ = self.wake.send(());
    }

    /// All quotes: the main symbols, then the compact ones.
    pub fn quotes(&self) -> Vec<Quote> {
        self.shared.lock().quotes.clone()
    }
    /// (main symbols, compact symbols), for the list layout.
    pub fn split_quotes(&self) -> (Vec<Quote>, Vec<Quote>) {
        let s = self.shared.lock();
        let find = |sym: &String| s.quotes.iter().find(|q| &q.symbol == sym).cloned();
        (
            s.config.symbols.iter().filter_map(find).collect(),
            s.config.compact_symbols.iter().filter_map(find).collect(),
        )
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
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
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

pub fn blit(b: &mut Bitmap, src: &Bitmap, x: isize, y: isize) {
    b.blit(src, x, y, false);
}

/// One asset filling the screen: label and change on top, big price, trend line.
/// `margin` keeps content that many pixels from the left/right edges (for the
/// OLED shift, which moves the whole image).
pub fn render_cycle(q: &Quote, fonts: &Fonts, margin: usize) -> Bitmap {
    let mut b = Bitmap::new(WIDTH, HEIGHT, false);
    let (left, right) = (margin as isize, (WIDTH - margin) as isize);
    let inner = WIDTH - 2 * margin;
    let top = margin as isize - SMALL_INK_TOP;
    let label = fonts.small.render_line(&q.display_label());
    blit(&mut b, &label, left, top);
    draw_session_tag(&mut b, q, left + label.w as isize + 3, margin as isize + 2);
    if let Some(pct) = q.change_pct() {
        let ch = fonts.small.render_line(&format_change(pct));
        blit(&mut b, &ch, right - ch.w as isize, top);
    }
    const TREND_H: usize = 14;
    let trend_y = (HEIGHT - margin - TREND_H) as isize;
    // the price goes in the space between the top row and the trend line
    let (area_top, area_bottom) = (margin as isize + SMALL_INK_H + 1, trend_y - 1);
    match q.price {
        Some(price) => {
            let text = format_price(price);
            let mut big = fonts.big.render_line(&text);
            // very large numbers: drop to the small font rather than clip
            if big.w > inner {
                big = fonts.small.render_line(&text);
            }
            let (ink_top, ink_h) = if big.h > 20 {
                (BIG_INK_TOP, BIG_INK_H)
            } else {
                (SMALL_INK_TOP, SMALL_INK_H)
            };
            let y = area_top + (area_bottom - area_top - ink_h) / 2 - ink_top;
            blit(&mut b, &big, ((WIDTH - big.w.min(WIDTH)) / 2) as isize, y);
            draw_trend(&mut b, q, left, trend_y, inner, TREND_H);
        }
        None => {
            let msg = if q.error.is_some() { "no data" } else { "loading..." };
            let m = fonts.small.render_line(msg);
            blit(&mut b, &m, ((WIDTH - m.w.min(WIDTH)) / 2) as isize, 24);
        }
    }
    b
}

/// Small "AH"/"PRE" tag in the 5x7 font at (x, y); returns the width used
/// (including a 2px lead-in gap), 0 if there's no tag.
fn draw_session_tag(b: &mut Bitmap, q: &Quote, x: isize, y: isize) -> isize {
    match q.session.tag() {
        Some(tag) => {
            let t = font5x7::render(tag);
            blit(b, &t, x, y);
            t.w as isize + 2
        }
        None => 0,
    }
}

pub const GRID_ROWS: usize = 4;

// Where the ink sits inside a rendered line of the pixel font (measured):
// 16px font: rows 4..=14 (incl. commas), 32px: rows 8..=29.
pub const SMALL_INK_TOP: isize = 4;
pub const SMALL_INK_H: isize = 11;
pub const BIG_INK_TOP: isize = 8;
pub const BIG_INK_H: isize = 22;
/// The compact row: two lines of the 5x7 font with a 1px gap.
const COMPACT_H: isize = 2 * font5x7::GLYPH_H as isize + 1;

/// Spread blocks of the given heights evenly over the screen height inside the
/// margin; returns each block's top y.
pub fn spread(heights: &[isize], margin: usize) -> Vec<isize> {
    let avail = (HEIGHT - 2 * margin) as isize;
    let total: isize = heights.iter().sum();
    let gaps = heights.len().saturating_sub(1) as isize;
    let free = (avail - total).max(0);
    let mut y = margin as isize;
    heights
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let top = y;
            // distribute the free space as evenly as whole pixels allow
            let gap = if (i as isize) < gaps {
                free * (i as isize + 1) / gaps - free * i as isize / gaps
            } else {
                0
            };
            y += h + gap;
            top
        })
        .collect()
}

/// Row tops for the list layout (header first if any, then assets, then compact).
fn grid_row_tops(header: bool, main_rows: usize, compact: bool, margin: usize) -> Vec<isize> {
    let mut heights = vec![SMALL_INK_H; header as usize + main_rows];
    if compact {
        heights.push(COMPACT_H);
    }
    spread(&heights, margin)
}

/// The y to draw a 16px line at for the list layout's header row (e.g. for a
/// scrolling header drawn separately).
pub fn grid_header_y(main_count: usize, has_compact: bool, margin: usize) -> isize {
    let main_rows = main_count.min(GRID_ROWS - 1 - has_compact as usize);
    grid_row_tops(true, main_rows, has_compact, margin)[0] - SMALL_INK_TOP
}

/// The list layout's top row.
pub enum Header<'a> {
    None,
    /// Centered text in the 16px font
    Text(&'a str),
    /// Leave the row empty (the caller draws something there, e.g. scrolling text)
    Reserved,
}

/// How the list layout splits into pages: (rows for main symbols per page, pages).
pub fn grid_pages(main_count: usize, has_compact: bool) -> (usize, usize) {
    let rows = GRID_ROWS - has_compact as usize;
    (rows, main_count.div_ceil(rows).max(1))
}

/// Whether the list layout has a spare row for a header.
pub fn grid_has_header_room(main_count: usize, has_compact: bool) -> bool {
    main_count + (has_compact as usize) < GRID_ROWS
}

/// Assets one per row (label, price, change), then the compact symbols sharing
/// one row (name above, % change below), with an optional header row on top.
pub fn render_grid(main: &[Quote], compact: &[Quote], header: Header, fonts: &Fonts, margin: usize) -> Bitmap {
    let mut b = Bitmap::new(WIDTH, HEIGHT, false);
    let (left, right) = (margin as isize, (WIDTH - margin) as isize);
    let has_header = !matches!(header, Header::None);
    let has_compact = !compact.is_empty();
    let main_rows = main.len().min(GRID_ROWS - has_header as usize - has_compact as usize);
    let tops = grid_row_tops(has_header, main_rows, has_compact, margin);
    let mut rows = tops.iter();
    if has_header {
        let y = rows.next().unwrap() - SMALL_INK_TOP;
        if let Header::Text(text) = header {
            let t = fonts.small.render_line(text);
            let x = if t.w > WIDTH - 2 * margin {
                left
            } else {
                ((WIDTH - t.w) / 2) as isize
            };
            blit(&mut b, &t, x, y);
        }
    }
    for q in main.iter().take(main_rows) {
        let ink_top = *rows.next().unwrap();
        let y = ink_top - SMALL_INK_TOP;
        // right side: change, then price to its left
        let mut x_right = right;
        if let Some(pct) = q.change_pct() {
            let ch = fonts.small.render_line(&format_change(pct));
            x_right -= ch.w as isize;
            blit(&mut b, &ch, x_right, y);
            x_right -= 4;
        }
        let price_text = q.price.map(format_price).unwrap_or_else(|| "--".into());
        let price = fonts.small.render_line(&price_text);
        let px = x_right - price.w as isize;
        // left side: symbol, plus a PRE/AH/OVN tag after it if there's room,
        // otherwise symbol and tag stacked in the small font
        let label = fonts.small.render_line(&q.display_label());
        let tag_w = q.session.tag().map_or(0, |t| font5x7::text_width(t) as isize + 3);
        let label_end = if left + label.w as isize + tag_w + 2 < px || q.session.tag().is_none() {
            blit(&mut b, &label, left, y);
            draw_session_tag(&mut b, q, left + label.w as isize + 3, ink_top + 2);
            left + label.w as isize + tag_w
        } else {
            let name = font5x7::render(&q.display_label());
            let tag = font5x7::render(q.session.tag().unwrap_or(""));
            let top = ink_top + SMALL_INK_H / 2 - font5x7::GLYPH_H as isize; // centered on the row
            blit(&mut b, &name, left, top);
            blit(&mut b, &tag, left, top + font5x7::GLYPH_H as isize + 1);
            left + name.w.max(tag.w) as isize
        };
        if px > label_end + 2 {
            blit(&mut b, &price, px, y);
        }
    }
    if let Some(&y) = rows.next() {
        let cols = compact.len().min(4);
        let col_w = (WIDTH - 2 * margin) / cols;
        for (i, q) in compact.iter().take(cols).enumerate() {
            let col_x = left + (i * col_w) as isize;
            let center = |w: usize| col_x + (col_w as isize - w as isize) / 2;
            let name = font5x7::render(&q.display_label());
            blit(&mut b, &name, center(name.w), y);
            let value = q.change_pct().map(format_change).unwrap_or_else(|| "--".into());
            let v = font5x7::render(&value);
            blit(&mut b, &v, center(v.w), y + font5x7::GLYPH_H as isize + 1);
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
            let mut s = q.display_label();
            if let Some(tag) = q.session.tag() {
                s += " ";
                s += tag;
            }
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
            let b = render_cycle(quote, &fonts, 4);
            assert_eq!((b.w, b.h), (WIDTH, HEIGHT));
        }
        let many = [q.clone(), empty.clone(), q.clone(), q.clone(), q.clone()];
        let b = render_grid(&many, &many[..3], Header::Text("12:00:00"), &fonts, 4);
        assert_eq!((b.w, b.h), (WIDTH, HEIGHT));
        assert!(render_scroll(&[q, empty], &fonts).w > WIDTH);
    }

    fn ink_bounds(b: &Bitmap) -> Option<(usize, usize, usize, usize)> {
        let mut r: Option<(usize, usize, usize, usize)> = None;
        for y in 0..b.h {
            for x in 0..b.w {
                if b.data[y * b.w + x] {
                    let (x0, y0, x1, y1) = r.unwrap_or((x, y, x, y));
                    r = Some((x0.min(x), y0.min(y), x1.max(x), y1.max(y)));
                }
            }
        }
        r
    }

    #[test]
    fn stays_inside_margins() {
        let fonts = Fonts::default();
        let q = |sym: &str, name: &str, price: f64| Quote {
            symbol: sym.into(),
            name: name.into(),
            price: Some(price),
            prev_close: Some(price * 1.05),
            intraday: (0..100)
                .map(|i| price + (i as f64 * 0.3).sin() * price * 0.03)
                .collect(),
            ..Default::default()
        };
        let mut mstr = q("MSTR", "MSTR", 1081.01);
        mstr.session = Session::Pre; // widest tag
        let main = [q("BTC-USD", "BTC", 159_033.5), mstr];
        let compact = [
            q("^GSPC", "S&P", 6000.0),
            q("^DJI", "DOW", 44000.0),
            q("^IXIC", "NAS", 19000.0),
        ];
        for margin in [0, 1, 4] {
            let grid = render_grid(&main, &compact, Header::Text("12:55:03 PM"), &fonts, margin);
            let cycle = render_cycle(&main[0], &fonts, margin);
            let cycle_tagged = render_cycle(&main[1], &fonts, margin);
            for b in [grid, cycle, cycle_tagged] {
                let (x0, y0, x1, y1) = ink_bounds(&b).unwrap();
                assert!(x0 >= margin && y0 >= margin, "margin {margin}: {x0},{y0}");
                assert!(
                    x1 < WIDTH - margin && y1 < HEIGHT - margin,
                    "margin {margin}: {x1},{y1}"
                );
            }
        }
    }

    #[test]
    fn picks_newest_extended_price() {
        let r: serde_json::Value = serde_json::from_str(
            r#"{"symbol":"MSTR","regularMarketPrice":77.55,"regularMarketTime":1000,
                "postMarketPrice":77.33,"postMarketTime":2000,
                "overnightMarketPrice":76.54,"overnightMarketTime":3000}"#,
        )
        .unwrap();
        assert_eq!(
            pick_extended(&r),
            Some(ExtendedPrice {
                price: 76.54,
                time: 3000,
                reference: 77.55,
                session: Session::Overnight
            })
        );
        // during regular hours nothing newer exists
        let r: serde_json::Value = serde_json::from_str(
            r#"{"regularMarketPrice":80.0,"regularMarketTime":5000,"preMarketPrice":79.0,"preMarketTime":4000}"#,
        )
        .unwrap();
        assert_eq!(pick_extended(&r), None);
    }
}
