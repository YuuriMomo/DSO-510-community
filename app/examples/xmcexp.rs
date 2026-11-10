//! LCD bus (XMC) write timing experiment: for each BK1WTMG value, the time the frame DMA takes ("PROFD" wait).
//!   xmcexp <hex value> [<hex value> ...]   (the original 0FF00202 is restored at the end)
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};
fn main() {
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
    cmd(&mut port, "DEEP 0", 300);
    let values: Vec<String> = std::env::args().skip(1).collect();
    let n = values.len();
    for (i, v) in values.iter().chain(std::iter::once(&"0FF00202".to_string())).enumerate() {
        let r = cmd(&mut port, &format!("POKE32 A0000104 {v}"), 300);
        cmd(&mut port, "PROFR", std::env::var("HOLD_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500));
        let t = cmd(&mut port, "PROFD", 3000).into_iter().chain(cmd(&mut port, "PROFD", 400)).find(|t| t.starts_with("PROFD")).unwrap_or_default();
        let wait = t.split_whitespace().nth(7).unwrap_or("?");
        let us = wait.split('/').next().and_then(|c| c.parse::<f64>().ok()).map(|c| format!("{:.0} us", c / 239.0)).unwrap_or("?".into());
        let label = if i == n { " (original, restored)" } else { "" };
        println!("[{}/{}] BK1WTMG {v}{label}: {}  -> frame DMA {us}", i + 1, n + 1, r.first().map(|s| s.as_str()).unwrap_or("no reply"));
    }
}
