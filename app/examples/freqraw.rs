//! What the firmware puts into freq_counter_raw at a few time bases (hardware counter vs screen measurement).
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};
fn main() {
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    for c in ["DEEP 0", "SET SCREEN 2", "SET TMODE 1"] { port.write_all(format!("{c}\n").as_bytes()).unwrap(); std::thread::sleep(Duration::from_millis(300)); }
    let (mut parser, mut out) = (Parser::default(), Vec::new());
    let mut buf = vec![0u8; 65536];
    for tb in [4u8, 7, 11, 13, 16] {
        port.write_all(format!("SET TB {tb}\n").as_bytes()).unwrap();
        let t0 = Instant::now();
        let mut vals = Vec::new();
        while t0.elapsed() < Duration::from_secs(3) {
            let Ok(n) = port.read(&mut buf) else { continue };
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) {
                if let Message::Status(s) = m { if t0.elapsed() > Duration::from_millis(800) { vals.push(s.freq_raw); } }
            }
        }
        vals.sort(); vals.dedup();
        println!("tb {tb:2}: freq_raw values {vals:?}");
    }
}
