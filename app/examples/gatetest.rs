//! Noise gate check: freq_raw with the signal at normal and at huge V/div (signal < 1/3 div).
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};
fn main() {
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let (mut parser, mut out) = (Parser::default(), Vec::new());
    let mut buf = vec![0u8; 65536];
    for (label, cmds) in [("normal", vec!["DEEP 0", "SET SCREEN 2", "SET TMODE 1", "SET TB 13", "SET VDIV 3"]), ("tiny (10 V/div)", vec!["SET VDIV 9"]), ("back", vec!["SET VDIV 3"])] {
        for c in cmds { port.write_all(format!("{c}\n").as_bytes()).unwrap(); std::thread::sleep(Duration::from_millis(300)); }
        let t0 = Instant::now();
        let (mut vals, mut swing) = (Vec::new(), (255u8, 0u8));
        while t0.elapsed() < Duration::from_secs(4) {
            let Ok(n) = port.read(&mut buf) else { continue };
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) {
                if t0.elapsed() < Duration::from_millis(1200) { continue; }
                match m {
                    Message::Status(s) => vals.push(s.freq_raw),
                    Message::Frame(f) => for &v in &f.trace { swing = (swing.0.min(v), swing.1.max(v)); },
                    _ => {}
                }
            }
        }
        println!("{label:16} trace swing {:3} codes, freq_raw {:?}", swing.1 as i32 - swing.0 as i32, vals);
    }
}
