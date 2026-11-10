//! Slow time bases (roll mode): prints what the scope sends as screen frames.
//!   rolltest <timebase index> [seconds] [commands...]
use dso510_link::{deep::RollStream, device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tb: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(18);
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(6);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    for c in ["DEEP 1".to_string(), "SET SCREEN 2".to_string(), "SET TMODE 1".to_string(), format!("SET TB {tb}")].into_iter().chain(args.iter().skip(3).cloned()) {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    let (mut parser, mut out) = (Parser::default(), Vec::new());
    let mut buf = vec![0u8; 65536];
    let t0 = Instant::now();
    let mut last_print = Duration::ZERO;
    let mut frames = 0;
    let mut roll = RollStream::default();
    let (mut chunks, mut records) = (0u32, 0u32);
    while t0.elapsed() < Duration::from_secs(secs) {
        let Ok(n) = port.read(&mut buf) else { continue };
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            match m {
                Message::Frame(f) => {
                    frames += 1;
                    if t0.elapsed() - last_print >= Duration::from_millis(500) {
                        last_print = t0.elapsed();
                        let valid: Vec<u8> = f.trace.iter().enumerate().filter(|(i, _)| f.valid(*i)).map(|(_, &v)| v).collect();
                        let (mn, mx) = (valid.iter().min().copied().unwrap_or(0), valid.iter().max().copied().unwrap_or(0));
                        println!(
                            "{:5.2}s tb {} run {} tmode {} start {:3} end {:3} valid {:3} min {:3} max {:3} first {:?} last {:?} frames {frames}",
                            t0.elapsed().as_secs_f64(), f.timebase, f.running, f.trig_mode, f.trace_start, f.trace_end, valid.len(), mn, mx,
                            valid.first(), valid.last()
                        );
                    }
                }
                Message::Roll(c) => {
                    chunks += 1;
                    records += roll.ingest(c) as u32;
                }
                Message::Status(s) if t0.elapsed() < Duration::from_millis(1500) => println!("status: tb {} running {} tmode {} app {}", s.timebase, s.running, s.trig_mode, s.app_mode),
                _ => {}
            }
        }
    }
    println!("frames in {secs} s: {frames}");
    match roll.capture() {
        Some(c) => {
            let (mn, mx) = (c.codes.iter().min().copied().unwrap_or(0), c.codes.iter().max().copied().unwrap_or(0));
            println!(
                "roll stream: {chunks} chunks, {records} record(s), {} samples = {:.2} s of signal in {secs} s (expected {:.0} samples/s), min {mn} max {mx}, LOST {}",
                c.len(), c.t_last(), 1.0 / c.dt, roll.lost
            );
        }
        None => println!("roll stream: nothing received ({chunks} chunks)"),
    }
}
