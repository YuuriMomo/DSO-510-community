//! Deep memory test: "DEEP 1", counts complete captures for a few seconds, prints the rate and saves
//! the first captures (header, 30100 samples, the screen trace drawn from them) for analysis.
//!   deeptest [seconds] [out_dir] [extra commands...]
use dso510_link::{device, protocol::{DeepInfo, Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let dir = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir).unwrap();
    // DSO510_COM=1 forces the COM port even when the WinUSB device is there
    let p = if std::env::var_os("DSO510_COM").is_some() {
        device::list().into_iter().find(|p| p.is_dso && p.path != device::WINUSB_PATH)
    } else {
        device::find_dso()
    }
    .expect("no DSO-510");
    println!("using {}", p.label);
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    port.write_all(b"DEEP 1\n").unwrap();
    // commands on a schedule, sent from the read loop (reading never stops)
    let mut schedule: Vec<(Duration, String)> = Vec::new();
    let mut at = Duration::ZERO;
    for c in args.iter().skip(3) {
        at += Duration::from_millis(300);
        if let Some(ms) = c.strip_prefix("WAIT ") {
            at += Duration::from_millis(ms.parse().unwrap_or(0));
        } else {
            schedule.push((at, c.clone()));
        }
    }
    schedule.reverse();
    let verbose = std::env::var_os("VERBOSE").is_some();

    let mut parser = Parser::default();
    let mut buf = vec![0u8; 65536];
    let mut out = Vec::new();
    let (mut bytes, mut frames, mut headers, mut complete, mut partial) = (0usize, 0, 0, 0, 0);
    let mut cur: Option<(DeepInfo, Vec<u8>, usize)> = None;
    let mut pending: Vec<(DeepInfo, Vec<u8>)> = Vec::new(); // complete capture waiting for its trace
    let mut saved = 0;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        while schedule.last().is_some_and(|(t, _)| t0.elapsed() >= *t) {
            let (_, c) = schedule.pop().unwrap();
            port.write_all(format!("{c}\n").as_bytes()).unwrap();
        }
        let Ok(n) = port.read(&mut buf) else { continue };
        bytes += n;
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            match m {
                Message::Deep(d) => {
                    if verbose { println!("{:.3} D seq {} start {} trig {} tb {}", t0.elapsed().as_secs_f64(), d.seq, d.start, d.trig, d.timebase_applied); }
                    headers += 1;
                    if cur.take().is_some() {
                        partial += 1;
                    }
                    cur = Some((d, vec![0; 30100], 0));
                }
                Message::Raw { offset, data } => {
                    if verbose { println!("{:.3} R {offset} {}", t0.elapsed().as_secs_f64(), data.len()); }
                    if let Some((_, raw, got)) = cur.as_mut() {
                        let o = offset as usize;
                        if o == *got && o + data.len() <= raw.len() {
                            raw[o..o + data.len()].copy_from_slice(&data);
                            *got += data.len();
                        }
                    }
                    if cur.as_ref().is_some_and(|c| c.2 >= 30100) {
                        let (d, raw, _) = cur.take().unwrap();
                        complete += 1;
                        pending.push((d, raw));
                    }
                }
                Message::Frame(f) => {
                    frames += 1;
                    for (d, raw) in pending.drain(..) {
                        if saved < 6 {
                            let base = dir.join(format!("deep_{saved}"));
                            std::fs::write(base.with_extension("raw"), &raw).unwrap();
                            std::fs::write(base.with_extension("trace"), &f.trace).unwrap();
                            std::fs::write(base.with_extension("txt"), format!("{d:#?}\nframe tb {} start {} end {}\n", f.timebase, f.trace_start, f.trace_end)).unwrap();
                            saved += 1;
                        }
                    }
                }
                Message::Text(t) => println!("text: {t}"),
                Message::Status(st) if verbose => println!("{:.3} S running {} tmode {} tb {} wait? app {}", t0.elapsed().as_secs_f64(), st.running, st.trig_mode, st.timebase, st.app_mode),
                _ => {}
            }
        }
    }
    port.write_all(b"DEEP 0\n").unwrap();
    let s = secs as f64;
    println!(
        "{s} s: {:.1} kB/s, deep headers {headers}, complete {complete} ({:.1}/s), partial {partial}, frames {frames} ({:.1}/s), crc errors {}",
        bytes as f64 / 1024.0 / s, complete as f64 / s, frames as f64 / s, parser.crc_errors
    );
}
