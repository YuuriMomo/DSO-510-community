//! Screen frames as text: each of the 301 trace points as one level character, to see what the scope draws.
//!   framedump [frames] [commands...]
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let want: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    for c in args.iter().skip(2) {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(400));
    }
    let (mut parser, mut out, mut buf) = (Parser::default(), Vec::new(), vec![0u8; 65536]);
    let t0 = Instant::now();
    let mut got = 0;
    let mut last = Instant::now();
    while got < want && t0.elapsed() < Duration::from_secs(30) {
        let Ok(n) = port.read(&mut buf) else { continue };
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            if let Message::Frame(f) = m {
                got += 1;
                // 301 points in 101 columns (every 3rd), level 0..9 over the trace's code range
                let line: String = f.trace.iter().step_by(3).map(|&v| char::from(b'0' + (v as u32 * 10 / 256) as u8)).collect();
                let (lo, hi) = (f.trace.iter().min().copied().unwrap_or(0), f.trace.iter().max().copied().unwrap_or(0));
                println!("{:5} ms  trig {:?} mode {} [{lo:3}..{hi:3}] {line}", last.elapsed().as_millis(), f.triggered(), f.trig_mode);
                last = Instant::now();
            }
        }
    }
}
