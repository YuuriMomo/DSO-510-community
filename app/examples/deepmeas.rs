//! Deep memory end-to-end check: captures through deep::Assembler and prints full-resolution
//! measurements next to the ones from the screen trace.
//!   deepmeas [captures] [commands...]      e.g. deepmeas 3 "SET TB 13"
use dso510_link::{deep::Assembler, device, measure::{self, eng}, protocol::{Message, Parser}, spectrum, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let want: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    for c in args.iter().skip(2) {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
    }
    port.write_all(b"DEEP 1\n").unwrap();
    let (mut parser, mut asm, mut out) = (Parser::default(), Assembler::default(), Vec::new());
    let mut buf = vec![0u8; 65536];
    let mut last_frame = None;
    let mut got = 0;
    let t0 = Instant::now();
    while got < want && t0.elapsed() < Duration::from_secs(10) {
        let Ok(n) = port.read(&mut buf) else { continue };
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            match m {
                Message::Frame(f) => last_frame = Some(f),
                Message::Deep(d) => asm.header(d),
                Message::Raw { offset, data } => {
                    if let Some(c) = asm.chunk(offset, &data) {
                        got += 1;
                        let d = c.measure();
                        if let Ok(dir) = std::env::var("SAVE") { std::fs::write(format!("{dir}/cap_{}_{}.bin", c.info.timebase_applied, c.info.seq), &c.codes).ok(); }
                        println!(
                            "#{} tb {} ({}): {} Sa/s, record {} ({} pre-trigger){}",
                            c.info.seq, c.info.timebase_applied, eng(c.dt, "s"), eng(c.sample_rate(), ""), eng(c.t_last() - c.t_first(), "s"),
                            eng(-c.t_first(), "s"), if c.info.interleave { ", interleaved" } else { "" }
                        );
                        let show = |name: &str, m: &measure::Measurements| {
                            println!(
                                "   {name:<6} Vpp {:>9}  Vmax {:>9}  Vmin {:>9}  freq {:>10}  duty {:>6}  rise {:>9}  fall {:>9}",
                                eng(m.vpp, "V"), eng(m.vmax, "V"), eng(m.vmin, "V"),
                                m.freq.map(|f| eng(f, "Hz")).unwrap_or("---".into()),
                                m.duty.map(|d| format!("{d:.1}%")).unwrap_or("---".into()),
                                m.rise.map(|r| eng(r, "s")).unwrap_or("---".into()),
                                m.fall.map(|r| eng(r, "s")).unwrap_or("---".into())
                            );
                        };
                        show("deep", &d);
                        let v: Vec<f64> = c.codes.iter().map(|&x| c.volts(x)).collect();
                        let sp = spectrum::compute(&v, c.dt, spectrum::Window::BlackmanHarris);
                        if let Some(f) = &sp.fundamental {
                            let h: Vec<String> = sp.harmonics.iter().take(5).map(|h| format!("H{} {:.1}", h.order, h.dbc)).collect();
                            println!("   fft    f0 {} {} rms  THD {:.1}%  SFDR {:.1} dB  floor {:.1} dBV  {}", eng(f.freq, "Hz"), eng(f.vrms, "V"), sp.thd.unwrap_or(0.0), sp.sfdr.unwrap_or(0.0), sp.noise_floor, h.join(" "));
                        }
                        if let Some(f) = &last_frame {
                            show("screen", &measure::measure(f));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    port.write_all(b"DEEP 0\n").unwrap();
    println!("crc errors {}, incomplete {}", parser.crc_errors, asm.incomplete);
}
