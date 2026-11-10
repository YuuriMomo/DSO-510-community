//! Runs the app's frequency-response sweep (bode.rs) without the window: reference + measurement with
//! the probe on the generator output, which must give a flat 0 dB line.
//!   bodetest [points]
use dso510_link::{bode::{Bode, Target}, device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};
fn main() {
    let steps: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    let (mut parser, mut out, mut buf) = (Parser::default(), Vec::new(), vec![0u8; 65536]);
    let mut bode = Bode::default();
    bode.steps = steps;
    let mut frame = None;
    let mut vdiv = 6u8;
    for target in [Target::Reference, Target::Measurement] {
        let mut queue: Vec<String> = Vec::new();
        bode.start(target, vdiv, Vec::new(), &mut |c| queue.push(c));
        let t0 = Instant::now();
        let mut last_print = 0;
        while bode.running().is_some() && t0.elapsed() < Duration::from_secs(300) {
            for c in queue.drain(..) { port.write_all(format!("{c}\n").as_bytes()).unwrap(); std::thread::sleep(Duration::from_millis(30)); }
            if let Ok(n) = port.read(&mut buf) {
                parser.feed(&buf[..n], &mut out);
                for m in out.drain(..) { if let Message::Frame(f) = m { vdiv = f.vdiv; frame = Some(f) } }
            }
            bode.tick(frame.as_ref(), &mut |c| queue.push(c));
            if let Some((_, i, n)) = bode.running() { if i != last_print { last_print = i; println!("  {target:?}: {i}/{n}, ~{:.0} s left", t0.elapsed().as_secs_f64() / i as f64 * (n - i) as f64); } }
        }
        println!("{target:?} done in {:.0} s", t0.elapsed().as_secs_f64());
    }
    for (p, (hz, db)) in bode.measured.iter().zip(bode.gain_db()) {
        println!("{:>8.0} Hz (measured {:>9.1} Hz): {:.3} V peak, gain {:+.2} dB", p.set_hz, hz, p.amp, db);
    }
    let (max, cuts) = bode.cutoffs();
    println!("max {:?}, -3 dB points {:?}", max, cuts);
}
