//! Deep-memory trigger diagnosis: for several captures, where the trigger is and what the signal does there.
//!   trigdiag [captures] [commands...]
use dso510_link::{deep::Assembler, device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let want: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(12);
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    for c in args.iter().skip(2).map(|s| s.as_str()).chain(["DEEP 1"]) {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(400));
    }
    let (mut parser, mut asm, mut out, mut buf) = (Parser::default(), Assembler::default(), Vec::new(), vec![0u8; 65536]);
    let t0 = Instant::now();
    let mut got = 0;
    while got < want && t0.elapsed() < Duration::from_secs(30) {
        let Ok(n) = port.read(&mut buf) else { continue };
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            match m {
                Message::Deep(d) => asm.header(d),
                Message::Raw { offset, data } => if let Some(c) = asm.chunk(offset, &data) {
                    got += 1;
                    let level = 128 + c.info.trig_level as i32;
                    let (lo, hi) = (*c.codes.iter().min().unwrap(), *c.codes.iter().max().unwrap());
                    let t = c.trig.round() as isize;
                    let around: Vec<u8> = (t - 6..t + 10).filter(|&i| i >= 0 && (i as usize) < c.codes.len()).map(|i| c.codes[i as usize]).collect();
                    println!("record {:.3} ms (dt {:.3} us), status gen: see GET", c.codes.len() as f64 * c.dt * 1e3, c.dt * 1e6);
                    println!("[{got}/{want}] tb {} edge {} level {} (code {level}) coupling {} | record {}..{} | trig raw {:#06x} -> index {:.0} of {} | samples around: {:?}",
                        c.info.timebase, c.info.trig_edge, c.info.trig_level, c.info.coupling, lo, hi, c.info.trig, c.trig, c.codes.len(), around);
                },
                _ => {}
            }
        }
    }
    port.write_all(b"DEEP 0\n").unwrap();
}
