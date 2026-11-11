//! Start-up picture over USB (firmware v4.20): upload a picture, or a generated test picture, or delete it.
//!   logoup <picture file> [crop|bars]
//!   logoup --test
//!   logoup --delete
use dso510_link::{device, logo, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let up = match args.first().map(String::as_str) {
        Some("--delete") => logo::Upload::delete(),
        Some("--test") | None => {
            // colour bars, a gradient and a frame: shows cropping, colours and the fade
            let img = image::RgbImage::from_fn(logo::W, logo::H, |x, y| {
                let edge = x < 3 || y < 3 || x >= logo::W - 3 || y >= logo::H - 3;
                if edge {
                    return image::Rgb([255, 255, 255]);
                }
                if y < 160 {
                    let bars = [[255, 0, 0], [255, 160, 0], [255, 255, 0], [0, 200, 0], [0, 120, 255], [160, 0, 255], [255, 255, 255], [40, 40, 40]];
                    image::Rgb(bars[(x * 8 / logo::W) as usize])
                } else {
                    let v = (x * 255 / logo::W) as u8;
                    image::Rgb([v, v, v])
                }
            });
            let p = logo::prepare(&image::DynamicImage::ImageRgb8(img), logo::Fit::Crop).unwrap();
            println!("test picture: {} bytes JPEG", p.jpeg.len());
            logo::Upload::new(p.jpeg)
        }
        Some(path) => {
            let fit = if args.get(1).map(String::as_str) == Some("bars") { logo::Fit::Bars } else { logo::Fit::Crop };
            let p = logo::prepare(&logo::load(std::path::Path::new(path)).unwrap(), fit).unwrap();
            println!("{path}: {} bytes JPEG (quality {})", p.jpeg.len(), p.quality);
            logo::Upload::new(p.jpeg)
        }
    };
    let mut up = up;
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    let (mut parser, mut out, mut buf) = (Parser::default(), Vec::new(), vec![0u8; 65536]);
    port.write_all(b"DEEP 0\n").unwrap();
    let t0 = Instant::now();
    let mut last_print = Instant::now();
    while !up.finished() && t0.elapsed() < Duration::from_secs(120) {
        for c in up.step() {
            port.write_all(format!("{c}\n").as_bytes()).unwrap();
        }
        if let Ok(n) = port.read(&mut buf) {
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) {
                if let Message::Text(t) = m {
                    if !t.starts_with("LOGOD ") && !t.starts_with("HB ") {
                        println!("< {t}");
                    }
                    up.reply(&t);
                }
            }
        }
        if last_print.elapsed() > Duration::from_millis(500) && !up.is_delete() {
            last_print = Instant::now();
            let rate = up.acked as f64 / t0.elapsed().as_secs_f64();
            let left = (up.total() - up.acked) as f64 / rate.max(1.0);
            println!("{} / {} bytes, {:.1} KB/s, ~{:.0} s left", up.acked, up.total(), rate / 1024.0, left);
        }
    }
    println!("{:?} after {:.1} s", up.state, t0.elapsed().as_secs_f64());
}
