//! Hardware frequency counter (TMR2 edges per gate, "FREQHW") vs generator frequency and the on-screen value.
//!   freqhw [hz ...]
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
    let freqs: Vec<u32> = { let v: Vec<u32> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect(); if v.is_empty() { vec![10, 100, 1000, 5000, 10000, 20000, 50000] } else { v } };
    for c in ["DEEP 0", "SET SCREEN 2", "SET RUN 1", "SET TMODE 1", "SET GWAVE 1", "SET GDUTY 50", "SET TB 13"] { cmd(&mut port, c, 300); }
    for (i, f) in freqs.iter().enumerate() {
        cmd(&mut port, &format!("SET GFREQ {f}"), 1600);
        let mut counts = Vec::new();
        for _ in 0..4 {
            if let Some(t) = cmd(&mut port, "FREQHW", 520).iter().find(|t| t.starts_with("FREQHW")) {
                counts.push(t.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0));
            }
        }
        let shown = cmd(&mut port, "PEEK 2000016C 4", 400).into_iter().find(|t| t.starts_with("PEEK")).map(|t| {
            let b: Vec<u8> = t.split(':').nth(1).unwrap().split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
            i32::from_le_bytes(b[..4].try_into().unwrap())
        }).unwrap_or(0);
        let avg = counts.iter().sum::<u64>() as f64 / counts.len().max(1) as f64;
        println!("[{}/{}] generator {f:>6} Hz: counter {:?} -> {:.4} counts per Hz (gate {:.1} ms); on screen {shown} Hz",
            i + 1, freqs.len(), counts, avg / *f as f64, avg / *f as f64 * 1000.0);
    }
}
