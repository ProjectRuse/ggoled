use ggoled_draw::TextRenderer;
fn main() {
    let out = std::env::args().nth(1).unwrap();
    let lines = [
        (16.0, "S&P -1.5% DOW -0.9%"),
        (16.0, "-1.5% -0.9% -2.1%"),
        (12.0, "S&P -1.5 DOW -0.9 NAS -2.1"),
        (8.0, "S&P -1.5% DOW -0.9% NAS -2.1%"),
    ];
    let mut img = image::RgbImage::new(128 * 4, 64 * 4);
    let mut y0 = 0;
    for (size, text) in lines {
        let t = TextRenderer::new_pixel_operator_sized(size);
        let b = t.render_line(text);
        println!("size {size}: w {} h {} for {text:?}", b.w, b.h);
        for y in 0..b.h {
            for x in 0..b.w.min(128) {
                if b.data[y * b.w + x] {
                    for dy in 0..4 {
                        for dx in 0..4 {
                            if (y0 + y) * 4 + dy < 256 {
                                img.put_pixel(
                                    (x * 4 + dx) as u32,
                                    ((y0 + y) * 4 + dy) as u32,
                                    image::Rgb([235, 235, 235]),
                                );
                            }
                        }
                    }
                }
            }
        }
        y0 += b.h;
    }
    img.save(format!("{out}/fonts.png")).unwrap();
}
