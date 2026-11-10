//! On-screen measurements for "odd" generator frequencies and several duty cycles, against a reference
//! from the deep memory record (30100 samples, measured on the PC: mid-level crossings).
//!   measmatrix [freqs comma-separated] [duties comma-separated]
use dso510_link::{deep::{Assembler, DeepCapture}, device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

// time per division of the time base indices 0..17 (s)
const TB_DIV: [f64; 18] = [50e-9, 100e-9, 250e-9, 500e-9, 1e-6, 2e-6, 5e-6, 10e-6, 20e-6, 50e-6, 100e-6, 200e-6, 500e-6, 1e-3, 2e-3, 5e-3, 10e-3, 20e-3];

struct Io { port: Conn, parser: Parser, out: Vec<Message>, buf: Vec<u8>, asm: Assembler, capture: Option<DeepCapture> }
impl Io {
    fn cmd(&mut self, c: &str, ms: u64) -> Vec<String> {
        self.port.write_all(format!("{c}\n").as_bytes()).unwrap();
        let t0 = Instant::now();
        let mut texts = Vec::new();
        while t0.elapsed() < Duration::from_millis(ms) {
            let Ok(n) = self.port.read(&mut self.buf) else { continue };
            self.parser.feed(&self.buf[..n], &mut self.out);
            for m in self.out.drain(..) {
                match m {
                    Message::Text(t) => texts.push(t),
                    Message::Deep(d) => self.asm.header(d),
                    Message::Raw { offset, data } => if let Some(c) = self.asm.chunk(offset, &data) { self.capture = Some(c) },
                    _ => {}
                }
            }
        }
        texts
    }
}

fn main() {
    let arg = |i: usize, d: &str| std::env::args().nth(i).unwrap_or(d.to_string()).split(',').filter_map(|s| s.parse::<u32>().ok()).collect::<Vec<_>>();
    let freqs = arg(1, "37,123,777,1234,4321,12345,33333,47000");
    let duties = arg(2, "10,25,50,75,90");
    let p = device::find_dso().expect("no DSO-510");
    let mut io = Io { port: Conn::open(&p).unwrap(), parser: Parser::default(), out: Vec::new(), buf: vec![0u8; 65536],
                      asm: Assembler::default(), capture: None };
    for c in ["DEEP 0", "SET SCREEN 2", "SET RUN 1", "SET TMODE 1", "SET COUPLING 1", "SET GWAVE 1", "SET VDIV 6", "SET TLEVEL 0", "DEEP 1"] {
        io.cmd(c, 300);
    }
    let total = freqs.len() * duties.len();
    let t_start = Instant::now();
    println!("set f, duty, tb | reference (deep record) | shown on screen: freq and error, duty and error | high, low | their errors");
    let mut k = 0;
    for &f in &freqs {
        let tb = (0..TB_DIV.len()).find(|&i| TB_DIV[i] * 12.0 >= 2.0 / f as f64).unwrap_or(17);   // 2..6 periods on screen
        io.cmd(&format!("SET TB {tb}"), 300);
        io.cmd(&format!("SET GFREQ {f}"), 300);
        for &d in &duties {
            k += 1;
            io.cmd(&format!("SET GDUTY {d}"), 1700);                 // > two 0.5 s averaging windows
            io.capture = None;
            io.cmd("PING", 1500);                                    // a fresh deep record
            let reference = io.capture.as_ref().map(|c| c.measure_range(0, c.len()));
            let line = io.cmd("PEEK 2000016C 24", 400).into_iter().find(|t| t.starts_with("PEEK")).unwrap_or_default();
            let b: Vec<u8> = line.split(':').nth(1).unwrap_or("").split_whitespace().filter_map(|h| u8::from_str_radix(h, 16).ok()).collect();
            if b.len() < 24 { println!("{f:>7} {d:>3}%: no reply"); continue; }
            let v: Vec<i32> = b.chunks(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect();
            let (fs, duty, high, low) = (v[0] as f64, v[2] as f64 / 100.0, v[4] as f64, v[5] as f64);
            let rf = reference.as_ref().and_then(|m| m.freq).unwrap_or(f as f64);
            let rd = reference.as_ref().and_then(|m| m.duty).unwrap_or(d as f64);
            let per = 1e9 / rf;
            let (he, le) = (per * rd / 100.0, per * (100.0 - rd) / 100.0);
            let ns = |x: f64| if x >= 1e6 { format!("{:.3} ms", x / 1e6) } else { format!("{:.2} us", x / 1e3) };
            println!("{f:>6} {d:>2}% tb{tb:<2} | ref {rf:>9.1} Hz {rd:>5.1}% | {fs:>8.0} {:>+6.2}% {duty:>6.2}% {:>+5.1} | {:>10} {:>10} | {:>+6.1}% {:>+6.1}%",
                (fs / rf - 1.0) * 100.0, duty - rd, ns(high), ns(low), (high / he - 1.0) * 100.0, (low / le - 1.0) * 100.0);
            if k % 5 == 0 {
                let left = t_start.elapsed().as_secs_f64() / k as f64 * (total - k) as f64;
                println!("        PROGRESS {k}/{total}, ~{left:.0} s left");
            }
        }
    }
    for c in ["DEEP 0", "SET GDUTY 50", "SET GFREQ 1000", "SET TB 13"] { io.cmd(c, 300); }
}
