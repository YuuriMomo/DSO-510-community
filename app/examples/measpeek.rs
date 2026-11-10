//! On-screen measurement values (osc_measure results shown, 0x2000016C..) for a generator signal.
//!   measpeek [freq_hz] [tb]
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let f: u32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let tb: u8 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(13);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    let (mut parser, mut out, mut buf) = (Parser::default(), Vec::new(), vec![0u8; 65536]);
    let mut cmd = |port: &mut Conn, c: &str, ms: u64| -> Vec<String> {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        let t0 = Instant::now();
        let mut texts = Vec::new();
        while t0.elapsed() < Duration::from_millis(ms) {
            let Ok(n) = port.read(&mut buf) else { continue };
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) { if let Message::Text(t) = m { texts.push(t) } }
        }
        texts
    };
    for c in ["DEEP 0", "SET SCREEN 2", "SET RUN 1", "SET TMODE 1", "SET GWAVE 1", "SET GDUTY 50", &format!("SET GFREQ {f}"), &format!("SET TB {tb}")] {
        cmd(&mut port, c, 300);
    }
    std::thread::sleep(Duration::from_millis(2000));
    let t = cmd(&mut port, "PEEK 2000016C 48", 400);
    let line = t.iter().find(|t| t.starts_with("PEEK")).expect("no PEEK reply");
    let b: Vec<u8> = line.split(':').nth(1).unwrap().split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
    let v: Vec<i32> = b.chunks(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect();
    println!("generator {f} Hz, tb {tb}: freq {} Hz, period {} ns, duty {:.2} %, high {} ns, low {} ns", v[0], v[1], v[2] as f64 / 100.0, v[4], v[5]);
    println!("  max {:.3} V, min {:.3} V, Vpp {:.3} V, Vpp min10 {:.3} V, RMS {:.3} V, mean {:.3} V",
        v[6] as f64 / 1e6, v[7] as f64 / 1e6, v[8] as f64 / 1e6, v[9] as f64 / 1e6, v[10] as f64 / 1e6, v[11] as f64 / 1e6);
}
