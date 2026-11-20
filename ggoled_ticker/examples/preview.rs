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
    let quotes: Vec<Quote> = args[2..]
        .iter()
        .map(|s| {
            fetch_quote(&agent, s).unwrap_or_else(|e| Quote {
                symbol: s.clone(),
                error: Some(e.to_string()),
                ..Default::default()
            })
        })
        .collect();
    for q in &quotes {
        println!(
            "{}: {:?} prev {:?} pts {} err {:?}",
            q.symbol,
            q.price,
            q.prev_close,
            q.intraday.len(),
            q.error
        );
    }
    let fonts = Fonts::default();
    for q in &quotes {
        save(&render_cycle(q, &fonts), &format!("{out}/cycle_{}.png", q.label()), 128);
    }
    save(&render_grid(&quotes, &fonts), &format!("{out}/grid.png"), 128);
    let s = render_scroll(&quotes, &fonts);
    save(&s, &format!("{out}/scroll_full.png"), s.w);
}
