//! Real sampling clock vs nominal, measured against the PC clock: roll stream at 50 ms/div
//! (nominal 500 samples/s) and the 500 ms status period, over `seconds`.
//!   clockcheck [seconds]
use dso510_link::{device, protocol::Message, protocol::Parser, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    for c in ["DEEP 1", "SET SCREEN 2", "SET TMODE 1", "SET TB 18"] {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    let (mut parser, mut out) = (Parser::default(), Vec::new());
    let mut buf = vec![0u8; 65536];
    let t0 = Instant::now();
    let (mut first, mut last): (Option<(Instant, u32)>, Option<(Instant, u32)>) = (None, None);
    let mut restarts = 0;
    let (mut st_first, mut st_last, mut st_n) = (None, None, 0u32);
    while t0.elapsed() < Duration::from_secs(secs + 1) {
        let Ok(n) = port.read(&mut buf) else { continue };
        let now = Instant::now();
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            match m {
                Message::Roll(c) if t0.elapsed() > Duration::from_secs(1) => {
                    if c.flags & 2 != 0 { restarts += 1; first = None; }
                    if first.is_none() { first = Some((now, c.index)); }
                    last = Some((now, c.index + c.samples.len() as u32));
                }
                Message::Status(_) if t0.elapsed() > Duration::from_secs(1) => {
                    if st_first.is_none() { st_first = Some(now); }
                    st_last = Some(now);
                    st_n += 1;
                }
                _ => {}
            }
        }
    }
    if let (Some((ta, ia)), Some((tb, ib))) = (first, last) {
        let wall = (tb - ta).as_secs_f64();
        let rate = (ib - ia) as f64 / wall;
        println!("roll @50 ms/div: {} samples in {wall:.3} s wall = {rate:.2} samples/s (nominal 500.00) -> ratio {:.4} (restarts {restarts})", ib - ia, rate / 500.0);
    }
    if let (Some(a), Some(b)) = (st_first, st_last) {
        if st_n > 1 {
            let period = (b - a).as_secs_f64() / (st_n - 1) as f64;
            println!("status period: {:.2} ms over {} messages (nominal 500 ms = 50 ticks) -> tick = {:.4} ms", period * 1000.0, st_n, period * 1000.0 / 50.0);
        }
    }
}
