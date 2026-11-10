//! Hardware self-test of the v2 remote control: sends every SET command, waits for the reply and
//! checks the status message, then restores the original settings.
//!   cargo run --release --example selftest
use dso510_link::{link::{Event, Link}, protocol::{Message, Status}};
use std::time::{Duration, Instant};

struct T {
    link: Link,
    status: Option<Status>,
    frames: u32,
    texts: Vec<String>,
    raw: usize,
    pass: u32,
    fail: u32,
}

impl T {
    fn pump(&mut self, d: Duration) {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            if let Ok(e) = self.link.events.recv_timeout(Duration::from_millis(10)) {
                match e {
                    Event::Message(Message::Status(s)) => self.status = Some(s),
                    Event::Message(Message::Frame(_)) => self.frames += 1,
                    Event::Message(Message::Text(t)) => self.texts.push(t),
                    Event::Message(Message::Raw { data, .. }) => self.raw += data.len(),
                    Event::Disconnected(w) => println!("  !! disconnected: {w}"),
                    _ => {}
                }
            }
        }
    }
    /// send a command, wait for the status that follows, check it
    fn check(&mut self, cmd: &str, what: &str, ok: impl Fn(&Status) -> bool) {
        self.texts.clear();
        self.status = None;
        self.link.send(cmd);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            self.pump(Duration::from_millis(50));
            if self.status.as_ref().is_some_and(&ok) {
                break;
            }
        }
        let reply = self.texts.iter().find(|t| t.starts_with("OK") || t.starts_with("ERR") || t.starts_with("BUSY")).cloned().unwrap_or_else(|| "(no reply)".into());
        let good = self.status.as_ref().is_some_and(&ok);
        if good { self.pass += 1 } else { self.fail += 1 }
        println!("{} {cmd:<16} -> {reply:<22} {what}", if good { "PASS" } else { "FAIL" });
    }
}

fn main() {
    let mut t = T { link: Link::spawn(|| {}), status: None, frames: 0, texts: vec![], raw: 0, pass: 0, fail: 0 };
    t.pump(Duration::from_secs(2));
    t.link.send("GET");
    t.pump(Duration::from_millis(500));
    let orig = t.status.clone().expect("no status - is firmware v2 running?");
    println!("original: {orig:?}\n");

    t.check("SET SCREEN 2", "scope screen", |s| s.app_mode == 2);
    t.check("SET VDIV 6", "1 V/div", |s| s.vdiv == 6);
    t.check("SET VDIV 3", "100 mV/div", |s| s.vdiv == 3);
    t.check("SET TB 13", "1 ms/div", |s| s.timebase == 13);
    t.check("SET TB 7", "10 us/div", |s| s.timebase == 7);
    t.check("SET TLEVEL 20", "trigger +20", |s| s.trig_level == 20);
    t.check("SET OFFSET 10", "offset +10, trigger follows", |s| s.offset == 10 && s.trig_level == 30);
    t.check("SET OFFSET 0", "offset 0", |s| s.offset == 0);
    t.check("SET TLEVEL 0", "trigger 0", |s| s.trig_level == 0);
    t.check("SET HPOS 20", "position 20", |s| s.hpos == 20);
    t.check("SET HPOS 0", "position 0", |s| s.hpos == 0);
    t.check("SET COUPLING 0", "AC", |s| s.coupling == 0);
    t.check("SET COUPLING 1", "DC", |s| s.coupling == 1);
    t.check("SET PROBE 1", "x10", |s| s.probe == 1);
    t.check("SET PROBE 0", "x1", |s| s.probe == 0);
    t.check("SET TMODE 0", "Normal", |s| s.trig_mode == 0);
    t.check("SET TMODE 1", "Auto", |s| s.trig_mode == 1);
    t.check("SET TEDGE 0", "rising", |s| s.trig_edge == 0);
    t.check("SET TEDGE 1", "falling", |s| s.trig_edge == 1);
    t.check("SET RUN 0", "stopped", |s| !s.running);
    t.check("SET RUN 1", "running", |s| s.running);
    t.check("SET ADJ 2", "arrows: trigger", |s| s.adjust_target == 2);
    t.check("SET ADJ 0", "arrows: V/div", |s| s.adjust_target == 0);
    t.check("SET BEEP 3", "beep 3", |s| s.beep == 3);
    t.check("SET LIGHT 60", "backlight 60", |s| s.light == 60);
    t.check("SET AUTOOFF 2", "auto-off 30 min", |s| s.auto_off == 2);
    t.check("SET GWAVE 0", "sine", |s| s.gen_wave == 0);
    t.check("SET GFREQ 500", "500 Hz", |s| s.gen_freq == 500);
    t.check("SET GFREQ 5000", "5 kHz also on the scope screen (fw v4.3: no 1 kHz limit)", |s| s.gen_freq == 5000);
    t.check("SET GDUTY 30", "duty 30", |s| s.gen_duty == 30);
    t.check("SET GAMP 15", "1.5 V", |s| s.gen_ampl == 15);
    t.check("SET GOUT 0", "ERR SCREEN expected (scope screen)", |s| s.gen_out == orig.gen_out);
    t.check("SET SCREEN 3", "generator screen", |s| s.app_mode == 3);
    t.check("SET GFREQ 20000", "20 kHz on generator screen", |s| s.gen_freq == 20000);
    t.check("SET GOUT 0", "output off", |s| !s.gen_out);
    t.check("SET GOUT 1", "output on", |s| s.gen_out);
    t.check("SET SCREEN 4", "settings screen", |s| s.app_mode == 4);
    t.check("SET SCREEN 0", "main menu", |s| s.app_mode == 0);
    t.check("SET SCREEN 2", "scope screen", |s| s.app_mode == 2);
    t.check("SET VDIV 9", "ERR-free large step", |s| s.vdiv == 9);

    // raw buffer
    t.link.send("SET RUN 0");
    t.pump(Duration::from_millis(500));
    t.raw = 0;
    t.link.send("RAW");
    t.pump(Duration::from_secs(3));
    println!("{} RAW              -> {} bytes", if t.raw == 30100 { "PASS" } else { "FAIL" }, t.raw);
    if t.raw == 30100 { t.pass += 1 } else { t.fail += 1 }
    t.link.send("SET RUN 1");

    // restore
    println!("\nrestoring original settings…");
    for c in [
        format!("SET TB {}", orig.timebase), format!("SET VDIV {}", orig.vdiv), format!("SET COUPLING {}", orig.coupling),
        format!("SET PROBE {}", orig.probe), format!("SET TMODE {}", orig.trig_mode), format!("SET TEDGE {}", orig.trig_edge),
        format!("SET OFFSET {}", orig.offset), format!("SET TLEVEL {}", orig.trig_level), format!("SET HPOS {}", orig.hpos),
        format!("SET ADJ {}", orig.adjust_target), format!("SET BEEP {}", orig.beep), format!("SET LIGHT {}", orig.light),
        format!("SET AUTOOFF {}", orig.auto_off), format!("SET GWAVE {}", orig.gen_wave), format!("SET GDUTY {}", orig.gen_duty),
        format!("SET GAMP {}", orig.gen_ampl),
    ] {
        t.link.send(c);
        t.pump(Duration::from_millis(150));
    }
    t.link.send("SET SCREEN 3");
    t.pump(Duration::from_millis(400));
    t.link.send(format!("SET GFREQ {}", orig.gen_freq));
    t.pump(Duration::from_millis(200));
    t.link.send(format!("SET GOUT {}", orig.gen_out as u8));
    t.pump(Duration::from_millis(300));
    t.link.send(format!("SET SCREEN {}", if orig.app_mode == 9 { 2 } else { orig.app_mode }));
    t.pump(Duration::from_millis(800));
    t.link.send("GET");
    t.pump(Duration::from_millis(600));
    println!("final:    {:?}", t.status);
    println!("\n{} passed, {} failed, {} frames received during the test", t.pass, t.fail, t.frames);
}
