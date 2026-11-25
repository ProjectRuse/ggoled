//! Renders each ticker layout with live data to PNGs (4x scale) for checking.
//! cargo run -p ggoled_ticker --example preview -- OUT_DIR SYMBOL...
use ggoled_lib::Bitmap;
use ggoled_ticker::*;

fn save(b: &Bitmap, path: &str, w: usize) {
    const S: u32 = 4;
    let mut img = image::RgbImage::new(w as u32 * S, b.h as u32 * S);
    for y in 0..b.h {
        for x in 0..w.min(b.w) {
            if b.data[y * b.w + x] {
                for dy in 0..S {
                    for dx in 0..S {
                        img.put_pixel(x as u32 * S + dx, y as u32 * S + dy, image::Rgb([235, 235, 235]));
                    }
                }
            }
        }
    }
    img.save(path).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = &args[1];
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent("Mozilla/5.0 (ggoled)")
        .build()
        .into();
    let cfg = TickerConfig {
        symbols: args[2..].to_vec(),
        ..Default::default()
    };
    let mut fetched = fetch_all(&agent, &cfg.all_symbols()).unwrap();
    let stocks: Vec<String> = fetched
        .iter()
        .map(|q| q.symbol.clone())
        .filter(|s| has_extended_hours(s))
        .collect();
    if let Ok(prices) = YahooSession::default().extended_prices(&stocks) {
        for q in fetched.iter_mut() {
            if let Some(ext) = prices.get(&q.symbol) {
                println!("{}: {:?}", q.symbol, ext);
                q.price = Some(ext.price);
                q.prev_close = Some(ext.reference);
                q.session = ext.session;
            }
        }
    }
    let get = |sym: &String| {
        let mut q = fetched.iter().find(|q| &q.symbol == sym).cloned().unwrap_or_default();
        q.name = cfg.name_for(sym);
        q
    };
    let main: Vec<Quote> = cfg.symbols.iter().map(get).collect();
    let compact: Vec<Quote> = cfg.compact_symbols.iter().map(get).collect();
    let fonts = Fonts::default();
    for margin in [0, 4] {
        for q in &main {
            save(
                &render_cycle(q, &fonts, margin),
                &format!("{out}/cycle_{}_m{margin}.png", q.label()),
                128,
            );
        }
        save(
            &render_grid(&main, &compact, Header::Text("12:55:03"), &fonts, margin),
            &format!("{out}/grid_m{margin}.png"),
            128,
        );
    }
    let all: Vec<Quote> = main.iter().chain(&compact).cloned().collect();
    let s = render_scroll(&all, &fonts);
    save(&s, &format!("{out}/scroll_full.png"), s.w);
}
