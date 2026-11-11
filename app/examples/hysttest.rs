//! Trigger hysteresis (firmware v4.19 "THYST") on the device: TRIGTEST with and without it, then the
//! trigger level put on the flat top of the connected square wave (where only noise crosses it) and the
//! full-memory records classified: trigger on a real edge (the signal was low just before) or on noise.
//! Restores the trigger settings afterwards.
//!   hysttest [seconds per setting]
use dso510_link::{deep::Assembler, device, protocol::{Message, Parser, Status}, transport::Conn};
use std::time::{Duration, Instant};

struct Dev {
    port: Conn,
    parser: Parser,
    asm: Assembler,
    buf: Vec<u8>,
}

impl Dev {
    /// sends `c`, collects texts / status / complete records for `ms` (or until a text starting with `until`)
    fn cmd(&mut self, c: &str, ms: u64, until: Option<&str>) -> (Vec<String>, Option<Status>, Vec<dso510_link::deep::DeepCapture>) {
        if !c.is_empty() {
            self.port.write_all(format!("{c}\n").as_bytes()).unwrap();
        }
        let (mut texts, mut st, mut recs, mut out) = (Vec::new(), None, Vec::new(), Vec::new());
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(ms) {
            let Ok(n) = self.port.read(&mut self.buf) else { continue };
            self.parser.feed(&self.buf[..n], &mut out);
            for m in out.drain(..) {
                match m {
                    Message::Text(t) => texts.push(t),
                    Message::Status(s) => st = Some(s),
                    Message::Deep(d) => self.asm.header(d),
                    Message::Raw { offset, data } => {
                        if let Some(c) = self.asm.chunk(offset, &data) {
                            recs.push(c);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(u) = until {
                if texts.iter().any(|t: &String| t.starts_with(u)) {
                    break;
                }
            }
        }
        (texts, st, recs)
    }
}

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let p = device::find_dso().expect("no DSO-510");
    let mut d = Dev { port: Conn::open(&p).unwrap(), parser: Parser::default(), asm: Assembler::default(), buf: vec![0u8; 65536] };
    d.cmd("DEEP 0", 300, None);
    let (_, st, _) = d.cmd("GET", 800, None);
    let s0 = st.expect("no status");
    println!("settings: tb {} vdiv {} mode {} edge {} level {}", s0.timebase, s0.vdiv, s0.trig_mode, s0.trig_edge, s0.trig_level);
    let (t, _, _) = d.cmd("INFO", 1000, Some("DSO-510"));
    println!("{}", t.iter().find(|t| t.starts_with("DSO")).cloned().unwrap_or_default());

    for h in [0, 10] {
        d.cmd(&format!("THYST {h}"), 500, Some("THYST"));
        let (t, _, _) = d.cmd("TRIGTEST R", 8000, Some("TRIGTEST"));
        println!("THYST {h:2}: {}", t.iter().find(|t| t.starts_with("TRIGTEST")).cloned().unwrap_or("no answer".into()));
    }

    // levels of the square wave from a few records
    d.cmd("THYST 0", 500, Some("THYST"));
    d.cmd("SET TMODE 1", 300, None);
    let (_, _, recs) = d.cmd("DEEP 1", 3000, None);
    let Some(c) = recs.last() else {
        println!("no full-memory record");
        return;
    };
    let (lo, hi) = (*c.codes.iter().min().unwrap() as u32, *c.codes.iter().max().unwrap() as u32);
    let mid = (lo + hi) / 2;
    let upper: Vec<u32> = c.codes.iter().map(|&v| v as u32).filter(|&v| v > mid).collect();
    let top = upper.iter().sum::<u32>() as f64 / upper.len().max(1) as f64;
    println!("signal: codes {lo}..{hi}, top {top:.1} (noise on the top {}..{})",
             upper.iter().min().unwrap_or(&0), upper.iter().max().unwrap_or(&0));
    let spp = c.info.spp_capture.max(1.0) as usize;

    let total = 2 * 3 * 2;
    let mut n = 0;
    let t_all = Instant::now();
    for mode in [1u8, 0] {
        for dl in [-1i32, 0, 1] {
            let level = top.round() as i32 + dl - 128;
            for h in [0, 10] {
                n += 1;
                d.cmd(&format!("THYST {h}"), 500, Some("THYST"));
                d.cmd(&format!("SET TMODE {mode}"), 200, None);
                d.cmd("SET TEDGE 0", 200, None);
                d.cmd(&format!("SET TLEVEL {level}"), 600, None);
                let (_, _, recs) = d.cmd("", secs * 1000, None);
                let (mut edge, mut noise, mut none) = (0, 0, 0);
                for c in &recs {
                    if c.info.has_flags && !c.info.triggered {
                        none += 1;
                        continue;
                    }
                    let t = c.trig.round() as usize;
                    // real edge: the signal was in the lower half within 5 pixels before the trigger
                    let from = t.saturating_sub(5 * spp);
                    if c.codes[from..=t.min(c.codes.len() - 1)].iter().any(|&v| (v as u32) < mid) { edge += 1 } else { noise += 1 }
                }
                let left = t_all.elapsed().as_secs_f64() / n as f64 * (total - n) as f64;
                println!("{} level {:+} (code {}) THYST {h:2}: {} records: on an edge {edge}, on noise {noise}, untriggered {none}   [{n}/{total}, ~{left:.0} s left]",
                         if mode == 1 { "Normal" } else { "Auto  " }, dl, level + 128, recs.len());
            }
        }
    }
    d.cmd("THYST 0", 500, Some("THYST"));
    d.cmd("DEEP 0", 300, None);
    for c in [format!("SET TMODE {}", s0.trig_mode), format!("SET TEDGE {}", s0.trig_edge), format!("SET TLEVEL {}", s0.trig_level)] {
        d.cmd(&c, 300, None);
    }
    println!("restored: mode {} edge {} level {}", s0.trig_mode, s0.trig_edge, s0.trig_level);
}
