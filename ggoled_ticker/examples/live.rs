//! Runs the background fetcher for a while and prints what it gets.
use ggoled_ticker::*;
use std::time::Duration;
fn main() {
    let t = Ticker::new(TickerConfig::default());
    t.set_active(true);
    let mut last = 0;
    for i in 0..26 {
        std::thread::sleep(Duration::from_secs(1));
        if i == 12 {
            t.request_refresh(); // within 5 s of a fetch? then it waits
        }
        let g = t.generation();
        if g != last {
            last = g;
            let q = t.quotes();
            println!(
                "t={:>2}s gen {g}: {}",
                i + 1,
                q.iter()
                    .map(|q| format!(
                        "{} {:?} {:?} {:?} err {:?}",
                        q.display_label(),
                        q.session,
                        q.price,
                        q.change_pct().map(format_change),
                        q.error
                    ))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
    }
}
