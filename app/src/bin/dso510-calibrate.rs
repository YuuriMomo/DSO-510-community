//! DSO-510 Calibrate - sets the scope's vertical scale from a known DC voltage.
//!
//! The original firmware keeps one vertical scale in its settings (RAM 0x2000027A, saved when the scope is
//! switched off with its button): the screen shows ADC code c as 128 + (c - 128) / g with g = (value - 28) / 150,
//! so 178 means 1.0. The scope's own AUTO sets it once (when it is empty) from whatever is on the screen, which
//! can leave every range wrong by the same factor. This program measures the raw ADC codes of a known DC voltage
//! on every range where it fits (full-memory records carry the raw codes), computes g = measured / true and
//! writes the value; the screen then shows the true voltage. A zero measurement (probe tip on its ground clip)
//! first removes the ADC's zero error from the result.
#![cfg_attr(not(test), windows_subsystem = "windows")]

use dso510_link::deep::{Assembler, DeepCapture};
use dso510_link::i18n::{self, tr, trf};
use dso510_link::protocol::{px_to_volts, Message, Parser, Status, PX_PER_DIV, VDIV_VOLTS};
use dso510_link::{device, transport::Conn};
use eframe::egui::{self, Color32, RichText};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(255, 196, 0);
const GOOD: Color32 = Color32::from_rgb(70, 200, 110);
const BAD: Color32 = Color32::from_rgb(230, 70, 70);

const SCALE_ADDR: &str = "2000027A";
const SCALE_ONE: u8 = 178;
/// the voltage should fill this many divisions on a range used for the calibration
const FIT_MIN_DIV: f64 = 1.0;
const FIT_MAX_DIV: f64 = 3.6;
const RECORDS_PER_RANGE: usize = 3;

fn gain_of(scale: u8) -> f64 {
    (scale as f64 - 28.0) / 150.0
}
fn scale_of(gain: f64) -> u8 {
    (28.0 + 150.0 * gain).round().clamp(1.0, 254.0) as u8
}
/// V/div indices (probe x1) on which `volts` fills FIT_MIN_DIV..FIT_MAX_DIV divisions
fn ranges_for(volts: f64) -> Vec<u8> {
    (0..10u8).filter(|&i| (FIT_MIN_DIV..=FIT_MAX_DIV).contains(&(volts.abs() / VDIV_VOLTS[i as usize]))).collect()
}

// ---------------------------------------------------------------------------------------------------
// the scope, driven by a worker thread

enum Cmd {
    Connect,
    Zero(Vec<u8>),
    Voltage(Vec<u8>),
    Apply { scale: u8, check_range: u8 },
}

#[derive(Clone, Debug)]
struct RangeResult {
    vdiv: u8,
    /// mean raw ADC code
    code: f64,
    /// peak-to-peak of the record in divisions (a DC voltage is flat)
    spread_div: f64,
}

enum Update {
    Connected { scale: u8, info: String },
    Failed(String),
    Progress { done: usize, total: usize, what: String },
    Zero(Vec<RangeResult>),
    Voltage(Vec<RangeResult>),
    Applied { scale: u8, shown: f64 },
}

struct Dev {
    port: Conn,
    parser: Parser,
    out: Vec<Message>,
    buf: Vec<u8>,
    asm: Assembler,
    status: Option<Status>,
    texts: Vec<String>,
    deep: Vec<(Instant, DeepCapture)>,
    frames: Vec<(Instant, u8, i8, f64)>,
}

impl Dev {
    fn open() -> Result<Dev, String> {
        let p = device::find_dso().ok_or_else(|| tr("No DSO-510 found: connect it with a USB data cable and switch it on.").to_string())?;
        let port = Conn::open(&p).map_err(|e| format!("{}: {e}", tr("The scope is in use (close DSO-510 Link)")))?;
        Ok(Dev { port, parser: Parser::default(), out: Vec::new(), buf: vec![0; 65536], asm: Assembler::default(), status: None,
                 texts: Vec::new(), deep: Vec::new(), frames: Vec::new() })
    }
    fn pump(&mut self, ms: u64) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(ms) {
            let Ok(n) = self.port.read(&mut self.buf) else { continue };
            self.parser.feed(&self.buf[..n], &mut self.out);
            for m in self.out.drain(..) {
                match m {
                    Message::Status(s) => self.status = Some(s),
                    Message::Text(t) => self.texts.push(t),
                    Message::Deep(d) => self.asm.header(d),
                    Message::Raw { offset, data } => {
                        if let Some(c) = self.asm.chunk(offset, &data) {
                            self.deep.push((Instant::now(), c));
                        }
                    }
                    Message::Frame(f) => {
                        let end = (f.trace_end as usize).min(f.trace.len());
                        let s = &f.trace[(f.trace_start as usize).min(end)..end];
                        if !s.is_empty() {
                            let mean = s.iter().map(|&v| v as f64).sum::<f64>() / s.len() as f64;
                            self.frames.push((Instant::now(), f.vdiv + 3 * f.probe, f.offset, mean));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    fn cmd(&mut self, c: &str, ms: u64) -> Result<(), String> {
        self.port.write_all(format!("{c}\n").as_bytes()).map_err(|e| e.to_string())?;
        self.pump(ms);
        Ok(())
    }
    fn read_scale(&mut self) -> Result<u8, String> {
        self.texts.clear();
        self.cmd(&format!("PEEK {} 1", SCALE_ADDR), 400)?;
        let t = self.texts.iter().find(|t| t.starts_with("PEEK")).ok_or_else(|| tr("No answer to PEEK: the firmware is too old.").to_string())?;
        let hex = t.split(':').nth(1).and_then(|s| s.split_whitespace().next()).ok_or("PEEK?")?;
        u8::from_str_radix(hex, 16).map_err(|e| e.to_string())
    }
    /// the user's settings, restored after every step
    fn restore_cmds(&self) -> Vec<String> {
        let Some(s) = self.status.as_ref() else { return Vec::new() };
        vec![
            format!("SET PROBE {}", s.probe), format!("SET VDIV {}", s.vdiv), format!("SET COUPLING {}", s.coupling),
            format!("SET OFFSET {}", s.offset), format!("SET TB {}", s.timebase), format!("SET TMODE {}", s.trig_mode),
            format!("SET TLEVEL {}", s.trig_level), format!("SET RUN {}", s.running as u8),
        ]
    }
    fn setup(&mut self) -> Result<Vec<String>, String> {
        self.status = None;
        self.cmd("GET", 500)?;
        let restore = self.restore_cmds();
        if restore.is_empty() {
            return Err(tr("No status from the scope.").into());
        }
        for c in ["SET SCREEN 2", "SET PROBE 0", "SET COUPLING 1", "SET OFFSET 0", "SET TB 13", "SET TMODE 1", "SET TLEVEL 0", "SET RUN 1", "DEEP 1"] {
            self.cmd(c, 250)?;
        }
        Ok(restore)
    }
    fn finish(&mut self, restore: &[String]) {
        let _ = self.cmd("DEEP 0", 200);
        for c in restore {
            let _ = self.cmd(c, 200);
        }
    }
    /// raw ADC mean of RECORDS_PER_RANGE full-memory records on each range
    fn measure(&mut self, ranges: &[u8], tx: &Sender<Update>, ctx: &egui::Context) -> Result<Vec<RangeResult>, String> {
        let mut res = Vec::new();
        for (k, &v) in ranges.iter().enumerate() {
            let what = trf("{} V/div", &[&VDIV_VOLTS[v as usize]]);
            let _ = tx.send(Update::Progress { done: k, total: ranges.len(), what });
            ctx.request_repaint();
            self.cmd(&format!("SET VDIV {v}"), 200)?;
            let settled = Instant::now() + Duration::from_millis(1000);
            self.deep.clear();
            let t0 = Instant::now();
            let mut recs = Vec::new();
            while recs.len() < RECORDS_PER_RANGE {
                if t0.elapsed() > Duration::from_secs(10) {
                    return Err(tr("No full-memory records: is the scope showing the oscilloscope screen?").into());
                }
                self.pump(100);
                for (at, c) in self.deep.drain(..) {
                    if at > settled && c.info.vdiv == v && c.info.probe == 0 && c.info.offset == 0 && c.info.coupling == 1 {
                        recs.push(c);
                    }
                }
            }
            let (mut sum, mut n, mut lo, mut hi) = (0.0, 0.0, 255u8, 0u8);
            for c in &recs {
                let raw = c.adc.as_ref().unwrap_or(&c.codes);
                sum += raw.iter().map(|&x| x as f64).sum::<f64>();
                n += raw.len() as f64;
                lo = lo.min(*raw.iter().min().unwrap_or(&0));
                hi = hi.max(*raw.iter().max().unwrap_or(&0));
            }
            res.push(RangeResult { vdiv: v, code: sum / n.max(1.0), spread_div: (hi as f64 - lo as f64) / PX_PER_DIV });
        }
        let _ = tx.send(Update::Progress { done: ranges.len(), total: ranges.len(), what: String::new() });
        Ok(res)
    }
    /// the voltage the screen shows now (mean of the screen trace)
    fn shown_volts(&mut self, v: u8) -> Result<f64, String> {
        self.cmd(&format!("SET VDIV {v}"), 200)?;
        let settled = Instant::now() + Duration::from_millis(1000);
        self.frames.clear();
        let t0 = Instant::now();
        loop {
            self.pump(100);
            let ok: Vec<f64> = self.frames.iter().filter(|f| f.0 > settled && f.1 == v && f.2 == 0).map(|f| f.3).collect();
            if ok.len() >= 10 {
                return Ok(px_to_volts(ok.iter().sum::<f64>() / ok.len() as f64, VDIV_VOLTS[v as usize], 0));
            }
            if t0.elapsed() > Duration::from_secs(8) {
                return Err(tr("No screen frames from the scope.").into());
            }
        }
    }
}

fn worker(rx: Receiver<Cmd>, tx: Sender<Update>, ctx: egui::Context) {
    let mut dev: Option<Dev> = None;
    for cmd in rx {
        let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Update, String> {
            if matches!(cmd, Cmd::Connect) || dev.is_none() {
                dev = None;
                let mut d = Dev::open()?;
                d.cmd("GET", 500)?;
                let scale = d.read_scale()?;
                let info = d.texts.iter().find(|t| t.starts_with("DSO-510")).cloned().unwrap_or_default();
                dev = Some(d);
                if matches!(cmd, Cmd::Connect) {
                    return Ok(Update::Connected { scale, info });
                }
            }
            let d = dev.as_mut().unwrap();
            let restore = d.setup()?;
            let r = match &cmd {
                Cmd::Connect => unreachable!(),
                Cmd::Zero(ranges) => d.measure(ranges, &tx, &ctx).map(Update::Zero),
                Cmd::Voltage(ranges) => d.measure(ranges, &tx, &ctx).map(Update::Voltage),
                Cmd::Apply { scale, check_range } => (|| {
                    d.cmd(&format!("POKE8 {SCALE_ADDR} {scale}"), 300)?;
                    let now = d.read_scale()?;
                    if now != *scale {
                        return Err(tr("The scope did not take the new value.").to_string());
                    }
                    Ok(Update::Applied { scale: now, shown: d.shown_volts(*check_range)? })
                })(),
            };
            d.finish(&restore);
            r
        }));
        let r = step.unwrap_or_else(|p| {
            let m = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            Err(format!("{} {m}", tr("Internal error:")))
        });
        let u = r.unwrap_or_else(|e| {
            dev = None;                                   // reconnect next time
            Update::Failed(e)
        });
        let _ = tx.send(u);
        ctx.request_repaint();
    }
}

// ---------------------------------------------------------------------------------------------------
// the window

struct Calib {
    tx: Sender<Cmd>,
    rx: Receiver<Update>,
    lang: String,
    busy: Option<(String, Instant)>,
    progress: Option<(usize, usize, String)>,
    connected: Option<String>,
    error: Option<String>,
    scale_start: Option<u8>,
    scale_now: Option<u8>,
    volts_text: String,
    zero: Option<Vec<RangeResult>>,
    meas: Option<Vec<RangeResult>>,
    applied: Option<(u8, f64)>,
}

impl Calib {
    fn new(cc: &eframe::CreationContext) -> Self {
        let (tx, crx) = channel();
        let (utx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || worker(crx, utx, ctx));
        let lang = i18n::initial_language();
        i18n::set_language(&lang);
        setup_style(&cc.egui_ctx);
        let mut c = Calib { tx, rx, lang, busy: None, progress: None, connected: None, error: None, scale_start: None, scale_now: None,
                            volts_text: String::new(), zero: None, meas: None, applied: None };
        c.run(Cmd::Connect, tr("Connecting"));
        c
    }
    fn run(&mut self, c: Cmd, what: &str) {
        self.error = None;
        self.busy = Some((what.to_string(), Instant::now()));
        self.progress = None;
        let _ = self.tx.send(c);
    }
    fn volts(&self) -> Option<f64> {
        self.volts_text.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.abs() > 0.0)
    }
    fn poll(&mut self) {
        while let Ok(u) = self.rx.try_recv() {
            match u {
                Update::Progress { done, total, what } => {
                    self.progress = Some((done, total, what));
                    continue;
                }
                Update::Connected { scale, info } => {
                    self.connected = Some(info);
                    self.scale_start.get_or_insert(scale);
                    self.scale_now = Some(scale);
                }
                Update::Failed(e) => self.error = Some(e),
                Update::Zero(r) => self.zero = Some(r),
                Update::Voltage(r) => {
                    self.meas = Some(r);
                    self.applied = None;
                }
                Update::Applied { scale, shown } => {
                    self.scale_now = Some(scale);
                    self.applied = Some((scale, shown));
                }
            }
            self.busy = None;
            self.progress = None;
        }
    }
    /// per range: (V/div, measured raw volts, gain); and the mean gain
    fn result(&self, volts: f64) -> Option<(Vec<(f64, f64, f64)>, f64)> {
        let meas = self.meas.as_ref()?;
        let rows: Vec<(f64, f64, f64)> = meas.iter().map(|m| {
            let vpd = VDIV_VOLTS[m.vdiv as usize];
            let zero = self.zero.as_ref().and_then(|z| z.iter().find(|z| z.vdiv == m.vdiv)).map(|z| z.code).unwrap_or(128.0);
            let raw = (m.code - zero) / PX_PER_DIV * vpd;
            (vpd, raw, raw / volts)
        }).collect();
        if rows.is_empty() {
            return None;
        }
        let g = rows.iter().map(|r| r.2).sum::<f64>() / rows.len() as f64;
        Some((rows, g))
    }
}

impl eframe::App for Calib {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let busy = self.busy.is_some();
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("DSO-510 Calibrate").strong().color(ACCENT).size(18.0));
                ui.separator();
                match &self.connected {
                    Some(info) => ui.label(RichText::new(format!("● {info}")).color(GOOD)),
                    None => ui.label(RichText::new(tr("not connected")).color(BAD)),
                };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (code, name) in [("en", "English"), ("pl", "Polski")] {
                        if ui.selectable_label(self.lang == code, name).clicked() {
                            self.lang = code.into();
                            i18n::set_language(code);
                            i18n::save_setting("language", code);
                        }
                    }
                    if ui.add_enabled(!busy, egui::Button::new(tr("Reconnect"))).clicked() {
                        self.run(Cmd::Connect, tr("Connecting"));
                    }
                });
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.body(ui, busy));
        });
        if busy {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
    }
}

impl Calib {
    fn body(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.label(tr("Corrects the scope's voltage readings with one DC voltage you know. All V/div ranges share one scale factor, so one voltage calibrates them all."));
        egui::CollapsingHeader::new(RichText::new(tr("How to use")).strong()).default_open(true).show(ui, |ui| {
            ui.label(tr(CALIB_HELP));
        });
        ui.separator();

        // current state
        ui.horizontal(|ui| {
            ui.label(tr("Scale in the scope:"));
            match self.scale_now {
                Some(s) => {
                    let g = gain_of(s);
                    let col = if (g - 1.0).abs() < 0.05 { GOOD } else { ACCENT };
                    ui.label(RichText::new(trf("{} (factor {}: the screen shows {} x the input)", &[&s, &format!("{g:.3}"), &format!("{:.3}", 1.0 / g)])).color(col).monospace());
                }
                None => {
                    ui.label("-");
                }
            }
        });
        if let Some(s) = self.scale_start.filter(|&s| s == 0 || s == 255) {
            ui.colored_label(ACCENT, trf("The scope has no scale stored ({}): its AUTO button would measure one from the screen. Calibrate it here instead.", &[&s]));
        }
        ui.add_space(6.0);

        // the known voltage
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Known voltage")).strong());
            ui.add(egui::TextEdit::singleline(&mut self.volts_text).desired_width(90.0).hint_text("1.58"));
            ui.label("V");
        });
        let volts = self.volts();
        let ranges = volts.map(ranges_for).unwrap_or_default();
        match volts {
            None => {
                ui.weak(tr("Enter the voltage measured with a multimeter (a negative value for a negative voltage)."));
            }
            Some(_) if ranges.is_empty() => {
                ui.colored_label(BAD, tr("This voltage fits no range (0.01 V ... about 36 V with the probe at x1)."));
            }
            Some(_) => {
                let list: Vec<String> = ranges.iter().map(|&r| format!("{} V/div", VDIV_VOLTS[r as usize])).collect();
                ui.weak(trf("Measured on: {}", &[&list.join(", ")]));
            }
        }
        let can = self.connected.is_some() && !busy && !ranges.is_empty();
        ui.add_space(6.0);

        // step 1
        ui.horizontal(|ui| {
            if ui.add_enabled(can, egui::Button::new(tr("1. Measure zero"))).on_hover_text(tr("Probe tip clipped to its own ground clip (0 V)")).clicked() {
                self.zero = None;
                self.run(Cmd::Zero(ranges.clone()), tr("Measuring zero"));
            }
            match &self.zero {
                Some(z) => {
                    let mv: Vec<String> = z.iter().map(|r| format!("{:+.0} mV", (r.code - 128.0) / PX_PER_DIV * VDIV_VOLTS[r.vdiv as usize] * 1000.0)).collect();
                    ui.label(RichText::new(trf("zero error: {}", &[&mv.join(", ")])).monospace());
                }
                None => {
                    ui.weak(tr("optional, makes the result more accurate"));
                }
            }
        });
        // step 2
        ui.horizontal(|ui| {
            if ui.add_enabled(can, egui::Button::new(tr("2. Measure the voltage"))).on_hover_text(tr("Probe tip on +, ground clip on - of the known voltage")).clicked() {
                self.run(Cmd::Voltage(ranges.clone()), tr("Measuring the voltage"));
            }
        });

        // progress
        if let Some((what, t0)) = &self.busy {
            let (frac, text) = match &self.progress {
                Some((d, t, w)) if *t > 0 => {
                    let left = if *d > 0 { t0.elapsed().as_secs_f64() / *d as f64 * (*t - *d) as f64 } else { *t as f64 * 2.5 };
                    (*d as f32 / *t as f32, trf("{}: {} ({}/{}), about {} s left", &[what, w, &(d + 1).min(*t), t, &format!("{left:.0}")]))
                }
                _ => (0.0, format!("{what}...")),
            };
            ui.add(egui::ProgressBar::new(frac).text(text).animate(true));
        }
        if let Some(e) = &self.error {
            ui.colored_label(BAD, e);
        }

        // result
        if let (Some(v), Some((rows, g))) = (volts, self.result(volts.unwrap_or(1.0))) {
            ui.separator();
            ui.label(RichText::new(tr("Result")).strong());
            egui::Grid::new("rows").striped(true).num_columns(4).show(ui, |ui| {
                ui.strong(tr("V/div"));
                ui.strong(tr("ADC reads"));
                ui.strong(tr("the screen showed"));
                ui.strong(tr("after calibration"));
                ui.end_row();
                let shown_g = self.scale_now.map(gain_of).unwrap_or(1.0);
                let new_g = gain_of(scale_of(g));
                for (vpd, raw, _) in &rows {
                    ui.monospace(format!("{vpd} V"));
                    ui.monospace(format!("{raw:.3} V"));
                    ui.monospace(format!("{:.3} V", raw / shown_g));
                    ui.monospace(format!("{:.3} V", raw / new_g));
                    ui.end_row();
                }
            });
            let warn_spread = self.meas.as_ref().is_some_and(|m| m.iter().any(|r| r.spread_div > 0.6));
            let ratio = g;
            let new_scale = scale_of(g);
            ui.add_space(4.0);
            if warn_spread {
                ui.colored_label(ACCENT, tr("The signal is not steady (it moves by more than half a division): use a DC voltage, not the generator, and check the contacts."));
            }
            if !(0.8..=1.25).contains(&ratio) {
                let hint = if (0.08..=0.125).contains(&ratio) {
                    tr("The ADC reads about a tenth: the switch on the probe is at x10, set it to x1.")
                } else if ratio.abs() < 0.3 {
                    tr("The ADC reads almost nothing: the probe is not connected to the voltage.")
                } else if ratio < 0.0 {
                    tr("The ADC reads the opposite sign: swap the probe tip and the ground clip, or enter a negative voltage.")
                } else {
                    tr("The difference is too large for a calibration error: check the voltage you entered and the connection.")
                };
                ui.colored_label(BAD, trf("Measured {} x the entered voltage. {}", &[&format!("{ratio:.3}"), &hint]));
            } else {
                ui.label(trf("Measured {} x the entered voltage: new scale {} (factor {}).", &[&format!("{ratio:.4}"), &new_scale, &format!("{:.4}", gain_of(new_scale))]));
                let check = rows.iter().map(|r| r.0).zip(self.meas.as_ref().unwrap().iter().map(|m| m.vdiv))
                    .min_by(|a, b| (v.abs() / a.0 - 2.5).abs().total_cmp(&(v.abs() / b.0 - 2.5).abs())).map(|x| x.1).unwrap_or(5);
                ui.horizontal(|ui| {
                    let en = self.connected.is_some() && !busy;
                    if ui.add_enabled(en, egui::Button::new(RichText::new(tr("Apply")).strong())).clicked() {
                        self.run(Cmd::Apply { scale: new_scale, check_range: check }, tr("Writing"));
                    }
                });
            }
        }

        if let Some((s, shown)) = self.applied {
            ui.add_space(4.0);
            let v = volts.unwrap_or(0.0);
            let err = if v != 0.0 { (shown / v - 1.0) * 100.0 } else { 0.0 };
            ui.colored_label(if err.abs() < 3.0 { GOOD } else { ACCENT },
                trf("Written: scale {}. The screen now shows {} V ({}% against the known voltage).", &[&s, &format!("{shown:.3}"), &format!("{err:+.1}")]));
            ui.label(RichText::new(tr("Switch the scope off with its power button now: it saves the scale in its settings then. Unplugging or a reset does not save it.")).strong().color(ACCENT));
        }

        // undo
        ui.separator();
        ui.horizontal(|ui| {
            let en = self.connected.is_some() && !busy;
            if let Some(s0) = self.scale_start {
                if ui.add_enabled(en && self.scale_now != Some(s0) && s0 != 0 && s0 != 255, egui::Button::new(trf("Restore the value read at start ({})", &[&s0]))).clicked() {
                    self.run(Cmd::Apply { scale: s0, check_range: 6 }, tr("Writing"));
                }
            }
            if ui.add_enabled(en && self.scale_now != Some(SCALE_ONE), egui::Button::new(trf("Set factor 1.0 ({})", &[&SCALE_ONE])))
                .on_hover_text(tr("No correction: the screen shows the ADC as it is (accurate to a few percent on most units)")).clicked() {
                self.run(Cmd::Apply { scale: SCALE_ONE, check_range: 6 }, tr("Writing"));
            }
        });
    }
}

const CALIB_HELP: &str = "You need: a DC voltage you know, measured with a multimeter (a battery, 3.3 V or 5 V from a board or a USB port, a lab supply). Best between 1 and 10 V; with the probe at x1 up to about 36 V. Not the scope's own generator: it is not a calibrated reference.\n\n1. Close DSO-510 Link (only one program can use the scope), connect the scope with USB and switch it on.\n2. Set the switch on the probe to x1.\n3. Enter the voltage you measured with the multimeter.\n4. Step 1 (optional, more accurate): clip the probe tip to its own ground clip and press \"1. Measure zero\".\n5. Step 2: probe tip on the plus, ground clip on the minus of the voltage, press \"2. Measure the voltage\". The program sets the scope itself (DC, probe x1, Auto, several V/div ranges) and puts your settings back afterwards.\n6. Check the table and press \"Apply\". The program writes the new scale and reads the screen again.\n7. Switch the scope off with its power button: only then the scale is saved in its settings.\n\nUndo: \"Restore the value read at start\" or \"Set factor 1.0\", then switch off with the button again.";

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(22, 24, 28);
    v.window_fill = Color32::from_rgb(26, 28, 33);
    v.extreme_bg_color = Color32::from_rgb(14, 15, 18);
    v.selection.bg_fill = Color32::from_rgb(160, 120, 0);
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        s.visuals = v.clone();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
    });
}

/// `--selftest <volts> [file]`: connect, measure zero and the voltage without the window, write every update
/// to `file` (default dso510-calibrate-test.txt). For testing on the device.
fn selftest(volts: f64, file: &str) {
    use std::io::Write;
    let mut f = std::fs::File::create(file).expect("log file");
    let (tx, crx) = channel();
    let (utx, rx) = channel();
    std::thread::spawn(move || worker(crx, utx, egui::Context::default()));
    let ranges = ranges_for(volts);
    let _ = writeln!(f, "ranges {ranges:?}");
    for c in [Cmd::Connect, Cmd::Zero(ranges.clone()), Cmd::Voltage(ranges)] {
        tx.send(c).unwrap();
        let t0 = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_secs(40)) {
                Ok(Update::Progress { done, total, what }) => { let _ = writeln!(f, "{:6.1} s progress {done}/{total} {what}", t0.elapsed().as_secs_f64()); }
                Ok(Update::Connected { scale, info }) => { let _ = writeln!(f, "connected {info} scale {scale}"); break; }
                Ok(Update::Failed(e)) => { let _ = writeln!(f, "FAILED {e}"); break; }
                Ok(Update::Zero(r)) => { let _ = writeln!(f, "{:6.1} s zero {r:?}", t0.elapsed().as_secs_f64()); break; }
                Ok(Update::Voltage(r)) => { let _ = writeln!(f, "{:6.1} s voltage {r:?}", t0.elapsed().as_secs_f64()); break; }
                Ok(Update::Applied { scale, shown }) => { let _ = writeln!(f, "applied {scale} {shown}"); break; }
                Err(_) => { let _ = writeln!(f, "TIMEOUT: no answer in 40 s"); return; }
            }
            let _ = f.flush();
        }
        let _ = f.flush();
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--selftest") {
        let v = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1.58);
        selftest(v, args.get(3).map(|s| s.as_str()).unwrap_or("dso510-calibrate-test.txt"));
        return Ok(());
    }
    dso510_link::crashlog::install();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([760.0, 820.0]).with_min_inner_size([560.0, 480.0]).with_title("DSO-510 Calibrate"),
        ..Default::default()
    };
    eframe::run_native("DSO-510 Calibrate", options, Box::new(|cc| Ok(Box::new(Calib::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_and_gain() {
        assert_eq!(gain_of(178), 1.0);
        assert_eq!(scale_of(1.0), 178);
        assert_eq!(scale_of(gain_of(128)), 128);
        // the case seen: the screen showed 2.40 V for 1.58 V with scale 128; ADC 1.60 V -> scale 180
        assert_eq!(scale_of(1.60 / 1.58), 180);
    }

    #[test]
    fn ranges() {
        assert_eq!(ranges_for(1.58), vec![5, 6]);                    // 0.5 V (3.2 div), 1 V (1.6 div)
        assert_eq!(ranges_for(-5.0), vec![7, 8]);
        assert!(ranges_for(100.0).is_empty());
    }
}
