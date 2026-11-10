//! Trigger map vs direct check on the device (firmware "TRIGTEST"), over all 252 settings; a few minutes.
//! Prints progress with the time left. Single at 50 / 250 ns/div is reported separately: there the DMA
//! keeps writing the buffer after Stop, so the two searches see different data (test artifact).
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    let (mut parser, mut out) = (Parser::default(), Vec::new());
    let mut buf = vec![0u8; 65536];
    // waits `wait_ms`, or only until a text starting with `until` arrives
    let mut cmd = |port: &mut Conn, c: &str, wait_ms: u64, until: Option<&str>| -> Vec<String> {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        let t0 = Instant::now();
        let mut texts = Vec::new();
        while t0.elapsed() < Duration::from_millis(wait_ms) {
            let Ok(n) = port.read(&mut buf) else { continue };
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) { if let Message::Text(t) = m { texts.push(t) } }
            if let Some(u) = until { if texts.iter().any(|t: &String| t.starts_with(u)) { break; } }
        }
        texts
    };
    std::thread::sleep(Duration::from_millis(300));
    for c in ["DEEP 0", "SET SCREEN 2", "SET TMODE 1", "SET RUN 1"] { cmd(&mut port, c, 300, None); }
    let (mut total, mut bad, mut orig, mut fast, mut runs) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut ram, mut badram, mut worst_orig, mut worst_ram) = (0u64, 0u64, 0u64, 0u64);
    let (mut known, mut known_settings, mut unexpected_settings) = (0u64, 0u64, 0u64);
    let t_start = Instant::now();
    const SETTINGS: u64 = 7 * 3 * 3 * 2 * 2;
    for tb in [0u8, 2, 4, 8, 11, 13, 16] {
        cmd(&mut port, "SET RUN 1", 200, None);
        cmd(&mut port, &format!("SET TB {tb}"), 900, None);           // a fresh capture at this time base
        for tmode in [1u8, 0, 2] {
            cmd(&mut port, &format!("SET TMODE {tmode}"), 600, None);
            cmd(&mut port, "SET RUN 0", 400, None);
            for edge in [0u8, 1, 2] {
                // edge 2 ("both") is not reachable from the menu, poke it directly
                cmd(&mut port, &format!("POKE8 200000ED {edge}"), 120, None);
                for zoom in [0u8, 128] {
                    cmd(&mut port, &format!("POKE8 2000027A {zoom}"), 120, None);
                    for running in ["", " R"] {
                        let texts = cmd(&mut port, &format!("TRIGTEST{running}"), 3500, None);
                        if let Some(t) = texts.iter().find(|t| t.starts_with("TRIGTEST")) { if std::env::var_os("VERBOSE").is_some() { println!("tb {tb} tmode {tmode} edge {edge} zoom {zoom}{running}: {t}"); } }
                        let Some(t) = texts.iter().find(|t| t.starts_with("TRIGTEST")) else {
                            println!("tb {tb} tmode {tmode} edge {edge} zoom {zoom}{running}: no reply"); continue;
                        };
                        let num = |key: &str| t.split(key).nth(1).and_then(|r| r.split_whitespace().next()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                        total += num(" n "); bad += num(" bad "); orig += num(" direct "); fast += num(" avg "); runs += 1;
                        if num(" direct ") > worst_orig { worst_orig = num(" direct "); worst_ram = num(" avg "); }
                        if num(" bad ") > 0 {
                            if tmode == 2 && tb < 4 {                 // DMA keeps writing after Stop: artifact
                                known += num(" bad "); known_settings += 1;
                            } else {
                                unexpected_settings += 1;
                                println!("MISMATCH tb {tb} tmode {tmode} edge {edge} zoom {zoom}{running}: {t}");
                            }
                        }
                        if runs % 12 == 0 {
                            let left = t_start.elapsed().as_secs_f64() / runs as f64 * (SETTINGS - runs) as f64;
                            println!("PROGRESS {runs}/{SETTINGS} settings, real mismatches in {unexpected_settings} settings, ~{}m{:02}s left",
                                left as u64 / 60, left as u64 % 60);
                        }
                    }
                }
            }
            cmd(&mut port, "POKE8 200000ED 0", 100, None);
        }
    }
    cmd(&mut port, "SET TMODE 1", 300, None);
    cmd(&mut port, "SET RUN 1", 300, None);
    let us = |c: u64| c as f64 / runs.max(1) as f64 / 239.0;
    let _ = (ram, badram);
    println!("{runs} settings, {total} searches; map vs direct check mismatches: {} in {unexpected_settings} settings         (+ {known} in {known_settings} Single settings at 50/250 ns/div: DMA artifact, expected)", bad - known);
    println!("average per search: direct check {:.0} us, with the trigger map {:.0} us ({:.1}x)", us(orig), us(fast), orig as f64 / fast.max(1) as f64);
    println!("slowest setting: direct {:.0} us -> map {:.0} us", worst_orig as f64 / 239.0, worst_ram as f64 / 239.0);
}
