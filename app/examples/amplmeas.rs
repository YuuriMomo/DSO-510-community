//! Square-wave amplitude from deep records: top and base levels (histogram modes of the upper and lower
//! half), against the generator setting.
//!   amplmeas [records]
use dso510_link::{deep::Assembler, device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let want: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    for c in ["GET", "DEEP 1"] {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    let (mut parser, mut asm, mut out, mut buf) = (Parser::default(), Assembler::default(), Vec::new(), vec![0u8; 65536]);
    let t0 = Instant::now();
    let mut got = 0;
    while got < want && t0.elapsed() < Duration::from_secs(30) {
        let Ok(n) = port.read(&mut buf) else { continue };
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            match m {
                Message::Status(s) => println!("generator: wave {} freq {} Hz duty {} % ampl setting {} out {}", s.gen_wave, s.gen_freq, s.gen_duty, s.gen_ampl, s.gen_out),
                Message::Deep(d) => asm.header(d),
                Message::Raw { offset, data } => {
                    if let Some(c) = asm.chunk(offset, &data) {
                        got += 1;
                        let (lo, hi) = (*c.codes.iter().min().unwrap(), *c.codes.iter().max().unwrap());
                        let mid = (lo as u32 + hi as u32) / 2;
                        let mut hist = [0u32; 256];
                        for &v in &c.codes { hist[v as usize] += 1; }
                        // mean of each half: robust levels of a square wave
                        let level = |r: std::ops::RangeInclusive<usize>| {
                            let (n, s) = r.fold((0u64, 0u64), |(n, s), v| (n + hist[v] as u64, s + hist[v] as u64 * v as u64));
                            s as f64 / n.max(1) as f64
                        };
                        let (base, top) = (level(lo as usize..=mid as usize), level(mid as usize + 1..=hi as usize));
                        println!("record {got}: {} V/div zoom {} | top {:.3} V base {:.3} V -> amplitude {:.3} V | peak-to-peak {:.3} V",
                                 c.volts_per_div(), c.info.zoom, c.volts(top.round() as u8), c.volts(base.round() as u8),
                                 (top - base) / 25.0 * c.volts_per_div(), c.volts(hi) - c.volts(lo));
                    }
                }
                _ => {}
            }
        }
    }
    port.write_all(b"DEEP 0\n").unwrap();
}
