//! Profiling of the original firmware (firmware v4.3 "PROF"): cycles at 239 MHz for measure / build /
//! draw / whole frame at a few time bases, plus the generator jitter experiment ("GFREQRAW").
//!   prof            profile per time base
//!   prof gen        edge jitter of the captured generator square wave vs generator frequency
use dso510_link::{deep::Assembler, device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

struct Io { port: Conn, parser: Parser, out: Vec<Message>, buf: Vec<u8> }
impl Io {
    fn cmd(&mut self, c: &str) { self.port.write_all(format!("{c}\n").as_bytes()).unwrap(); }
    /// pump for `d`, return texts and messages
    fn pump(&mut self, d: Duration, mut f: impl FnMut(Message)) {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            let Ok(n) = self.port.read(&mut self.buf) else { continue };
            self.parser.feed(&self.buf[..n], &mut self.out);
            for m in self.out.drain(..) { f(m); }
        }
    }
}

fn mhz(c: &str) -> String {
    // "PROF measure a/b build ..." -> convert cycle numbers to microseconds
    c.split(' ').map(|w| {
        if let Some((a, b)) = w.split_once('/') {
            if let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) { return format!("{:.0}/{:.0}us", a / 239.0, b / 239.0); }
        }
        w.to_string()
    }).collect::<Vec<_>>().join(" ")
}

fn main() {
    let gen_mode = std::env::args().nth(1).as_deref() == Some("gen");
    let p = device::find_dso().expect("no DSO-510");
    let mut io = Io { port: Conn::open(&p).unwrap(), parser: Parser::default(), out: Vec::new(), buf: vec![0u8; 65536] };
    std::thread::sleep(Duration::from_millis(300));
    for c in ["DEEP 0", "SET SCREEN 2", "SET TMODE 1", "SET RUN 1"] { io.cmd(c); io.pump(Duration::from_millis(300), |_| {}); }
    // PRE="cmd;cmd": extra commands first (e.g. FASTBUILD 1); TBS="0,1,2": time bases
    for c in std::env::var("PRE").unwrap_or_default().split(';').filter(|c| !c.is_empty()) {
        io.cmd(c);
        io.pump(Duration::from_millis(300), |m| if let Message::Text(t) = m { println!("{t}") });
    }
    if !gen_mode {
        let tbs: Vec<u8> = std::env::var("TBS").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or(vec![2, 4, 8, 11, 13, 16]);
        for tb in tbs {
            io.cmd(&format!("SET TB {tb}"));
            io.pump(Duration::from_millis(800), |_| {});
            io.cmd("PROFR");
            io.pump(Duration::from_millis(200), |_| {});
            let mut frames = 0;
            io.pump(Duration::from_secs(3), |m| if let Message::Frame(_) = m { frames += 1 });
            io.cmd("PROF");
            let mut txt = String::new();
            io.pump(Duration::from_millis(400), |m| if let Message::Text(t) = m { if t.starts_with("PROF") { txt = t } });
            println!("tb {tb:2} ({:.0} frames/s): {}", frames as f64 / 3.0, mhz(&txt));
            io.cmd("PROFD");
            let mut parts = String::new();
            io.pump(Duration::from_millis(400), |m| if let Message::Text(t) = m { if t.starts_with("PROFD") { parts = t } });
            if !parts.is_empty() {
                let names = ["grid", "text", "ovl1", "ovl2", "ovl3", "flush", "wait"];
                let v: Vec<String> = parts.split_whitespace().skip(1).zip(names).map(|(w, n)| format!("{n} {}", mhz(w))).collect();
                println!("        frame parts (avg/max): {}", v.join(", "));
            }
        }
        return;
    }
    // generator experiment: square wave on the probe, deep capture, period-to-period jitter with hysteresis
    io.cmd("SET GWAVE 1"); io.cmd("SET GDUTY 50"); io.pump(Duration::from_millis(300), |_| {});
    let tbs: Vec<u8> = std::env::var("TBS").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or(vec![8, 4, 2]);
    for tb in tbs {
        io.cmd(&format!("SET TB {tb}"));
        io.pump(Duration::from_millis(600), |_| {});
        for f in [1000u32, 5000, 10000, 20000, 50000] {
            io.cmd(&format!("GFREQRAW {f}"));
            io.pump(Duration::from_millis(600), |_| {});
            io.cmd("DEEP 1");
            let mut asm = Assembler::default();
            let mut caps = Vec::new();
            io.pump(Duration::from_secs(if tb == 2 { 6 } else { 3 }), |m| match m {
                Message::Deep(d) => asm.header(d),
                Message::Raw { offset, data } => if let Some(c) = asm.chunk(offset, &data) { caps.push(c) },
                _ => {}
            });
            io.cmd("DEEP 0");
            io.pump(Duration::from_millis(300), |_| {});
            if let Ok(dir) = std::env::var("DUMP") {
                if let Some(c) = caps.last() { std::fs::write(format!("{dir}/gen_tb{tb}_{f}.bin"), &c.codes).ok(); }
            }
            let mut periods = Vec::new();
            let mut dt = 0.0;
            for c in &caps {
                dt = c.dt;
                let (lo, hi) = (*c.codes.iter().min().unwrap() as f64, *c.codes.iter().max().unwrap() as f64);
                if hi - lo < 20.0 { continue; }
                let (l, h) = (lo + (hi - lo) * 0.3, lo + (hi - lo) * 0.7);
                let mid = (lo + hi) / 2.0;
                let mut armed = false;
                let mut edges = Vec::new();
                for i in 1..c.codes.len() {
                    let x = c.codes[i] as f64;
                    if x < l { armed = true; }
                    if armed && x > h {
                        // interpolate the mid crossing going backwards
                        let mut j = i;
                        while j > 1 && (c.codes[j - 1] as f64) >= mid { j -= 1; }
                        let (a, b) = (c.codes[j - 1] as f64, c.codes[j] as f64);
                        edges.push(j as f64 - 1.0 + if b != a { (mid - a) / (b - a) } else { 0.0 });
                        armed = false;
                    }
                }
                for w in edges.windows(2) { periods.push((w[1] - w[0]) * c.dt); }
            }
            if periods.len() < 2 { println!("tb {tb} gen {f:6} Hz: not enough edges ({} captures)", caps.len()); continue; }
            let mean = periods.iter().sum::<f64>() / periods.len() as f64;
            let sd = (periods.iter().map(|p| (p - mean) * (p - mean)).sum::<f64>() / periods.len() as f64).sqrt();
            let (mn, mx) = periods.iter().fold((f64::MAX, f64::MIN), |(a, b), &p| (a.min(p), b.max(p)));
            println!(
                "tb {tb} (dt {:5.1} ns) gen {f:6} Hz: {} periods, mean {:9.3} us (expected {:9.3}), jitter rms {:6.1} ns, p-p {:6.1} ns",
                dt * 1e9, periods.len(), mean * 1e6, 1e6 / f as f64, sd * 1e9, (mx - mn) * 1e9
            );
        }
    }
    io.cmd("SET GFREQ 1000");
    io.pump(Duration::from_millis(300), |_| {});
}
