//! DC accuracy: the probe on a known DC voltage, mean of the screen trace and of the deep record (raw ADC
//! codes and as the screen scales them) at several V/div.
//!   dcmeas [vdivs comma-separated] [restore commands...]
use dso510_link::{deep::Assembler, device, protocol::{px_to_volts, Message, Parser, VDIV_VOLTS}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let vdivs: Vec<u8> = args.get(1).map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect()).unwrap_or(vec![4, 5, 6, 7, 8]);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    let (mut parser, mut out, mut buf) = (Parser::default(), Vec::new(), vec![0u8; 65536]);
    let mut send = |port: &mut Conn, c: &str| {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    };
    for c in ["SET COUPLING 1", "SET TMODE 1", "DEEP 1"] {
        send(&mut port, c);
    }
    println!("V/div    screen mean   deep raw mean   deep scaled mean   zoom");
    for (k, &v) in vdivs.iter().enumerate() {
        send(&mut port, &format!("SET VDIV {v}"));
        let t0 = Instant::now();
        let mut asm = Assembler::default();
        let (mut frames, mut fsum, mut fn_) = (0, 0.0, 0.0);
        let mut deep = None;
        let mut offset = 0i8;
        while t0.elapsed() < Duration::from_secs(4) && (frames < 10 || deep.is_none()) {
            let Ok(n) = port.read(&mut buf) else { continue };
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) {
                match m {
                    Message::Frame(f) if t0.elapsed() > Duration::from_millis(1200) && f.vdiv == v => {
                        frames += 1;
                        offset = f.offset;
                        let s = &f.trace[f.trace_start as usize..(f.trace_end as usize).min(f.trace.len())];
                        fsum += s.iter().map(|&x| x as f64).sum::<f64>();
                        fn_ += s.len() as f64;
                    }
                    Message::Deep(d) => asm.header(d),
                    Message::Raw { offset, data } => {
                        if let Some(c) = asm.chunk(offset, &data) {
                            if t0.elapsed() > Duration::from_millis(1200) && c.info.vdiv == v { deep = Some(c) }
                        }
                    }
                    _ => {}
                }
            }
        }
        let vpd = VDIV_VOLTS[v as usize];
        let fmean = if fn_ > 0.0 { px_to_volts(fsum / fn_, vpd, offset) } else { f64::NAN };
        match deep {
            Some(c) => {
                let mean = |s: &[u8]| s.iter().map(|&x| x as f64).sum::<f64>() / s.len() as f64;
                let raw = c.adc.as_ref().unwrap_or(&c.codes);
                println!("{:>6} V  {:>9.3} V   {:>11.3} V   {:>14.3} V   {}", vpd, fmean,
                         px_to_volts(mean(raw), vpd, c.info.offset), px_to_volts(mean(&c.codes), vpd, c.info.offset), c.info.zoom);
            }
            None => println!("{:>6} V  {:>9.3} V   (no deep record)", vpd, fmean),
        }
        eprintln!("PROGRESS {}/{}", k + 1, vdivs.len());
    }
    for c in args.iter().skip(2) {
        send(&mut port, c);
    }
    send(&mut port, "DEEP 0");
}
