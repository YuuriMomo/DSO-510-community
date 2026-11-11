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
                    // the whole record in 100 columns: level 0..9 of the column's mean, '|' marks the trigger column
                    let w = c.codes.len().div_ceil(100);
                    let prof: String = c.codes.chunks(w).enumerate().map(|(i, ch)| {
                        if (c.trig as usize) / w == i { '|' } else {
                            let m = ch.iter().map(|&v| v as u32).sum::<u32>() / ch.len() as u32;
                            char::from(b'0' + (m * 10 / 256) as u8)
                        }
                    }).collect();
                    println!("    {prof}");
                    // the firmware's falling / rising condition on the received samples (raw index = codes index + 4)
                    let d = (c.info.spp_capture.max(1.0)) as usize;
                    let lv = level as u8;
                    let cond = |i: usize| -> bool {
                        if i + 3 * d >= c.codes.len() { return false }
                        let v = |k: usize| c.codes[i + k * d];
                        let fall = v(0) >= lv && v(1) < lv && v(2) < lv && v(3) < lv;
                        let rise = v(0) <= lv && v(1) > lv && v(2) > lv && v(3) > lv;
                        match c.info.trig_edge { 0 => rise, 1 => fall, _ => rise || fall }
                    };
                    let hits: Vec<usize> = (0..c.codes.len()).filter(|&i| cond(i)).collect();
                    let at = if c.trig >= 0.0 { cond(c.trig as usize) } else { false };
                    println!("    spp {} zoom {} triggered {} | condition at trig: {at} | {} positions meet it, first: {:?}", c.info.spp_capture, c.info.zoom, c.info.triggered, hits.len(), &hits[..hits.len().min(8)]);
                },
                _ => {}
            }
        }
    }
    port.write_all(b"DEEP 0\n").unwrap();
}
