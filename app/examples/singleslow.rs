//! Single mode at the roll time bases (>= 50 ms/div): does a triggered record reach the host?
//!   singleslow [tb]   (generator: 5 Hz square on the probe)
use dso510_link::{deep::Assembler, device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let tb: u8 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(18);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    let (mut parser, mut out, mut buf) = (Parser::default(), Vec::new(), vec![0u8; 65536]);
    let mut pump = |port: &mut Conn, d: Duration, f: &mut dyn FnMut(Message)| {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            let Ok(n) = port.read(&mut buf) else { continue };
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) { f(m); }
        }
    };
    for c in ["DEEP 0", "SET SCREEN 2", "SET GWAVE 1", "SET GFREQ 5", "SET TMODE 1", &format!("SET TB {tb}"), "SET TLEVEL 0", "DEEP 1", "SET TMODE 2", "SET RUN 1"] {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        pump(&mut port, Duration::from_millis(300), &mut |_| {});
    }
    println!("Single at tb {tb}: waiting up to 15 s for a record (progress every 3 s)");
    let mut asm = Assembler::default();
    let (mut headers, mut rolls, mut done) = (0, 0, None);
    let t0 = Instant::now();
    for step in 1..=5 {
        pump(&mut port, Duration::from_secs(3), &mut |m| match m {
            Message::Deep(d) => { headers += 1; asm.header(d) }
            Message::Roll(_) => rolls += 1,
            Message::Raw { offset, data } => if let Some(c) = asm.chunk(offset, &data) { done.get_or_insert(c); },
            _ => {}
        });
        println!("  [{step}/5] {:>2} s: record headers {headers}, roll chunks {rolls}, complete record: {}",
            t0.elapsed().as_secs(), done.is_some());
        if done.is_some() { break; }
    }
    if let Some(c) = &done {
        let s = &c.codes;
        let (mn, mx) = (s.iter().min().unwrap(), s.iter().max().unwrap());
        let edges = s.windows(2).filter(|w| (w[0] < 128) != (w[1] < 128)).count();
        let mids: Vec<String> = s.chunks(1000).map(|k| format!("{}", k.iter().map(|&v| v as u32).sum::<u32>() / k.len() as u32)).collect();
        println!("mean of every 1000 samples (21 ms): {}", mids.join(" "));
        let mut state = s[0] >= 132; let mut flips = 0;
        for &v in s.iter() { if state && v < 124 { state = false; flips += 1 } else if !state && v > 132 { state = true; flips += 1 } }
        println!("edges with hysteresis 124/132: {flips}");
        println!("record: {} samples over {:.2} s (dt {:.1} us), min {mn} max {mx}, {edges} crossings of mid-scale (5 Hz square: ~{:.0} expected)", s.len(), s.len() as f64 * c.dt, c.dt * 1e6, s.len() as f64 * c.dt * 10.0);
    }
    for c in ["DEEP 0", "SET TMODE 1", "SET TB 4"] { port.write_all(format!("{c}\n").as_bytes()).unwrap(); pump(&mut port, Duration::from_millis(200), &mut |_| {}); }
}
