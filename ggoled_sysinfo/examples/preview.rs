//! Renders the system info layouts to PNG (4x) and runs the collector briefly.
use ggoled_lib::Bitmap;
use ggoled_sysinfo::*;

fn save(b: &Bitmap, path: &str) {
    const S: u32 = 4;
    let mut img = image::RgbImage::new(b.w as u32 * S, b.h as u32 * S);
    for y in 0..b.h {
        for x in 0..b.w {
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
    let out = std::env::args().nth(1).unwrap();
    let mut s = Snapshot {
        cpu_load: Some(34.0),
        cpu_temp: Some(61.4),
        gpu_load: Some(98.0),
        gpu_temp: Some(72.0),
        gpu_power: Some(245.0),
        vram_used: Some(7.1),
        vram_total: Some(12.0),
        ram_used: Some(18.2),
        ram_total: Some(32.0),
        fps: Some(144.0),
        ..Default::default()
    };
    let fonts = Fonts::default();
    save(&render(&s, &fonts, 4), &format!("{out}/sys_game.png"));
    s.fps = None;
    save(&render(&s, &fonts, 4), &format!("{out}/sys_desktop.png"));

    let info = SysInfo::new(SysInfoConfig::default());
    info.set_active(true);
    std::thread::sleep(std::time::Duration::from_millis(3500));
    println!("{:?}\n{:?}", info.snapshot(), info.sources());
}
