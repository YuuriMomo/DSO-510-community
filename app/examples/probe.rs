//! Console connection test: finds the DSO-510, opens it like the GUI does, prints what arrives.
//!   cargo run --release --example probe -- [seconds] [command ...]
use dso510_link::{device, link::{Event, Link}, protocol::Message};
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    let cmds: Vec<String> = args.collect();
    println!("ports:");
    for p in device::list() {
        println!("  {}{}  ->  {}", if p.is_dso { "* " } else { "  " }, p.label, p.path);
    }
    let link = Link::spawn(|| {});
    let t0 = Instant::now();
    let (mut frames, mut status, mut texts) = (0u32, 0u32, 0u32);
    let mut sent = false;
    while t0.elapsed() < Duration::from_secs(secs) {
        while let Ok(e) = link.events.try_recv() {
            match e {
                Event::Connected(p) => println!("connected: {} ({})", p.label, p.path),
                Event::Disconnected(w) => println!("disconnected: {w}"),
                Event::WriteError(w) => println!("write error: {w}"),
                Event::Message(Message::Frame(f)) => {
                    frames += 1;
                    if frames == 1 {
                        println!("first frame: {} / {}, trig {}, running {}, {} points", f.vdiv_label(), f.timebase_label(), f.trig_level, f.running, f.trace.len());
                    }
                }
                Event::Message(Message::Status(s)) => {
                    status += 1;
                    if status == 1 {
                        println!("status: {s:?}");
                    }
                }
                Event::Message(Message::Text(t)) => {
                    texts += 1;
                    if !t.starts_with("HB ") || texts < 2 {
                        println!("text: {t}");
                    }
                }
                Event::Message(m) => println!("other: {m:?}"),
            }
        }
        if !sent && t0.elapsed() > Duration::from_millis(1500) {
            for c in &cmds {
                println!("> {c}");
                link.send(c.clone());
            }
            sent = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("{secs} s: {frames} frames, {status} status, {texts} texts");
}
