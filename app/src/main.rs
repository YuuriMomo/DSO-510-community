//! DSO-510 Link - live view, measurements, history and full remote control of the DSO-510
//! oscilloscope / signal generator over USB.
//!
//! The scope is found automatically (USB VID 2E3C / PID 5740) and opened through its device
//! interface path, so COM-port numbering conflicts do not matter. Protocol: docs/usb_link.md,
//! firmware side: fw/src/usb_link.c and fw/src/remote.c.
#![cfg_attr(not(test), windows_subsystem = "windows")]

use dso510_link::{bode, crashlog, deep, deep_view, device, export, i18n, link, measure, protocol, scope, spectrum, spectrum_view};
use i18n::{tr, trf};
use eframe::egui::{self, Color32, RichText};
use link::{Event, Link};
use measure::{eng, Measurements};
use protocol::{Frame, Message, Status, RAW_SAMPLES};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const HISTORY_MAX: usize = 5000;
const DEEP_HISTORY_MAX: usize = 200;

/// Running min / mean / max of one measurement over many acquisitions.
#[derive(Clone, Copy, Default)]
struct Stat {
    n: u32,
    min: f64,
    max: f64,
    sum: f64,
}

impl Stat {
    fn add(&mut self, x: Option<f64>) {
        let Some(x) = x.filter(|x| x.is_finite()) else { return };
        if self.n == 0 {
            self.min = x;
            self.max = x;
        } else {
            self.min = self.min.min(x);
            self.max = self.max.max(x);
        }
        self.sum += x;
        self.n += 1;
    }
    fn mean(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / self.n as f64)
    }
}

/// (label, unit, value) of the measurements kept in the statistics
fn stat_values(m: &Measurements) -> [(&'static str, &'static str, Option<f64>); 10] {
    [
        ("Vpp", "V", Some(m.vpp)),
        ("Vrms", "V", Some(m.vrms)),
        ("Vavg", "V", Some(m.vavg)),
        ("Freq", "Hz", m.freq),
        ("Period", "s", m.period),
        ("Duty", "%", m.duty),
        ("Rise", "s", m.rise),
        ("Fall", "s", m.fall),
        ("+Width", "s", m.pwidth),
        ("-Width", "s", m.nwidth),
    ]
}
const ACCENT: Color32 = Color32::from_rgb(255, 196, 0);
const GOOD: Color32 = Color32::from_rgb(70, 200, 110);
const BAD: Color32 = Color32::from_rgb(230, 70, 70);

/// Front panel as key codes understood by the firmware ("KEY n").
const PANEL: &[(&str, u8, &str)] = &[
    ("Run/Stop", 2, "button 1: Run/Stop, generator output, delete"),
    ("▲", 3, "button 2: up / +"),
    ("▼", 4, "button 3: down / -"),
    ("Adjust", 5, "button 4: what the arrows adjust / level up"),
    ("AUTO", 6, "button 5: AUTO"),
    ("▶ Next", 7, "button 6: next / right"),
    ("OK", 8, "button 7: OK / enter"),
    ("◀ Prev", 9, "button 8: previous / left"),
    ("Save shot", 11, "button 1 long: save screenshot on the scope"),
    ("Menu", 12, "button 4 long: side menu"),
    ("Auto B", 13, "button 5 long"),
    ("Home", 14, "button 7 long: main menu"),
];

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Screen,
    Deep,
    /// frequency response with the built-in generator (bode.rs)
    Bode,
}

struct ShotTour {
    dir: std::path::PathBuf,
    step: usize,
    since: Instant,
    waiting_shot: Option<String>,
    shot_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

const BODE_HELP: &str = "What it shows: how much of a sine your circuit lets through at each frequency (gain in dB: 0 dB unchanged, -3 dB the cutoff, -6 dB half the amplitude, +6 dB twice). Only one channel, so no phase.\n\nConnections:\n1. Generator output of the scope -> input of your circuit.\n2. Ground of the probe and of the generator -> ground of the circuit.\n3. Probe x1 (set the probe switch and the scope to X1).\n\nSteps:\n1. Probe on the circuit INPUT (or directly on the generator output) and press \"1. Reference\". It measures what the generator really delivers at every frequency, which also cancels the generator's and the scope's own response.\n2. Move the probe to the circuit OUTPUT and press \"2. Measure\". The plot shows the gain, output divided by reference.\n3. Repeat 2 as often as you like (e.g. after changing a part); the reference stays valid while the range, points and generator voltage are unchanged.\n\nThe app sets the generator (sine), the time base and the V/div by itself for every point; the scope screen changes during the sweep, that is normal. A point takes about 1.5-2 s.\n\nTips: range 10 Hz - 50 kHz (generator limit). For a high-pass or an AC-coupled circuit use DC coupling on the scope. If the output is very small (deep stopband) the readings below about -40 dB are noise. Dashed orange lines: maximum - 3 dB and the -3 dB frequencies. CSV saves all points.";

#[derive(Clone, Copy, PartialEq)]
enum ConnState {
    Searching,
    Connected,
    Off,
}

/// Values edited in the UI; sent with a small delay so dragging a slider does not flood the scope.
#[derive(Default)]
struct Pending {
    items: Vec<(String, i32, Instant)>,
}

impl Pending {
    fn set(&mut self, name: &str, v: i32) {
        self.items.retain(|(n, _, _)| n != name);
        self.items.push((name.to_string(), v, Instant::now()));
    }
    fn due(&mut self, delay: Duration) -> Vec<(String, i32)> {
        let now = Instant::now();
        let (due, keep): (Vec<_>, Vec<_>) = self.items.drain(..).partition(|(_, _, t)| now.duration_since(*t) >= delay);
        self.items = keep;
        due.into_iter().map(|(n, v, _)| (n, v)).collect()
    }
    fn is_pending(&self, name: &str) -> bool {
        self.items.iter().any(|(n, _, _)| n == name)
    }
}

struct App {
    link: Link,
    conn: ConnState,
    port_label: String,
    ports: Vec<device::PortChoice>,
    fw_info: String,
    fw_version: u8,
    status: Option<Status>,
    frame: Option<Frame>,
    meas: Measurements,
    history: VecDeque<Frame>,
    view: Option<usize>,
    persistence: usize,
    cursors: scope::Cursors,
    scope: scope::ScopeView,
    pending: Pending,
    ui_offset: i32,
    ui_tlevel: i32,
    ui_hpos: i32,
    ui_gfreq: i32,
    ui_gduty: i32,
    ui_gamp: f32,
    ui_beep: i32,
    ui_light: i32,
    frames: u64,
    fps: f32,
    fps_t0: Instant,
    fps_n: u32,
    raw: Vec<u8>,
    raw_received: usize,
    log: VecDeque<String>,
    toast: Option<(String, Instant)>,
    mode: ViewMode,
    deep_asm: deep::Assembler,
    deep_history: VecDeque<deep::DeepCapture>,
    deep_idx: Option<usize>,
    deep_view: deep_view::DeepView,
    deep_streaming: bool,
    deep_visible: (usize, usize),
    deep_meas_key: Option<(u16, usize, usize, usize)>,
    deep_n: u32,
    deep_rate: f32,
    roll: deep::RollStream,
    show_fft: bool,
    fft_window: spectrum::Window,
    spec: Option<spectrum::Spectrum>,
    spec_key: Option<(u8, u64, usize, usize, u8)>,
    spec_view: spectrum_view::SpectrumView,
    bode: bode::Bode,
    /// DSO510_SHOTS=<dir>: screenshot tour for the documentation (English UI)
    tour: Option<ShotTour>,
    show_stats: bool,
    stats: [Stat; 10],
    stats_id: Option<u64>,
    stats_ctx: Option<(u8, u8, u8, u8)>,
    lang: String,
    languages: Vec<i18n::Language>,
    /// the previous session ended abnormally (see crash.log)
    crash_banner: bool,
    /// last frame whose acquisition was triggered (firmware v4.18)
    last_trig: Option<Instant>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let ctx = cc.egui_ctx.clone();
        let repaint = ctx.clone();
        Self::with_link(ctx, Link::spawn(move || repaint.request_repaint()))
    }

    fn with_link(ctx: egui::Context, link: Link) -> Self {
        setup_style(&ctx);
        let lang = i18n::initial_language();
        i18n::set_language(&lang);
        Self {
            link,
            conn: ConnState::Searching,
            port_label: String::new(),
            ports: device::list(),
            fw_info: String::new(),
            fw_version: 0,
            status: None,
            frame: None,
            meas: Measurements::default(),
            history: VecDeque::new(),
            view: None,
            persistence: 0,
            cursors: scope::Cursors::default(),
            scope: scope::ScopeView::default(),
            pending: Pending::default(),
            ui_offset: 0,
            ui_tlevel: 0,
            ui_hpos: 0,
            ui_gfreq: 1000,
            ui_gduty: 50,
            ui_gamp: 1.0,
            ui_beep: 5,
            ui_light: 80,
            frames: 0,
            fps: 0.0,
            fps_t0: Instant::now(),
            fps_n: 0,
            raw: vec![0; RAW_SAMPLES],
            raw_received: 0,
            log: VecDeque::new(),
            toast: None,
            mode: ViewMode::Screen,
            deep_asm: deep::Assembler::default(),
            deep_history: VecDeque::new(),
            deep_idx: None,
            deep_view: deep_view::DeepView::default(),
            deep_streaming: false,
            deep_visible: (0, 0),
            deep_meas_key: None,
            deep_n: 0,
            deep_rate: 0.0,
            roll: deep::RollStream::default(),
            show_fft: false,
            fft_window: spectrum::Window::Hann,
            spec: None,
            spec_key: None,
            spec_view: spectrum_view::SpectrumView::default(),
            bode: bode::Bode::default(),
            tour: std::env::var_os("DSO510_SHOTS").map(|d| ShotTour { dir: d.into(), step: 0, since: Instant::now(), waiting_shot: None, shot_done: Default::default() }),
            show_stats: false,
            stats: [Stat::default(); 10],
            stats_id: None,
            stats_ctx: None,
            lang,
            languages: i18n::available(),
            crash_banner: false,
            last_trig: None,
        }
    }

    fn v2(&self) -> bool {
        self.fw_version >= 2
    }

    /// firmware with deep memory streaming (usb_link v3)
    fn has_deep(&self) -> bool {
        self.fw_version >= 3
    }

    /// firmware that streams the roll mode (usb_link v4)
    fn has_roll(&self) -> bool {
        self.fw_version >= 4
    }

    /// The scope runs in roll mode (50 ms/div and slower, Auto / Normal): there is a stream instead of
    /// records. Single still captures one triggered record at these time bases.
    fn roll_mode(&self) -> bool {
        let tb = self.status.as_ref().map(|s| s.timebase).or(self.frame.as_ref().map(|f| f.timebase)).unwrap_or(0);
        let single = self.status.as_ref().map(|s| s.trig_mode).or(self.frame.as_ref().map(|f| f.trig_mode)).unwrap_or(0) == 2;
        tb >= 18 && !single
    }

    fn shown_deep(&self) -> Option<&deep::DeepCapture> {
        pick_deep(self.roll_mode(), &self.roll, &self.deep_history, self.deep_idx)
    }

    /// keep the device's streaming state in line with the selected view
    fn sync_deep_streaming(&mut self) {
        let can = self.conn == ConnState::Connected && self.has_deep();
        let want = can && self.mode == ViewMode::Deep;
        if can && want != self.deep_streaming {
            self.send(if want { "DEEP 1" } else { "DEEP 0" });
        }
        self.deep_streaming = want;
    }

    fn log(&mut self, s: impl Into<String>) {
        let s = s.into();
        crashlog::note(&s);
        self.log.push_back(s);
        while self.log.len() > 300 {
            self.log.pop_front();
        }
    }

    fn notify(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.log(s.clone());
        self.toast = Some((s, Instant::now()));
    }

    fn send(&mut self, cmd: &str) {
        if self.conn == ConnState::Connected {
            self.link.send(cmd);
            self.log(format!("> {cmd}"));
        }
    }

    fn set(&mut self, name: &str, v: i32) {
        self.send(&format!("SET {name} {v}"));
    }

    /// The device status, or (firmware v1) the settings carried by the latest frame.
    fn effective_status(&self) -> Option<Status> {
        if let Some(s) = &self.status {
            return Some(s.clone());
        }
        self.frame.as_ref().map(|f| Status {
            version: 1,
            app_mode: f.app_mode,
            timebase: f.timebase,
            vdiv: f.vdiv,
            probe: f.probe,
            coupling: f.coupling,
            trig_mode: f.trig_mode,
            trig_edge: f.trig_edge,
            running: f.running,
            trig_level: f.trig_level,
            offset: f.offset,
            freq_raw: f.freq_raw,
            ..Default::default()
        })
    }

    /// add the current measurements once per new live acquisition; reset when the settings change
    fn update_stats(&mut self) {
        let (id, ctx) = if self.mode == ViewMode::Deep {
            if self.roll_mode() {
                return;
            }
            let Some(c) = self.deep_history.back().filter(|_| self.deep_idx.is_none()) else { return };
            (1u64 << 40 | c.info.seq as u64 | (self.deep_history.len() as u64) << 16, (1u8, c.info.timebase_applied, c.info.vdiv, c.info.probe))
        } else {
            let Some(f) = self.frame.as_ref().filter(|_| self.view.is_none()) else { return };
            (f.seq as u64 | self.frames << 16, (0u8, f.timebase, f.vdiv, f.probe))
        };
        if self.stats_ctx != Some(ctx) {
            self.stats = [Stat::default(); 10];
            self.stats_ctx = Some(ctx);
        }
        if self.stats_id != Some(id) {
            self.stats_id = Some(id);
            for (st, (_, _, v)) in self.stats.iter_mut().zip(stat_values(&self.meas)) {
                st.add(v);
            }
        }
    }

    /// spectrum of the deep record (visible window) or of the screen trace
    fn update_spectrum(&mut self) {
        if !self.show_fft {
            return;
        }
        let w = self.fft_window as u8;
        if self.mode == ViewMode::Deep {
            let Some(c) = self.shown_deep() else { return };
            let (a, b) = visible_range(self.deep_visible, c.len(), 16);
            let key = (1u8, c.info.seq as u64 | (self.deep_history.len() as u64) << 16 | (c.len() as u64) << 32, a, b, w);
            if self.spec_key != Some(key) {
                let v: Vec<f64> = c.codes[a..b].iter().map(|&x| c.volts(x)).collect();
                self.spec = Some(spectrum::compute(&v, c.dt, self.fft_window));
                self.spec_key = Some(key);
            }
        } else if let Some(f) = self.shown_frame() {
            let key = (0u8, f.seq as u64 | self.frames << 16 | (self.view.unwrap_or(usize::MAX) as u64) << 40, 0, 0, w);
            if self.spec_key != Some(key) {
                let v: Vec<f64> = f.samples().iter().map(|p| p[1]).collect();
                self.spec = Some(spectrum::compute(&v, f.seconds_per_px(), self.fft_window));
                self.spec_key = Some(key);
            }
        }
    }

    fn shown_frame(&self) -> Option<&Frame> {
        match self.view {
            Some(i) => self.history.get(i),
            None => self.frame.as_ref(),
        }
    }

    fn poll(&mut self) {
        let mut events = Vec::new();
        while let Ok(e) = self.link.events.try_recv() {
            events.push(e);
        }
        for e in events {
            match e {
                Event::Connected(p) => {
                    self.conn = ConnState::Connected;
                    self.port_label = p.label.clone();
                    self.fw_version = 0;
                    self.fw_info.clear();
                    self.deep_streaming = false;
                    self.deep_asm = deep::Assembler::default();
                    self.notify(trf("Connected: {}", &[&p.label]));
                }
                Event::Disconnected(why) => {
                    if self.conn == ConnState::Connected {
                        self.notify(trf("Disconnected: {}", &[&why]));
                    }
                    self.conn = if why.contains("by user") { ConnState::Off } else { ConnState::Searching };
                    self.status = None;
                    self.deep_streaming = false;
                }
                Event::WriteError(e) => self.log(format!("write failed: {e}")),
                Event::Message(Message::Frame(f)) => {
                    self.frames += 1;
                    self.fps_n += 1;
                    if self.history.len() >= HISTORY_MAX {
                        self.history.pop_front();
                        if let Some(i) = self.view.as_mut() {
                            *i = i.saturating_sub(1);
                        }
                    }
                    if f.triggered() == Some(true) {
                        self.last_trig = Some(Instant::now());
                    }
                    self.history.push_back(f.clone());
                    self.frame = Some(f);
                    if self.status.is_none() {
                        if let Some(s) = self.effective_status() {
                            self.sync_controls(&s);
                        }
                    }
                }
                Event::Message(Message::Status(s)) => {
                    if self.fw_version < s.version {
                        self.fw_version = s.version;
                    }
                    self.sync_controls(&s);
                    self.status = Some(s);
                }
                Event::Message(Message::Text(t)) => {
                    if t.contains("usb_link v") {
                        let v = t.rsplit('v').next().and_then(|v| v.trim().parse().ok()).unwrap_or(1);
                        self.fw_version = self.fw_version.max(v);
                        self.fw_info = t.clone();
                    }
                    if t.starts_with("ERR SCREEN") {
                        self.notify(tr("The scope must show the matching screen for this setting"));
                    } else if t.starts_with("BUSY") {
                        self.notify(tr("Scope busy (AUTO running) - try again"));
                    }
                    if !t.starts_with("HB ") {
                        self.log(format!("< {t}"));
                    }
                }
                Event::Message(Message::Deep(d)) => self.deep_asm.header(d),
                Event::Message(Message::Roll(c)) => {
                    if self.roll.ingest(c) {
                        self.deep_view.restart_follow();        // a new record: follow it live
                        self.deep_meas_key = None;
                    }
                }
                Event::Message(Message::Raw { offset, data }) if self.deep_asm.in_progress() => {
                    if let Some(c) = self.deep_asm.chunk(offset, &data) {
                        if c.untriggered_normal() {
                            continue;                       // Normal: keep the last triggered record
                        }
                        self.deep_n += 1;
                        if self.deep_history.len() >= DEEP_HISTORY_MAX {
                            self.deep_history.pop_front();
                            if let Some(i) = self.deep_idx.as_mut() {
                                *i = i.saturating_sub(1);
                            }
                        }
                        self.deep_history.push_back(c);
                    }
                }
                Event::Message(Message::Raw { offset, data }) => {
                    let o = offset as usize;
                    if o + data.len() <= self.raw.len() {
                        self.raw[o..o + data.len()].copy_from_slice(&data);
                        self.raw_received = self.raw_received.max(o + data.len());
                        if self.raw_received >= RAW_SAMPLES {
                            self.notify(tr("Raw buffer received (30100 samples)"));
                        }
                    }
                }
                Event::Message(Message::Unknown(t)) => self.log(format!("unknown message type {t}")),
            }
        }
        self.sync_deep_streaming();
        if self.mode == ViewMode::Deep {
            if let Some(c) = self.shown_deep() {
                let (a, b) = visible_range(self.deep_visible, c.len(), 8);
                let key = (c.info.seq, a, b, self.deep_history.len().wrapping_add(c.len()));
                if self.deep_meas_key != Some(key) {
                    let mut m = c.measure_range(a, b);
                    m.freq_counter = self.status.as_ref().filter(|s| s.freq_raw > 0).map(|s| s.freq_raw as f64);
                    self.meas = m;
                    self.deep_meas_key = Some(key);
                }
            }
        } else if let Some(f) = self.shown_frame() {
            self.meas = measure::measure(f);
        }
        self.update_stats();
        self.update_spectrum();
        let dt = self.fps_t0.elapsed().as_secs_f32();
        if dt >= 1.0 {
            self.fps = self.fps_n as f32 / dt;
            self.deep_rate = self.deep_n as f32 / dt;
            self.deep_n = 0;
            self.fps_n = 0;
            self.fps_t0 = Instant::now();
        }
        for (name, v) in self.pending.due(Duration::from_millis(120)) {
            self.set(&name, v);
        }
    }

    /// Copy device values into the slider state unless the user is editing them.
    fn sync_controls(&mut self, s: &Status) {
        if !self.pending.is_pending("OFFSET") { self.ui_offset = s.offset as i32; }
        if !self.pending.is_pending("TLEVEL") { self.ui_tlevel = s.trig_level as i32; }
        if !self.pending.is_pending("HPOS") { self.ui_hpos = s.hpos as i32; }
        if !self.pending.is_pending("GFREQ") { self.ui_gfreq = s.gen_freq as i32; }
        if !self.pending.is_pending("GDUTY") { self.ui_gduty = s.gen_duty as i32; }
        if !self.pending.is_pending("GAMP") { self.ui_gamp = s.gen_ampl as f32 / 10.0; }
        if !self.pending.is_pending("BEEP") { self.ui_beep = s.beep as i32; }
        if !self.pending.is_pending("LIGHT") { self.ui_light = s.light as i32; }
    }

    // ---- actions -------------------------------------------------------------------------

    fn run_stop(&mut self) {
        let running = self.status.as_ref().map(|s| s.running).or(self.frame.as_ref().map(|f| f.running)).unwrap_or(true);
        if self.v2() { self.set("RUN", if running { 0 } else { 1 }); } else { self.send("KEY 2"); }
    }
    fn single(&mut self) {
        if self.v2() {
            self.set("TMODE", 2);
            self.set("RUN", 1);
        }
    }
    fn auto(&mut self) {
        if self.v2() { self.set("AUTO", 1); } else { self.send("KEY 6"); }
    }
    fn device_shot(&mut self) {
        if self.v2() { self.set("SHOT", 1); } else { self.send("KEY 11"); }
        self.notify(tr("Screenshot saved on the scope (gallery)"));
    }
    fn save_png(&mut self) {
        if let Some(f) = self.shown_frame().cloned() {
            let m = measure::measure(&f);
            match export::save_png(&f, &m, "DSO-510") {
                Ok(p) => self.notify(trf("Saved {}", &[&p.display()])),
                Err(e) => self.notify(trf("PNG failed: {}", &[&e])),
            }
        }
    }
    fn save_deep_csv(&mut self, visible_only: bool) {
        if let Some(c) = self.shown_deep() {
            let (a, b) = if visible_only && self.deep_visible.1 > self.deep_visible.0 { self.deep_visible } else { (0, c.len()) };
            let r = export::save_deep_csv(c, a, b);
            match r {
                Ok(p) => self.notify(trf("Saved {}", &[&p.display()])),
                Err(e) => self.notify(trf("CSV failed: {}", &[&e])),
            }
        }
    }
    fn save_csv(&mut self) {
        if self.mode == ViewMode::Deep {
            return self.save_deep_csv(false);
        }
        if let Some(f) = self.shown_frame().cloned() {
            match export::save_csv(&f) {
                Ok(p) => self.notify(trf("Saved {}", &[&p.display()])),
                Err(e) => self.notify(trf("CSV failed: {}", &[&e])),
            }
        }
    }
    fn open_file(&mut self, path: &std::path::Path) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext == "dsos" {
            match export::load_session(path) {
                Ok(frames) if !frames.is_empty() => {
                    let n = frames.len();
                    self.history = frames.into();
                    self.view = Some(0);
                    self.notify(trf("Loaded session with {} frames - browse it with the history bar", &[&n]));
                }
                Ok(_) => self.notify(tr("Session file is empty")),
                Err(e) => self.notify(format!("{}: {e}", path.display())),
            }
        } else {
            match std::fs::read(path).ok().and_then(|d| protocol::frame_from_screenshot_file(&d)) {
                Some(f) => {
                    self.history.push_back(f);
                    self.view = Some(self.history.len() - 1);
                    self.notify(trf("Opened {} (settings are not stored in this file: shown as 1 V / 1 ms)", &[&path.display()]));
                }
                None => self.notify(trf("{}: not a session or 301-byte trace file", &[&path.display()])),
            }
        }
    }
}

/// The visible sample range remembered from the previous frame, made valid for a record of `len` samples
/// (the record can be a different, shorter one now: a stream instead of an acquisition).
fn visible_range(visible: (usize, usize), len: usize, min_len: usize) -> (usize, usize) {
    let (a, b) = (visible.0.min(len), visible.1.min(len));
    if b > a + min_len { (a, b) } else { (0, len) }
}

/// The deep record to show: the roll-mode stream, else the selected / newest acquisition.
/// (A free function so that the borrow covers only these fields, not the whole App.)
fn pick_deep<'a>(roll_mode: bool, roll: &'a deep::RollStream, history: &'a VecDeque<deep::DeepCapture>, idx: Option<usize>) -> Option<&'a deep::DeepCapture> {
    if roll_mode {
        if let Some(c) = roll.capture() {
            return Some(c);
        }
    }
    match idx {
        Some(i) => history.get(i),
        None => history.back(),
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(22, 24, 28);
    v.window_fill = Color32::from_rgb(26, 28, 33);
    v.extreme_bg_color = Color32::from_rgb(14, 15, 18);
    v.selection.bg_fill = Color32::from_rgb(160, 120, 0);
    v.hyperlink_color = ACCENT;
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        s.visuals = v.clone();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.interaction.tooltip_delay = 0.15;
    });
}

/// "Battery: 80 % · 3.91 V · charger connected"
fn battery_text(s: &Status) -> String {
    let volts = if s.battery_mv > 0 { format!("{:.2}", s.battery_mv as f32 / 1000.0) } else { "---".to_string() };
    let mut t = trf("Battery: {} % · {} V", &[&s.battery_pct, &volts]);
    if s.charging() {
        t.push_str(" · ");
        t.push_str(tr("charger connected"));
    }
    t
}

/// Battery icon (fill = level, colour like the scope's own icon: dark amber below 60 %) with the
/// details in a tooltip. A lightning bolt marks charger / USB power.
fn battery_ui(ui: &mut egui::Ui, s: &Status) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(36.0, 16.0), egui::Sense::hover());
    let body = egui::Rect::from_min_size(rect.min, egui::vec2(32.0, 16.0));
    let col = if s.battery_pct > 60 { GOOD } else if s.battery_pct > 30 { ACCENT } else { BAD };
    let p = ui.painter();
    p.rect_stroke(body, 3.0, egui::Stroke::new(1.5, Color32::from_gray(190)), egui::StrokeKind::Inside);
    p.rect_filled(egui::Rect::from_min_size(rect.min + egui::vec2(32.0, 5.0), egui::vec2(3.0, 6.0)), 1.0, Color32::from_gray(190));
    let inner = body.shrink(3.0);
    let fill = egui::Rect::from_min_size(inner.min, egui::vec2(inner.width() * (s.battery_pct as f32 / 100.0).clamp(0.0, 1.0), inner.height()));
    p.rect_filled(fill, 1.5, col);
    if s.charging() {
        p.text(body.center(), egui::Align2::CENTER_CENTER, "⚡", egui::FontId::proportional(12.0), Color32::BLACK);
    }
    let tip = |ui: &mut egui::Ui| {
        ui.set_max_width(320.0);
        ui.label(RichText::new(battery_text(s)).strong());
        ui.label(trf("The scope reports the level in steps ({} %).", &[&s.battery_pct]));
        ui.label(if s.charging() {
            tr("Charger or USB power connected: while charging the measured voltage is raised, so the level is overestimated.")
        } else {
            tr("Running on the battery.")
        });
    };
    if tour_tip(ui, "battery") { resp.show_tooltip_ui(tip) } else { resp.on_hover_ui(tip); }
}

/// screenshot tour: the tooltips it shows without the mouse
const TOUR_TIP: &str = "tour_tip";
fn tour_tip(ui: &egui::Ui, key: &str) -> bool {
    ui.data(|d| d.get_temp::<Vec<String>>(egui::Id::unique(TOUR_TIP))).is_some_and(|v| v.iter().any(|k| k == key))
}

/// Small [?] that explains a control when the mouse rests on it.
fn help_tip(ui: &mut egui::Ui, text: &str) {
    help_tip_as(ui, text, "");
}

/// `key`: the screenshot tour shows this tip when it names the key
fn help_tip_as(ui: &mut egui::Ui, text: &str, key: &str) {
    let r = ui.add(egui::Label::new(RichText::new("[?]").small().monospace().weak()).sense(egui::Sense::hover()));
    let tip = |ui: &mut egui::Ui| {
        ui.set_max_width(320.0);
        ui.label(text);
    };
    if !key.is_empty() && tour_tip(ui, key) {
        r.show_tooltip_ui(tip);
    } else {
        r.on_hover_cursor(egui::CursorIcon::Help).on_hover_ui(tip);
    }
}

/// Collapsible group with a [?] next to the title.
fn section(ui: &mut egui::Ui, title: &str, help: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    let id = ui.make_persistent_id(("section", title));
    let mut is_open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(open);
    ui.horizontal(|ui| {
        let arrow = if is_open { "▾" } else { "▸" };
        let r = ui
            .add(egui::Label::new(RichText::new(format!("{arrow} {title}")).strong().color(ACCENT)).sense(egui::Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if r.clicked() {
            is_open = !is_open;
        }
        help_tip_as(ui, help, title);
    });
    ui.data_mut(|d| d.insert_temp(id, is_open));
    if is_open {
        ui.indent(id, add);
    }
    ui.add_space(2.0);
}

fn needs_v2(resp: egui::Response, v2: bool) -> egui::Response {
    if v2 { resp } else { resp.on_disabled_hover_text(tr("Needs the v2 firmware (fw/build/APP_DSO-510.bin)")) }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl App {
    /// One frame of the whole window (separate from eframe::App::ui so that tests can drive it).
    fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.tour_step(&ctx);
        if self.bode.running().is_some() {
            if self.conn != ConnState::Connected {
                self.bode.stop(&mut |_| {});
            } else {
                let frame = self.frame.clone();
                let link = &self.link;
                self.bode.tick(frame.as_ref(), &mut |c| link.send(c));
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        if self.conn != ConnState::Connected {
            ctx.request_repaint_after(Duration::from_millis(300));
        }

        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for p in dropped {
            self.open_file(&p);
        }

        let v2 = self.v2();
        let connected = self.conn == ConnState::Connected;
        let st = self.effective_status();
        let running = st.as_ref().map(|s| s.running).or(self.frame.as_ref().map(|f| f.running)).unwrap_or(false);

        // ---------------- top bar ----------------
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("DSO-510 Link")).size(18.0).strong().color(ACCENT));
                ui.separator();
                let (dot, text) = match self.conn {
                    ConnState::Connected => (GOOD, format!("{}  ·  fw v{}", self.port_label, self.fw_version.max(1))),
                    ConnState::Searching => (ACCENT, tr("searching for the scope…").to_string()),
                    ConnState::Off => (BAD, tr("disconnected").to_string()),
                };
                ui.label(RichText::new("●").color(dot));
                ui.label(text);
                egui::ComboBox::from_id_salt("port")
                    .selected_text(tr("Port"))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(false, tr("Automatic (find DSO-510)")).clicked() {
                            self.link.connect(None);
                            self.conn = ConnState::Searching;
                        }
                        for p in self.ports.clone() {
                            if ui.selectable_label(false, &p.label).clicked() {
                                self.link.connect(Some(p));
                                self.conn = ConnState::Searching;
                            }
                        }
                        ui.separator();
                        if ui.selectable_label(false, tr("Refresh list")).clicked() {
                            self.ports = device::list();
                        }
                        if ui.selectable_label(false, tr("Disconnect")).clicked() {
                            self.link.disconnect();
                        }
                    });
                ui.separator();
                ui.label(format!("{:.0} fps", self.fps));
                if let Some(s) = st.as_ref().filter(|s| s.has_battery) {
                    ui.separator();
                    battery_ui(ui, s);
                }
                ui.separator();
                let has_deep = self.has_deep();
                if ui.selectable_label(self.mode == ViewMode::Screen, tr("Screen")).on_hover_text(tr("The 301-point trace the scope shows, ~50 frames/s")).clicked() {
                    self.mode = ViewMode::Screen;
                }
                let deep_ok = has_deep || !connected;
                let r = ui.add_enabled(deep_ok, egui::Button::selectable(self.mode == ViewMode::Deep, tr("Full memory")));
                let r = if deep_ok {
                    r.on_hover_text(tr("All 30100 samples of every acquisition: zoom, pan, measurements at full resolution"))
                } else {
                    r.on_disabled_hover_text(tr("Needs firmware usb_link v3 (fw/build/APP_DSO-510.bin)"))
                };
                if r.clicked() {
                    self.mode = ViewMode::Deep;
                }
                if ui.add_enabled(v2 || !connected, egui::Button::selectable(self.mode == ViewMode::Bode, tr("Frequency response")))
                    .on_hover_text(tr("Bode plot: the generator sweeps a sine, the scope measures the amplitude at the output of your circuit (filter, amplifier...)"))
                    .clicked()
                {
                    self.mode = ViewMode::Bode;
                }
                if self.mode == ViewMode::Deep && self.deep_streaming {
                    ui.label(RichText::new(trf("{} captures/s", &[&format!("{:.1}", self.deep_rate)])).weak());
                }
                ui.separator();
                self.language_menu(ui);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let deep_mode = self.mode == ViewMode::Deep;
                    let can_csv = if deep_mode { self.shown_deep().is_some() } else { self.shown_frame().is_some() };
                    let csv_tip = if deep_mode { tr("Save the whole deep capture (30100 samples) as CSV") } else { tr("Save the shown frame as CSV") };
                    if ui.add_enabled(can_csv, egui::Button::new(tr("CSV"))).on_hover_text(csv_tip).clicked() {
                        self.save_csv();
                    }
                    if ui.add_enabled(!deep_mode && self.shown_frame().is_some(), egui::Button::new(tr("📷 PNG")))
                        .on_hover_text(tr("Save the shown frame as an image"))
                        .on_disabled_hover_text(tr("PNG export is for the Screen view; use CSV for deep captures"))
                        .clicked()
                    {
                        self.save_png();
                    }
                    if ui.add_enabled(connected, egui::Button::new(tr("💾 Save on scope"))).on_hover_text(tr("Store a screenshot in the scope's gallery")).clicked() {
                        self.device_shot();
                    }
                    ui.separator();
                    if ui.add_enabled(connected, egui::Button::new(RichText::new(tr("AUTO")).strong())).clicked() {
                        self.auto();
                    }
                    if needs_v2(ui.add_enabled(connected && v2, egui::Button::new(tr("Single"))), v2).clicked() {
                        self.single();
                    }
                    let (txt, col) = if running { (tr("■ STOP"), BAD) } else { (tr("▶ RUN"), GOOD) };
                    if ui.add_enabled(connected, egui::Button::new(RichText::new(txt).strong().color(Color32::BLACK)).fill(col).min_size(egui::vec2(90.0, 0.0))).clicked() {
                        self.run_stop();
                    }
                });
            });
            ui.add_space(4.0);
        });

        // ---------------- history bar ----------------
        egui::Panel::bottom("history").show(ui, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                let deep_mode = self.mode == ViewMode::Deep;
                let n = if deep_mode { self.deep_history.len() } else { self.history.len() };
                let mut view = if deep_mode { self.deep_idx } else { self.view };
                let live = view.is_none();
                if ui.add(egui::Button::new(RichText::new(tr("● LIVE")).color(if live { Color32::BLACK } else { Color32::WHITE })).fill(if live { GOOD } else { Color32::from_gray(60) })).clicked() {
                    view = None;
                }
                if ui.button("⏮").on_hover_text(tr("oldest")).clicked() && n > 0 {
                    view = Some(0);
                }
                if ui.button("◀").clicked() && n > 0 {
                    view = Some(view.unwrap_or(n - 1).saturating_sub(1));
                }
                if ui.button("▶").clicked() && n > 0 {
                    let i = view.unwrap_or(n - 1) + 1;
                    view = if i >= n { None } else { Some(i) };
                }
                let mut idx = view.unwrap_or(n.saturating_sub(1)) as u32;
                let w = (ui.available_width() - 330.0).max(120.0);
                ui.spacing_mut().slider_width = w;
                if n > 1 && ui.add(egui::Slider::new(&mut idx, 0..=(n as u32 - 1)).show_value(false)).changed() {
                    view = Some(idx as usize);
                }
                let what = if deep_mode { tr("captures") } else { tr("frames") };
                ui.label(format!("{} / {} {what}", if view.is_none() { n } else { idx as usize + 1 }, n));
                if deep_mode {
                    self.deep_idx = view;
                } else {
                    self.view = view;
                }
                if !deep_mode && ui.button(tr("Save session")).on_hover_text(tr("All frames of the history (.dsos)")).clicked() && n > 0 {
                    match export::save_session(self.history.iter()) {
                        Ok(p) => self.notify(trf("Saved {}", &[&p.display()])),
                        Err(e) => self.notify(trf("Save failed: {}", &[&e])),
                    }
                }
                if ui.button(tr("Clear")).clicked() {
                    if deep_mode {
                        self.deep_history.clear();
                        self.deep_idx = None;
                    } else {
                        self.history.clear();
                        self.view = None;
                    }
                }
                if ui.button("📂").on_hover_text(tr("Open the captures folder (drop .dsos or scope .bin files on the window to open them)")).clicked() {
                    let _ = std::process::Command::new("explorer").arg(export::captures_dir()).spawn();
                }
            });
            ui.add_space(3.0);
        });

        // ---------------- control panel ----------------
        egui::Panel::right("controls").resizable(true).default_size(340.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(4.0);
                if self.mode == ViewMode::Bode {
                    self.bode_ui(ui, v2 && connected);
                }
                self.measurements_ui(ui);
                if self.mode == ViewMode::Deep {
                    self.deep_ui(ui);
                }
                self.vertical_ui(ui, v2 && connected);
                self.horizontal_ui(ui, v2 && connected);
                self.trigger_ui(ui, v2 && connected);
                self.spectrum_ui(ui);
                self.cursor_ui(ui);
                section(ui, tr("Display"), tr("Appearance of the trace. Persistence leaves faded older frames behind the current one, which shows jitter and noise."), false, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Slider::new(&mut self.persistence, 0..=64).text(tr("persistence (frames)")));
                        help_tip(ui, tr("Number of previous frames drawn faded behind the current one (0 means off). Good for spotting jitter, noise and rare glitches."));
                    });
                });
                self.generator_ui(ui, v2 && connected);
                self.instrument_ui(ui, v2 && connected);
                self.advanced_ui(ui, connected);
            });
        });

        // ---------------- scope ----------------
        egui::CentralPanel::default().show(ui, |ui| {
            if self.crash_banner {
                ui.horizontal(|ui| {
                    ui.colored_label(BAD, tr("The previous session ended abnormally. Details are in crash.log."));
                    if ui.small_button(tr("Open log folder")).clicked() {
                        let _ = std::process::Command::new("explorer").arg(crashlog::dir()).spawn();
                    }
                    if ui.small_button(tr("Dismiss")).clicked() {
                        self.crash_banner = false;
                    }
                });
            }
            if self.mode == ViewMode::Bode {
                self.bode.show(ui);
                return;
            }
            if self.mode == ViewMode::Deep {
                if connected && !self.has_deep() {
                    ui.colored_label(ACCENT, tr("Full memory needs firmware usb_link v3: flash fw/build/APP_DSO-510.bin."));
                }
                let badge = if self.deep_idx.is_some() {
                    Some((tr("HISTORY"), ACCENT))
                } else if self.shown_deep().is_none() {
                    None
                } else {
                    Some(if running { self.trig_badge(tr("LIVE")) } else { (tr("STOP"), BAD) })
                };
                let roll_mode = self.roll_mode();
                let c = pick_deep(roll_mode, &self.roll, &self.deep_history, self.deep_idx);
                let full = ui.available_size();
                let top_h = if self.show_fft { (full.y * 0.58).max(120.0) } else { full.y };
                self.deep_view.t_on = self.cursors.time_on;
                self.deep_view.v_on = self.cursors.volt_on;
                let can_set = v2 && connected && self.deep_idx.is_none() && !c.is_some_and(|c| c.stream);
                let out = ui.allocate_ui(egui::vec2(full.x, top_h), |ui| self.deep_view.show(ui, c, badge, can_set)).inner;
                self.deep_visible = out.visible;
                if let Some(l) = out.set_trigger_level {
                    self.ui_tlevel = l as i32;
                    self.set("TLEVEL", l as i32);
                }
                if self.show_fft {
                    ui.add_space(4.0);
                    self.spec_view.show(ui, self.spec.as_ref());
                }
                return;
            }
            if connected && !v2 && self.fw_version == 1 {
                ui.colored_label(ACCENT, tr("Firmware v1: live view and front-panel keys only. Flash fw/build/APP_DSO-510.bin for full remote control."));
            }
            let shown = self.shown_frame().cloned();
            let pers: Vec<&Frame> = if self.persistence > 0 && !self.history.is_empty() {
                let end = self.view.map(|i| i + 1).unwrap_or(self.history.len());
                let start = end.saturating_sub(self.persistence + 1);
                self.history.range(start..end.saturating_sub(1)).collect()
            } else {
                Vec::new()
            };
            let badge = if self.view.is_some() {
                Some((tr("HISTORY"), ACCENT))
            } else if shown.is_some() {
                Some(if running { self.trig_badge(tr("RUN")) } else { (tr("STOP"), BAD) })
            } else {
                None
            };
            let full = ui.available_size();
            let top_h = if self.show_fft { (full.y * 0.62).max(120.0) } else { full.y };
            let out = ui
                .allocate_ui(egui::vec2(full.x, top_h), |ui| {
                    self.scope.show(ui, scope::ScopeInput {
                        frame: shown.as_ref(),
                        persistence: &pers,
                        cursors: &mut self.cursors,
                        hpos: st.as_ref().map(|s| s.hpos),
                        show_trigger: true,
                        can_set_trigger: v2 && connected && self.view.is_none(),
                        badge,
                    })
                })
                .inner;
            if self.show_fft {
                ui.add_space(4.0);
                self.spec_view.show(ui, self.spec.as_ref());
            }
            if let Some(l) = out.set_trigger_level {
                self.ui_tlevel = l as i32;
                self.set("TLEVEL", l as i32);
            }
            if let Some(o) = out.set_offset {
                self.ui_offset = o as i32;
                self.set("OFFSET", o as i32);
            }
            if let Some(h) = out.set_hpos {
                self.ui_hpos = h as i32;
                self.set("HPOS", h as i32);
            }
        });

        // toast
        if let Some((msg, t)) = &self.toast {
            if t.elapsed() < Duration::from_secs(4) {
                egui::Area::new(egui::Id::unique("toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -60.0))
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(RichText::new(msg).color(Color32::WHITE));
                        });
                    });
                ctx.request_repaint_after(Duration::from_millis(500));
            } else {
                self.toast = None;
            }
        }
    }
}

impl App {
    fn language_menu(&mut self, ui: &mut egui::Ui) {
        let current = self.languages.iter().find(|l| l.code == self.lang).map(|l| l.name.clone()).unwrap_or_else(|| self.lang.clone());
        let mut chosen = None;
        egui::ComboBox::from_id_salt("language")
            .selected_text(format!("🌐 {current}"))
            .show_ui(ui, |ui| {
                for l in &self.languages {
                    if ui.selectable_label(l.code == self.lang, &l.name).on_hover_text(&l.source).clicked() {
                        chosen = Some(l.code.clone());
                    }
                }
                ui.separator();
                ui.label(RichText::new(tr("Translations")).weak());
                if ui.selectable_label(false, tr("Open translations folder")).on_hover_text(tr("Edit the file and choose the language again to reload it.")).clicked() {
                    let dir = i18n::lang_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let pl = dir.join("pl.txt");
                    if !pl.exists() {
                        let _ = std::fs::write(&pl, include_str!("../lang/pl.txt"));
                    }
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                    self.languages = i18n::available();
                }
                if ui.selectable_label(false, tr("Export template for a new language")).clicked() {
                    let dir = i18n::lang_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let path = dir.join("template.txt.new");
                    match i18n::write_template(&path) {
                        Ok(()) => {
                            let _ = std::process::Command::new("explorer").arg("/select,").arg(&path).spawn();
                        }
                        Err(e) => chosen_err(&mut self.log, e),
                    }
                }
            });
        if let Some(code) = chosen {
            self.languages = i18n::available();
            let n = i18n::set_language(&code);
            i18n::save_setting("language", &code);
            self.lang = code;
            if self.lang != "en" {
                self.notify(trf("{} texts translated", &[&n]));
            }
        }
    }

    fn measurements_ui(&mut self, ui: &mut egui::Ui) {
        let deep_caption = if self.mode == ViewMode::Deep {
            let (a, b) = self.deep_visible;
            Some(if b > a { trf("from {} samples in the visible window", &[&(b - a)]) } else { tr("from the whole record").to_string() })
        } else {
            None
        };
        section(ui, tr("Measurements"), tr("Automatic measurements of the shown trace: levels, frequency, duty cycle and edge times. They are computed in this program; in Full memory from the visible window."), true, |ui| {
            if let Some(c) = &deep_caption {
                ui.small(c);
            }
            let m = &self.meas;
            let opt = |x: Option<f64>, u: &str| x.map(|v| eng(v, u)).unwrap_or_else(|| "---".into());
            egui::Grid::new("meas").num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, a: &str, av: String, b: &str, bv: String| {
                    ui.label(RichText::new(a).weak());
                    ui.label(RichText::new(av).monospace().strong());
                    ui.label(RichText::new(b).weak());
                    ui.label(RichText::new(bv).monospace().strong());
                    ui.end_row();
                };
                row(ui, "Vpp", eng(m.vpp, "V"), tr("Freq"), opt(m.freq.or(m.freq_counter), "Hz"));
                row(ui, "Vmax", eng(m.vmax, "V"), tr("Period"), opt(m.period, "s"));
                row(ui, "Vmin", eng(m.vmin, "V"), tr("Duty"), m.duty.map(|d| format!("{d:.1} %")).unwrap_or_else(|| "---".into()));
                row(ui, "Vavg", eng(m.vavg, "V"), tr("Rise"), opt(m.rise, "s"));
                row(ui, "Vrms", eng(m.vrms, "V"), tr("Fall"), opt(m.fall, "s"));
                row(ui, tr("Ampl"), eng(m.amplitude, "V"), tr("Counter"), opt(m.freq_counter, "Hz"));
                let pct = |x: Option<f64>| x.map(|d| format!("{d:.1} %")).unwrap_or_else(|| "---".into());
                row(ui, tr("Overshoot"), pct(m.overshoot), tr("+Width"), opt(m.pwidth, "s"));
                row(ui, tr("Preshoot"), pct(m.preshoot), tr("-Width"), opt(m.nwidth, "s"));
                row(ui, tr("Edges"), format!("{}", m.edges), "", String::new());
            });
            if m.clipped {
                ui.colored_label(BAD, tr("⚠ signal clipped - increase V/div"));
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.show_stats, tr("Statistics"));
                help_tip(ui, tr("Running min / mean / max of each measurement over consecutive acquisitions, to judge stability and jitter. They reset when time/div or V/div change."));
                if self.show_stats && ui.small_button(tr("Reset")).clicked() {
                    self.stats = [Stat::default(); 10];
                }
            });
            if self.show_stats {
                let n = self.stats.iter().map(|s| s.n).max().unwrap_or(0);
                ui.small(trf("{} acquisitions (reset when time/div or V/div change)", &[&n]));
                egui::Grid::new("stats").num_columns(4).spacing([10.0, 2.0]).striped(true).show(ui, |ui| {
                    for h in [tr("Value"), "min", tr("mean"), "max"] {
                        ui.label(RichText::new(h).weak());
                    }
                    ui.end_row();
                    for (st, (name, unit, _)) in self.stats.iter().zip(stat_values(&self.meas)) {
                        if st.n == 0 {
                            continue;
                        }
                        let f = |x: f64| if unit == "%" { format!("{x:.1} %") } else { eng(x, unit) };
                        ui.label(tr(name));
                        ui.label(RichText::new(f(st.min)).monospace());
                        ui.label(RichText::new(st.mean().map(f).unwrap_or_default()).monospace().strong());
                        ui.label(RichText::new(f(st.max)).monospace());
                        ui.end_row();
                    }
                });
            }
        });
    }

    fn deep_ui(&mut self, ui: &mut egui::Ui) {
        let c = self.shown_deep().cloned();
        section(ui, tr("Full memory"), tr("Every acquisition with all 30100 samples instead of the 301 points of the screen: about 100x finer detail, zoom, precise measurements and a fine spectrum."), true, |ui| {
            let Some(c) = c else {
                ui.label(if self.deep_streaming { if self.roll_mode() && !self.has_roll() { tr("The roll-mode stream needs firmware usb_link v4: flash fw/build/APP_DSO-510.bin.") } else if self.roll_mode() { tr("Waiting for the first samples… (50 ms/div and slower: the scope streams one point per pixel step, at 20 s/div one every 0.8 s)") } else { tr("Waiting for the first capture… (the scope screen must be open)") } } else { tr("Not streaming.") });
                return;
            };
            egui::Grid::new("deepinfo").num_columns(3).spacing([12.0, 3.0]).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, a: &str, b: String, h: &str| {
                    ui.label(RichText::new(a).weak());
                    ui.label(RichText::new(b).monospace());
                    help_tip(ui, h);
                    ui.end_row();
                };
                row(ui, tr("Record"), trf("{} samples, {}", &[&c.len(), &eng(c.t_last() - c.t_first(), "s")]), tr("Number of samples of this acquisition and the time they cover."));
                row(ui, tr("Sample rate"), format!("{}/s {}", eng(c.sample_rate(), "Sa"), if c.info.interleave { tr("(4× interleaved)") } else { "" }), tr("How often the converter samples. Up to 50 MS/s at the fastest time bases; slower time bases sample less often."));
                row(ui, tr("Resolution"), trf("{} / sample", &[&eng(c.dt, "s")]), tr("Time between two neighbouring samples."));
                if c.stream {
                    row(ui, tr("Mode"), tr("stream (roll mode)").to_string(), tr("From 50 ms/div the scope has no sample record: it adds one point per pixel step to the screen. Here they are collected into one long history; time 0 is the start of the record."));
                }
                if !c.stream {
                row(ui, tr("vs. screen"), trf("{}× longer, {} samples/pixel", &[&format!("{:.0}", (c.t_last() - c.t_first()) / (c.screen_window().1 - c.screen_window().0)), &format!("{:.0}", c.info.spp_view.max(0.0))]), tr("How much longer the record is than the part shown on the scope's own screen, and how many samples one screen pixel covers."));
                row(ui, tr("Pre-trigger"), eng(-c.t_first(), "s"), tr("Time recorded before the trigger point: you can look back at what led to the event."));
                row(ui, tr("Captured"), format!("{}  #{}", c.when, c.info.seq), tr("Time of reception (UTC) and the acquisition number."));
                }
            });
            ui.horizontal(|ui| {
                if ui.button(tr("Fit all")).on_hover_text(tr("whole record (double-click the plot)")).clicked() {
                    self.deep_view.fit_all();
                }
                if ui.button(tr("Screen window")).on_hover_text(tr("the part the scope shows")).clicked() {
                    self.deep_view.show_screen_window(&c);
                    self.deep_view.follow_end = c.stream;       // the newest screen of a stream keeps following
                }
                if c.stream && ui.button(tr("Follow live")).on_hover_text(tr("Keep the newest samples at the right edge. While following, the wheel zooms around the newest sample; drag the view into the past to stop following and zoom there.")).clicked() {
                    self.deep_view.follow_end = true;
                }
                if ui.button("＋").clicked() {
                    self.deep_view.zoom(&c, 0.5);
                }
                if ui.button("－").clicked() {
                    self.deep_view.zoom(&c, 2.0);
                }
            });
            ui.horizontal(|ui| {
                if ui.button(tr("CSV (all)")).clicked() {
                    self.save_deep_csv(false);
                }
                if ui.button(tr("CSV (visible)")).clicked() {
                    self.save_deep_csv(true);
                }
                if ui.button(tr("Raw .bin")).clicked() {
                    match export::save_raw(c.adc.as_ref().unwrap_or(&c.codes)) {
                        Ok(p) => self.notify(trf("Saved {}", &[&p.display()])),
                        Err(e) => self.notify(trf("Save failed: {}", &[&e])),
                    }
                }
            });
            ui.small(tr("Wheel: zoom · drag: pan · drag the strip on top: move · double-click: fit. The blue band is the scope's screen. Acquisition pauses while a record is sent (about 5 captures/s)."));
            if self.deep_asm.incomplete > 0 {
                ui.small(trf("incomplete transfers: {}", &[&self.deep_asm.incomplete]));
            }
        });
    }

    fn vertical_ui(&mut self, ui: &mut egui::Ui, en: bool) {
        let st = self.effective_status();
        section(ui, tr("Vertical"), tr("Input settings: sensitivity (volts per division), AC/DC coupling, probe ×1/×10 and vertical offset."), true, |ui| {
            ui.add_enabled_ui(en, |ui| {
                let probe = st.as_ref().map(|s| s.probe).unwrap_or(0);
                let vdiv = st.as_ref().map(|s| s.vdiv).unwrap_or(6) as usize;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_label(tr("V/div")).selected_text(protocol::VDIV_LABELS[(probe as usize * 3 + vdiv).min(12)]).show_ui(ui, |ui| {
                        for i in 0..10 {
                            if ui.selectable_label(i == vdiv, protocol::VDIV_LABELS[probe as usize * 3 + i]).clicked() {
                                self.set("VDIV", i as i32);
                            }
                        }
                    });
                    help_tip(ui, tr("Volts per vertical division. Choose the lowest value at which the signal still fits on the screen: that gives the best resolution."));
                });
                ui.horizontal(|ui| {
                    let c = st.as_ref().map(|s| s.coupling).unwrap_or(1);
                    ui.label(tr("Coupling"));
                    if ui.selectable_label(c == 1, tr("DC")).clicked() { self.set("COUPLING", 1); }
                    if ui.selectable_label(c == 0, tr("AC")).clicked() { self.set("COUPLING", 0); }
                    ui.separator();
                    ui.label(tr("Probe"));
                    if ui.selectable_label(probe == 0, "×1").clicked() { self.set("PROBE", 0); }
                    if ui.selectable_label(probe == 1, "×10").clicked() { self.set("PROBE", 1); }
                    help_tip(ui, tr("DC shows the whole signal; AC blocks the DC component and shows only the changes. Probe ×10 is for a probe set to 10:1: it multiplies the scale by 10."));
                });
                ui.horizontal(|ui| {
                    if ui.add(egui::Slider::new(&mut self.ui_offset, -100..=100).text(tr("offset (px)"))).changed() {
                        self.pending.set("OFFSET", self.ui_offset);
                    }
                    help_tip(ui, tr("Moves the trace up or down (in screen pixels, 25 per division). The trigger level moves with it. You can also drag the yellow marker on the left of the screen."));
                });
            });
        });
    }

    fn horizontal_ui(&mut self, ui: &mut egui::Ui, en: bool) {
        let st = self.effective_status();
        section(ui, tr("Horizontal"), tr("Time base (time per division) and horizontal position of the trigger point."), true, |ui| {
            ui.add_enabled_ui(en, |ui| {
                let tb = st.as_ref().map(|s| s.timebase).unwrap_or(13) as usize;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_label(tr("Time/div")).selected_text(protocol::TIMEBASE_LABELS[tb.min(26)]).height(400.0).show_ui(ui, |ui| {
                        for (i, l) in protocol::TIMEBASE_LABELS.iter().enumerate() {
                            if ui.selectable_label(i == tb, *l).clicked() {
                                self.set("TB", i as i32);
                            }
                        }
                    });
                    help_tip(ui, tr("Time per horizontal division. Fast settings (up to 20 µs/div) are sampled at 25-50 MS/s; from 50 ms/div the scope scrolls the trace (roll mode)."));
                });
                ui.horizontal(|ui| {
                    if ui.add(egui::Slider::new(&mut self.ui_hpos, -150..=150).text(tr("position"))).changed() {
                        self.pending.set("HPOS", self.ui_hpos);
                    }
                    help_tip(ui, tr("Moves the trigger point left or right to see more of what happened before or after the event. You can also drag the orange marker at the top of the screen."));
                });
                ui.small(tr("Position applies immediately while stopped in roll mode, otherwise on the next capture."));
            });
        });
    }

    fn trigger_ui(&mut self, ui: &mut egui::Ui, en: bool) {
        let st = self.effective_status();
        section(ui, tr("Trigger"), tr("When the scope starts drawing: the trigger fires when the signal crosses the level in the chosen direction. Normal waits for it, Auto draws without it after a while, Single catches one event."), true, |ui| {
            ui.add_enabled_ui(en, |ui| {
                let mode = st.as_ref().map(|s| s.trig_mode).unwrap_or(1);
                let edge = st.as_ref().map(|s| s.trig_edge).unwrap_or(0);
                ui.horizontal(|ui| {
                    for (i, l) in protocol::TRIG_MODE_LABELS.iter().enumerate() {
                        if ui.selectable_label(mode as usize == i, tr(l)).clicked() {
                            self.set("TMODE", i as i32);
                        }
                    }
                    help_tip(ui, tr("Normal: draws only after a trigger. Auto: draws even without one. Single: waits for one trigger, then stops."));
                });
                ui.horizontal(|ui| {
                    if ui.selectable_label(edge == 0, tr("↑ Rising")).clicked() { self.set("TEDGE", 0); }
                    if ui.selectable_label(edge == 1, tr("↓ Falling")).clicked() { self.set("TEDGE", 1); }
                    help_tip(ui, tr("Trigger on a rising or on a falling edge of the signal."));
                });
                let volts = self.frame.as_ref().map(|f| (self.ui_tlevel as f64 - f.offset as f64) / protocol::PX_PER_DIV * f.volts_per_div());
                ui.horizontal(|ui| {
                    if ui.add(egui::Slider::new(&mut self.ui_tlevel, -100..=100).text(trf("level  {}", &[&volts.map(|v| eng(v, "V")).unwrap_or_default()]))).changed() {
                        self.pending.set("TLEVEL", self.ui_tlevel);
                    }
                    help_tip(ui, tr("Voltage at which the trigger fires. Set it inside the signal's swing; you can drag the orange ▶ marker on the right of the screen."));
                });
                ui.small(tr("Tip: drag the orange ▶ marker on the screen."));
            });
        });
    }

    fn spectrum_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, tr("Spectrum (FFT)"), tr("Shows which frequencies the signal consists of. A pure sine is a single peak; distortion adds harmonics and interference shows up as extra spikes."), false, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.show_fft, tr("Show"));
                egui::ComboBox::from_id_salt("fftwin").selected_text(tr(self.fft_window.label())).show_ui(ui, |ui| {
                    for w in spectrum::Window::ALL {
                        ui.selectable_value(&mut self.fft_window, w, tr(w.label()));
                    }
                });
                help_tip(ui, tr("Window: reduces leakage between frequencies. Hann is a good default, Blackman-Harris has the lowest side lobes, Flat top measures amplitude most exactly, Rectangular has the sharpest frequency resolution."));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.spec_view.log_freq, tr("log frequency"));
                if ui.small_button(tr("whole range")).clicked() {
                    self.spec_view.reset();
                }
                help_tip(ui, tr("Logarithmic frequency axis is better for wide ranges; 'whole range' undoes zoom and pan."));
            });
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut self.spec_view.db_span, 40.0..=140.0).step_by(10.0).text(tr("dB range")));
                help_tip(ui, tr("How many dB below the highest peak are shown. 80 dB is enough for most signals; the 8-bit converter of the scope is good for about 45 dB."));
            });
            if !self.show_fft {
                ui.small(tr("Full memory gives a fine spectrum (up to 30100 samples); the screen trace only 301 points."));
                return;
            }
            let Some(s) = &self.spec else { return };
            egui::Grid::new("fftres").num_columns(3).spacing([12.0, 3.0]).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, a: &str, b: String, h: &str| {
                    ui.label(RichText::new(a).weak());
                    ui.label(RichText::new(b).monospace().strong());
                    help_tip(ui, h);
                    ui.end_row();
                };
                let h_fund = tr("Frequency of the strongest peak, interpolated between the FFT bins, so it is more exact than the bin spacing.");
                if let Some(f) = &s.fundamental {
                    row(ui, tr("Fundamental"), eng(f.freq, "Hz"), h_fund);
                    row(ui, tr("Level"), format!("{} rms ({:.1} dBV)", eng(f.vrms, "V"), 20.0 * f.vrms.max(1e-9).log10()), tr("Amplitude of the fundamental as RMS volts and in dBV (decibels relative to 1 V RMS)."));
                } else {
                    row(ui, tr("Fundamental"), "---".into(), h_fund);
                }
                row(ui, "THD", s.thd.map(|t| format!("{t:.2} %")).unwrap_or_else(|| "---".into()), tr("Total harmonic distortion: RMS of the harmonics H2-H10 relative to the fundamental. 0 % is a perfect sine; a square wave has tens of percent."));
                row(ui, "SFDR", s.sfdr.map(|t| format!("{t:.1} dBc")).unwrap_or_else(|| "---".into()), tr("Spurious-free dynamic range: by how many dB the strongest unwanted spike is below the fundamental. Larger means cleaner."));
                row(ui, tr("Noise floor"), format!("{:.1} dBV / bin", s.noise_floor), tr("Typical (median) level of the bins: the noise of the signal and of the scope's 8-bit converter."));
                row(ui, tr("Resolution"), eng(s.df, "Hz"), tr("Distance between two FFT bins. A longer record (Full memory) gives a finer resolution."));
            });
            if !s.harmonics.is_empty() {
                egui::Grid::new("harm").num_columns(3).spacing([12.0, 1.0]).show(ui, |ui| {
                    for h in s.harmonics.iter().take(6) {
                        ui.label(RichText::new(format!("H{}", h.order)).weak());
                        ui.label(RichText::new(eng(h.freq, "Hz")).monospace());
                        ui.label(RichText::new(format!("{:.1} dBc", h.dbc)).monospace());
                        ui.end_row();
                    }
                });
            }
            ui.small(tr("Wheel: zoom · drag: pan · double-click: whole range. In Full memory the spectrum follows the visible window."));
        });
    }

    fn cursor_ui(&mut self, ui: &mut egui::Ui) {
        let f = self.shown_frame().cloned();
        section(ui, tr("Cursors"), tr("Two draggable lines for measuring time differences (ΔT, 1/ΔT) and voltage differences (ΔV) on the screen."), false, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.cursors.time_on, RichText::new(tr("Time")).color(scope::CURSOR_T_COLOR));
                ui.checkbox(&mut self.cursors.volt_on, RichText::new(tr("Voltage")).color(scope::CURSOR_V_COLOR));
                help_tip(ui, tr("Tick Time and/or Voltage, then drag the dashed lines on the screen. The differences are shown below."));
            });
            if self.mode == ViewMode::Deep {
                if let Some(c) = self.shown_deep() {
                    if self.cursors.time_on {
                        let [t1, t2] = self.deep_view.cur_t;
                        let d = (t2 - t1).abs();
                        ui.monospace(format!("T1 {}  T2 {}", eng(t1, "s"), eng(t2, "s")));
                        ui.monospace(format!("ΔT {}  1/ΔT {}", eng(d, "s"), if d > 0.0 { eng(1.0 / d, "Hz") } else { "---".into() }));
                    }
                    if self.cursors.volt_on {
                        let [v1, v2] = self.deep_view.cur_v.map(|px| c.volts(px.round().clamp(0.0, 255.0) as u8));
                        ui.monospace(format!("V1 {}  V2 {}", eng(v1, "V"), eng(v2, "V")));
                        ui.monospace(format!("ΔV {}", eng((v2 - v1).abs(), "V")));
                    }
                }
            } else if let Some(f) = f {
                let dt = f.seconds_per_px();
                if self.cursors.time_on {
                    let t1 = self.cursors.t[0] as f64 * dt;
                    let t2 = self.cursors.t[1] as f64 * dt;
                    let d = (t2 - t1).abs();
                    ui.monospace(format!("T1 {}  T2 {}", eng(t1, "s"), eng(t2, "s")));
                    ui.monospace(format!("ΔT {}  1/ΔT {}", eng(d, "s"), if d > 0.0 { eng(1.0 / d, "Hz") } else { "---".into() }));
                }
                if self.cursors.volt_on {
                    let v1 = protocol::px_to_volts(self.cursors.v[0] as f64, f.volts_per_div(), f.offset);
                    let v2 = protocol::px_to_volts(self.cursors.v[1] as f64, f.volts_per_div(), f.offset);
                    ui.monospace(format!("V1 {}  V2 {}", eng(v1, "V"), eng(v2, "V")));
                    ui.monospace(format!("ΔV {}", eng((v2 - v1).abs(), "V")));
                }
            }
            ui.small(tr("Drag the dashed lines on the screen."));
        });
    }

    /// Documentation screenshots (DSO510_SHOTS=<dir>): English UI, Screen, Full memory, FFT, Frequency
    /// response with its help, then the frequency response after the user has measured it; then closes.
    /// Trigger status like a bench scope: Trig'd (triggered recently), WAIT (Normal / Single without a
    /// trigger: the display keeps the last triggered waveform), AUTO (Auto without a trigger: free running).
    /// `plain` is the label for firmware that does not report it.
    fn trig_badge(&self, plain: &'static str) -> (&'static str, Color32) {
        // roll time bases (50 ms/div and slower) scroll without a trigger, except Single
        let Some(f) = self.frame.as_ref().filter(|f| f.triggered().is_some() && (f.timebase < 18 || f.trig_mode == 2)) else {
            return (plain, GOOD);
        };
        if self.last_trig.is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
            (tr("Trig'd"), GOOD)
        } else if f.trig_mode == 1 {
            (tr("AUTO"), ACCENT)
        } else {
            (tr("WAIT"), ACCENT)
        }
    }

    fn tour_step(&mut self, ctx: &egui::Context) {
        if self.tour.is_none() {
            return;
        }
        // read before the tour is borrowed
        let connected = self.conn == ConnState::Connected;
        let have_frame = self.frame.is_some();
        let have_deep = self.shown_deep().is_some();
        let bode_done = !self.bode.measured.is_empty() && self.bode.running().is_none();
        let Some(t) = self.tour.as_mut() else { return };
        ctx.request_repaint_after(Duration::from_millis(100));
        // a requested screenshot is saved by its callback, which sets `done`
        if t.waiting_shot.is_some() {
            if t.shot_done.load(std::sync::atomic::Ordering::Acquire) {
                let name = t.waiting_shot.take().unwrap();
                t.step += 1;
                t.since = Instant::now();
                self.log(format!("screenshot {name}"));
            }
            return;
        }
        let waited = t.since.elapsed();
        let shoot = |t: &mut ShotTour, name: &str| {
            t.waiting_shot = Some(name.to_string());
            t.shot_done.store(false, std::sync::atomic::Ordering::Release);
            let path = t.dir.join(name);
            let done = t.shot_done.clone();
            let repaint = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
                let [w, h] = img.size;
                let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                if let Ok(f) = std::fs::File::create(&path) {
                    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
                    enc.set_color(png::ColorType::Rgba);
                    enc.set_depth(png::BitDepth::Eight);
                    if let Ok(mut wr) = enc.write_header() {
                        let _ = wr.write_image_data(&rgba);
                    }
                }
                done.store(true, std::sync::atomic::Ordering::Release);
                repaint.request_repaint();
            })));
        };
        match t.step {
            0 => {
                i18n::set_language("en");                // not saved: the user's setting stays
                self.lang = "en".into();
                self.mode = ViewMode::Screen;
                self.show_fft = false;
                self.crash_banner = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1600.0, 900.0)));
                ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 1.0e6); // no tooltip under a resting mouse
                t.step = 1;
                t.since = Instant::now();
            }
            1 if connected && have_frame && waited > Duration::from_secs(4) => shoot(t, "screen.png"),
            // tooltips: the battery and the [?] of the measurements, shown without the mouse
            2 => {
                ctx.data_mut(|d| d.insert_temp(egui::Id::unique(TOUR_TIP), vec!["battery".to_string(), tr("Measurements").to_string()]));
                t.step = 3;
                t.since = Instant::now();
            }
            3 if waited > Duration::from_secs(1) => shoot(t, "tooltips.png"),
            4 => {
                ctx.data_mut(|d| d.remove::<Vec<String>>(egui::Id::unique(TOUR_TIP)));
                self.mode = ViewMode::Deep;
                t.step = 5;
                t.since = Instant::now();
            }
            5 if have_deep && waited > Duration::from_secs(5) => shoot(t, "full_memory.png"),
            6 => {
                self.show_fft = true;
                t.step = 7;
                t.since = Instant::now();
            }
            7 if waited > Duration::from_secs(3) => shoot(t, "fft.png"),
            8 => {
                self.show_fft = false;
                self.mode = ViewMode::Bode;
                t.step = 9;
                t.since = Instant::now();
            }
            9 if waited > Duration::from_secs(2) => shoot(t, "frequency_response_help.png"),
            // DSO510_SHOTS_QUICK: no frequency response measurement, close now
            10 if std::env::var_os("DSO510_SHOTS_QUICK").is_some() => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            // the user measures (reference, move the probe, measurement); then the result
            10 if bode_done && waited > Duration::from_secs(2) => {
                shoot(t, "frequency_response.png")
            }
            11 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }

    fn bode_ui(&mut self, ui: &mut egui::Ui, en: bool) {
        section(ui, tr("Frequency response"), tr("Measures how much your circuit passes at each frequency (gain in dB), using the scope's own generator."), true, |ui| {
            let running = self.bode.running();
            ui.add_enabled_ui(running.is_none(), |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("from"));
                    ui.add(egui::DragValue::new(&mut self.bode.f_start).range(10.0..=40000.0).suffix(" Hz").speed(5.0));
                    ui.label(tr("to"));
                    ui.add(egui::DragValue::new(&mut self.bode.f_stop).range(20.0..=50000.0).suffix(" Hz").speed(50.0));
                });
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut self.bode.steps, 5..=80).text(tr("points")));
                    help_tip(ui, tr("Frequencies per sweep, spaced logarithmically. Each point takes about 1.5-2 s."));
                });
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut self.bode.gen_amp, 0.1..=3.0).step_by(0.1).text(tr("generator V")));
                    help_tip(ui, tr("Generator amplitude. Lower it if your circuit distorts or clips (amplifiers); the reference must be measured with the same setting."));
                });
            });
            let vdiv = self.effective_status().map(|s| s.vdiv).unwrap_or(6);
            let mut start = None;
            let mut stop = false;
            ui.horizontal(|ui| {
                if let Some((t, i, n)) = running {
                    let what = if t == bode::Target::Reference { tr("reference") } else { tr("measurement") };
                    ui.label(format!("{what}: {i} / {n}"));
                    if ui.button(tr("Stop")).clicked() {
                        stop = true;
                    }
                } else {
                    if ui.add_enabled(en, egui::Button::new(tr("1. Reference"))).on_hover_text(tr("Probe on the generator output (or on the circuit input): measures what goes into the circuit")).clicked() {
                        start = Some(bode::Target::Reference);
                    }
                    if ui.add_enabled(en, egui::Button::new(tr("2. Measure"))).on_hover_text(tr("Probe on the circuit output: the gain is this divided by the reference")).clicked() {
                        start = Some(bode::Target::Measurement);
                    }
                }
            });
            if stop {
                let link = &self.link;
                self.bode.stop(&mut |c| link.send(c));
            }
            if let Some(t) = start {
                // what the sweep changes, to put back afterwards (generator, time base, V/div, trigger)
                let restore = match self.effective_status() {
                    Some(s) => {
                        let mut v: Vec<String> = vec![
                            "SET SCREEN 3".into(),
                            format!("SET GWAVE {}", s.gen_wave),
                            format!("SET GFREQ {}", s.gen_freq.clamp(1, 50000)),
                            format!("SET GDUTY {}", s.gen_duty),
                            format!("SET GAMP {}", s.gen_ampl.clamp(1, 30)),
                            format!("SET GOUT {}", s.gen_out as u8),
                            "SET SCREEN 2".into(),
                            format!("SET TB {}", s.timebase),
                            format!("SET VDIV {}", s.vdiv),
                            format!("SET TMODE {}", s.trig_mode),
                            format!("SET TLEVEL {}", s.trig_level),
                            format!("SET OFFSET {}", s.offset),
                            format!("SET HPOS {}", s.hpos),
                        ];
                        if s.app_mode != 2 && s.app_mode <= 4 {
                            v.push(format!("SET SCREEN {}", s.app_mode));
                        }
                        v
                    }
                    None => Vec::new(),
                };
                let link = &self.link;
                self.bode.start(t, vdiv, restore, &mut |c| link.send(c));
            }
            let mut save = false;
            ui.horizontal(|ui| {
                if !self.bode.reference.is_empty() {
                    ui.label(RichText::new(trf("reference: {} points", &[&self.bode.reference.len()])).weak());
                }
                save = ui.add_enabled(!self.bode.measured.is_empty(), egui::Button::new(tr("CSV"))).clicked();
            });
            if save {
                let path = export::captures_dir().join(format!("bode_{}.csv", export::stamp()));
                match std::fs::create_dir_all(export::captures_dir()).and_then(|_| std::fs::write(&path, self.bode.csv())) {
                    Ok(()) => self.notify(trf("Saved {}", &[&path.display()])),
                    Err(e) => self.notify(trf("CSV failed: {}", &[&e])),
                }
            }
            egui::CollapsingHeader::new(tr("How to use")).default_open(self.bode.measured.is_empty()).show(ui, |ui| {
                ui.label(RichText::new(tr("Step 1: reference")).strong());
                bode::connection_diagram(ui, 1);
                ui.label(RichText::new(tr("Step 2: measurement")).strong());
                bode::connection_diagram(ui, 2);
                ui.add_space(4.0);
                ui.label(tr(BODE_HELP));
            });
        });
    }

    fn generator_ui(&mut self, ui: &mut egui::Ui, en: bool) {
        let st = self.effective_status();
        section(ui, tr("Signal generator"), tr("Controls the generator output of the scope: waveform, frequency, duty cycle, amplitude and output on/off."), false, |ui| {
            ui.add_enabled_ui(en, |ui| {
                let on_gen = st.as_ref().map(|s| s.app_mode == 3).unwrap_or(false);
                let wave = st.as_ref().map(|s| s.gen_wave).unwrap_or(0) as usize;
                egui::ComboBox::from_label(tr("Waveform")).selected_text(tr(protocol::WAVE_LABELS[wave.min(12)])).show_ui(ui, |ui| {
                    for (i, l) in protocol::WAVE_LABELS.iter().enumerate() {
                        if ui.selectable_label(i == wave, tr(l)).clicked() {
                            self.set("GWAVE", i as i32);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(tr("Frequency"));
                    if ui.add(egui::DragValue::new(&mut self.ui_gfreq).range(1..=50000).suffix(" Hz").speed(5.0)).changed() {
                        self.pending.set("GFREQ", self.ui_gfreq);
                    }
                    for (l, f) in [("100", 100), ("1k", 1000), ("10k", 10000)] {
                        if ui.small_button(l).clicked() {
                            self.ui_gfreq = f;
                            self.set("GFREQ", f);
                        }
                    }
                });
                if ui.add(egui::Slider::new(&mut self.ui_gduty, 0..=100).text(tr("duty %"))).changed() {
                    self.pending.set("GDUTY", self.ui_gduty);
                }
                if ui.add(egui::Slider::new(&mut self.ui_gamp, 0.1..=3.0).step_by(0.1).text(tr("amplitude V"))).changed() {
                    self.pending.set("GAMP", (self.ui_gamp * 10.0).round() as i32);
                }
                ui.horizontal(|ui| {
                    let out = st.as_ref().map(|s| s.gen_out).unwrap_or(false);
                    if ui.add_enabled(on_gen, egui::Button::new(if out { tr("Output ON") } else { tr("Output OFF") }).fill(if out { GOOD } else { Color32::from_gray(60) }))
                        .on_disabled_hover_text(tr("Switch the scope to the generator screen first")).clicked()
                    {
                        self.set("GOUT", if out { 0 } else { 1 });
                    }
                    if ui.button(tr("Show generator screen")).clicked() {
                        self.set("SCREEN", 3);
                    }
                });
                if self.fw_version < 4 {
                    ui.small(tr("Above 1 kHz the scope limits the generator while the oscilloscope screen is open."));
                }
            });
        });
    }

    fn instrument_ui(&mut self, ui: &mut egui::Ui, en: bool) {
        let st = self.effective_status();
        section(ui, tr("Instrument"), tr("Switch the scope's screens and set device options: key beep, backlight, auto power-off and the measurements shown on the device."), false, |ui| {
            ui.add_enabled_ui(en, |ui| {
                ui.label(trf("Screen: {}", &[&st.as_ref().map(|s| tr(protocol::screen_name(s.app_mode))).unwrap_or("?")]));
                ui.horizontal(|ui| {
                    if ui.button(tr("Scope")).clicked() { self.set("SCREEN", 2); }
                    if ui.button(tr("Generator")).clicked() { self.set("SCREEN", 3); }
                    if ui.button(tr("Settings")).clicked() { self.set("SCREEN", 4); }
                    if ui.button(tr("Menu")).clicked() { self.set("SCREEN", 0); }
                });
                if ui.add(egui::Slider::new(&mut self.ui_beep, 0..=10).text(tr("key beep"))).changed() {
                    self.pending.set("BEEP", self.ui_beep);
                }
                if ui.add(egui::Slider::new(&mut self.ui_light, 25..=100).text(tr("backlight %"))).changed() {
                    self.pending.set("LIGHT", self.ui_light);
                }
                let ao = st.as_ref().map(|s| s.auto_off).unwrap_or(0) as usize;
                egui::ComboBox::from_label(tr("Auto power-off")).selected_text(tr(protocol::AUTO_OFF_LABELS[ao.min(3)])).show_ui(ui, |ui| {
                    for (i, l) in protocol::AUTO_OFF_LABELS.iter().enumerate() {
                        if ui.selectable_label(i == ao, tr(l)).clicked() {
                            self.set("AUTOOFF", i as i32);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(tr("On-screen measurements"));
                    if ui.small_button(tr("all")).clicked() { self.set("MEAS", 0xFFF); }
                    if ui.small_button(tr("none")).clicked() { self.set("MEAS", 0); }
                });
                if let Some(s) = &st {
                    let adj = protocol::ADJUST_LABELS.get(s.adjust_target as usize).copied().unwrap_or("?");
                    ui.horizontal(|ui| {
                        ui.label(trf("Arrow keys adjust: {}", &[&tr(adj)]));
                        if ui.small_button(tr("next")).clicked() { self.set("ADJ", ((s.adjust_target + 1) % 3) as i32); }
                    });
                    ui.small(trf("Gallery: {} screenshots", &[&s.gallery_count]));
                    if s.has_battery {
                        ui.horizontal(|ui| {
                            ui.small(battery_text(s));
                            help_tip(ui, tr("Battery level and voltage measured by the scope once per second. On USB power the scope charges and the voltage is raised, so the level shown is overestimated; it is accurate on battery power."));
                        });
                    }
                }
                ui.small(tr("Settings are stored by the scope when it is switched off."));
            });
        });
    }

    fn advanced_ui(&mut self, ui: &mut egui::Ui, connected: bool) {
        section(ui, tr("Front panel & advanced"), tr("Virtual buttons of the scope, download of the raw sample buffer and diagnostic commands."), false, |ui| {
            ui.add_enabled_ui(connected, |ui| {
                egui::Grid::new("keys").num_columns(4).show(ui, |ui| {
                    for (i, (label, code, tip)) in PANEL.iter().enumerate() {
                        if ui.button(tr(label)).on_hover_text(tr(tip)).clicked() {
                            self.send(&format!("KEY {code}"));
                        }
                        if i % 4 == 3 {
                            ui.end_row();
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(tr("Read raw buffer")).on_hover_text(tr("All 30100 ADC samples (stop the scope first)")).clicked() {
                        self.raw_received = 0;
                        self.send("RAW");
                    }
                    if self.raw_received > 0 {
                        ui.label(format!("{} / {RAW_SAMPLES}", self.raw_received));
                        if ui.button(tr("Save .bin")).clicked() {
                            match export::save_raw(&self.raw[..self.raw_received]) {
                                Ok(p) => self.notify(trf("Saved {}", &[&p.display()])),
                                Err(e) => self.notify(trf("Save failed: {}", &[&e])),
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button(tr("INFO")).clicked() { self.send("INFO"); }
                    if ui.button(tr("DIAG")).clicked() { self.send("DIAG"); }
                    if ui.button(tr("GET")).clicked() { self.send("GET"); }
                });
            });
            if !self.fw_info.is_empty() {
                ui.small(&self.fw_info);
            }
            ui.separator();
            egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).max_height(160.0).show(ui, |ui| {
                for l in &self.log {
                    ui.monospace(l);
                }
            });
        });
    }
}

fn chosen_err(log: &mut VecDeque<String>, e: std::io::Error) {
    log.push_back(format!("translation template: {e}"));
}

fn main() -> eframe::Result {
    crashlog::install();
    let crashed = crashlog::begin_session();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("DSO-510 Link"),
        ..Default::default()
    };
    let result = eframe::run_native(
        "DSO-510 Link",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc);
            app.crash_banner = crashed;
            Ok(Box::new(app))
        }),
    );
    if result.is_ok() {
        crashlog::end_session();
    }
    result
}

#[cfg(test)]
mod ui_tests {
    //! The whole window driven headless with scripted mouse input.
    use super::*;
    use egui::{Event, Pos2, PointerButton, RawInput, Rect, Vec2};

    struct Win {
        ctx: egui::Context,
        app: App,
        t: f64,
        pos: Pos2,
    }

    impl Win {
        fn new() -> Win {
            let ctx = egui::Context::default();
            let mut app = App::with_link(ctx.clone(), Link::detached());
            app.conn = ConnState::Connected;
            app.fw_version = 3;
            let trace: Vec<u8> = (0..301).map(|i| (128.0 + (i as f32 / 20.0).sin() * 40.0) as u8).collect();
            app.frame = Some(Frame { trace, timebase: 13, vdiv: 6, trace_end: 301, running: true, app_mode: 2, ..Default::default() });
            app.status = Some(Status { version: 3, app_mode: 2, timebase: 13, vdiv: 6, running: true, ..Default::default() });
            let mut w = Win { ctx, app, t: 0.0, pos: Pos2::new(5.0, 5.0) };
            for _ in 0..4 {
                w.step(vec![]);
            }
            w
        }
        fn step(&mut self, events: Vec<Event>) {
            let mut input = RawInput::default();
            input.screen_rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360.0, 820.0)));
            input.time = Some(self.t);
            self.t += 0.016;
            input.events = events;
            let app = &mut self.app;
            let _ = self.ctx.run_ui(input, |ui| app.draw(ui));
        }
        fn move_to(&mut self, p: Pos2) {
            self.pos = p;
            self.step(vec![Event::PointerMoved(p)]);
        }
        fn button(&mut self, pressed: bool) {
            let pos = self.pos;
            self.step(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() }]);
        }
        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.move_to(from);
            self.move_to(from);
            self.button(true);
            for i in 1..=10 {
                self.move_to(from + (to - from) * (i as f32 / 10.0));
            }
            self.step(vec![]);
            self.button(false);
            self.step(vec![]);
        }
        fn sent(&self, prefix: &str) -> bool {
            self.app.log.iter().any(|l| l.starts_with(prefix))
        }
    }

    fn scope_at(w: &Win, x: f32, y: f32) -> Pos2 {
        let plot = w.app.scope.plot_rect.expect("scope was not drawn");
        Pos2::new(plot.left() + x / 300.0 * plot.width(), plot.bottom() - (y - 28.0) / 200.0 * plot.height())
    }

    #[test]
    fn screen_view_trigger_marker_in_the_real_window() {
        let mut w = Win::new();
        let from = scope_at(&w, 300.0, 128.0) + Vec2::new(8.0, 0.0);
        w.drag(from, from + Vec2::new(0.0, -30.0));
        assert!(w.sent("> SET TLEVEL"), "log: {:?}", w.app.log);
    }

    #[test]
    fn screen_view_offset_and_position_in_the_real_window() {
        let mut w = Win::new();
        let g = scope_at(&w, 0.0, 128.0) + Vec2::new(-10.0, 0.0);
        w.drag(g, g + Vec2::new(0.0, 25.0));
        assert!(w.sent("> SET OFFSET"), "log: {:?}", w.app.log);
        let h = scope_at(&w, 150.0, 228.0) + Vec2::new(0.0, -7.0);
        w.drag(h, h + Vec2::new(40.0, 0.0));
        assert!(w.sent("> SET HPOS"), "log: {:?}", w.app.log);
    }

    #[test]
    fn screen_view_cursors_in_the_real_window() {
        let mut w = Win::new();
        w.app.cursors.time_on = true;
        w.app.cursors.volt_on = true;
        w.step(vec![]);
        let t0 = w.app.cursors.t[0];
        let p = scope_at(&w, t0, 128.0);
        w.drag(p, p + Vec2::new(40.0, 0.0));
        assert!(w.app.cursors.t[0] > t0 + 10.0, "{} -> {}", t0, w.app.cursors.t[0]);
    }

    fn deep_capture() -> deep::DeepCapture {
        let info = protocol::DeepInfo { samples: 30100, spp_capture: 20.0, spp_view: 20.0, timebase: 11, timebase_applied: 11, vdiv: 6, ..Default::default() };
        let raw: Vec<u8> = (0..30100).map(|i| (128.0 + (i as f32 / 40.0).sin() * 50.0) as u8).collect();
        deep::DeepCapture::assemble(info, &raw)
    }

    #[test]
    fn full_memory_cursors_and_trigger_in_the_real_window() {
        let mut w = Win::new();
        w.app.mode = ViewMode::Deep;
        w.app.deep_history.push_back(deep_capture());
        w.app.cursors.time_on = true;
        w.app.cursors.volt_on = true;
        for _ in 0..4 {
            w.step(vec![]);
        }
        let plot = w.app.deep_view.plot_rect.expect("deep view was not drawn");
        let (t0, t1) = (w.app.deep_history[0].t_first(), w.app.deep_history[0].t_last());
        let x_of = |t: f64| plot.left() + ((t - t0) / (t1 - t0)) as f32 * plot.width();
        let y_of = |code: f32| plot.bottom() - (code - 28.0) / 200.0 * plot.height();
        // time cursor
        let before = w.app.deep_view.cur_t[0];
        let from = Pos2::new(x_of(before), plot.center().y);
        w.drag(from, from + Vec2::new(60.0, 0.0));
        assert!(w.app.deep_view.cur_t[0] > before, "time cursor did not move");
        // voltage cursor
        let vb = w.app.deep_view.cur_v[1];
        let from = Pos2::new(plot.center().x + 30.0, y_of(vb));
        w.drag(from, from + Vec2::new(0.0, 30.0));
        assert!(w.app.deep_view.cur_v[1] < vb, "voltage cursor did not move");
        // trigger level line (level 0 = screen centre)
        w.app.cursors.time_on = false;
        w.app.cursors.volt_on = false;
        w.step(vec![]);
        let from = Pos2::new(plot.center().x - 40.0, y_of(128.0));
        w.drag(from, from + Vec2::new(0.0, -40.0));
        assert!(w.sent("> SET TLEVEL"), "log: {:?}", w.app.log);
    }

    /// xorshift: deterministic pseudo random numbers
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn f(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo)
        }
    }

    fn random_deep(r: &mut Rng) -> deep::DeepCapture {
        let tb = r.below(18) as u8;
        let info = protocol::DeepInfo {
            seq: r.below(60000) as u16,
            start: if r.below(3) == 0 { r.below(30100) as u16 } else { 0 },
            trig: if r.below(4) == 0 { 0xFFFF } else { r.below(30100) as u16 },
            samples: 30100,
            spp_capture: [0.2, 1.0, 2.0, 20.0][r.below(4) as usize],
            spp_view: 20.0,
            timebase: tb,
            timebase_applied: tb,
            interleave: tb < 4,
            vdiv: r.below(10) as u8,
            probe: r.below(2) as u8,
            trig_level: r.below(200) as i16 - 100,
            hpos: r.below(300) as i16 - 150,
            offset: r.below(200) as i8 - 100,
            ..Default::default()
        };
        let kind = r.below(4);
        let raw: Vec<u8> = (0..30100)
            .map(|i| match kind {
                0 => (128.0 + (i as f32 / 40.0).sin() * 60.0) as u8,
                1 => if (i / 120) % 2 == 0 { 200 } else { 60 },
                2 => (r.below(256)) as u8,
                _ => 128,
            })
            .collect();
        deep::DeepCapture::assemble(info, &raw)
    }

    /// Thousands of random mouse actions on the central area plus random state changes: nothing may panic.
    #[test]
    fn random_use_does_not_panic() {
        for seed in 1..=6u64 {
            let mut r = Rng(0x9E3779B97F4A7C15 ^ seed.wrapping_mul(0x1234567));
            let mut w = Win::new();
            for it in 0..1500u32 {
                // state changes
                match r.below(14) {
                    0 => w.app.mode = if r.below(2) == 0 { ViewMode::Screen } else { ViewMode::Deep },
                    1 => w.app.show_fft = r.below(2) == 0,
                    2 => w.app.fft_window = spectrum::Window::ALL[r.below(4) as usize],
                    3 => w.app.cursors.time_on = r.below(2) == 0,
                    4 => w.app.cursors.volt_on = r.below(2) == 0,
                    5 => w.app.spec_view.log_freq = r.below(2) == 0,
                    6 => w.app.spec_view.db_span = [40.0, 80.0, 140.0][r.below(3) as usize],
                    7 => w.app.show_stats = r.below(2) == 0,
                    8 => w.app.persistence = r.below(65) as usize,
                    9 => {
                        w.app.deep_history.push_back(random_deep(&mut r));
                        if w.app.deep_history.len() > 6 {
                            w.app.deep_history.pop_front();
                        }
                        w.app.deep_idx = if r.below(3) == 0 { Some(r.below(w.app.deep_history.len() as u64) as usize) } else { None };
                    }
                    10 => {
                        let tb = r.below(27) as u8;
                        let trace: Vec<u8> = (0..301).map(|i| match r.below(3) { 0 => (128.0 + (i as f32 / r.f(5.0, 30.0)).sin() * 50.0) as u8, 1 => 128, _ => r.below(256) as u8 }).collect();
                        let (a, b) = if r.below(4) == 0 { (r.below(300) as u16, r.below(300) as u16) } else { (0, 301) };
                        w.app.frame = Some(Frame { trace, timebase: tb, vdiv: r.below(10) as u8, probe: r.below(2) as u8, trace_start: a, trace_end: b, running: true, app_mode: 2, offset: r.below(200) as i8 - 100, trig_level: r.below(200) as i16 - 100, ..Default::default() });
                        if r.below(3) == 0 {
                            let f = w.app.frame.clone().unwrap();
                            w.app.history.push_back(f);
                            w.app.view = if r.below(3) == 0 { Some(r.below(w.app.history.len() as u64) as usize) } else { None };
                        }
                    }
                    11 => {
                        w.app.deep_view.fit_all();
                        let n = r.below(120) as usize;
                        let samples: Vec<u8> = (0..n).map(|_| r.below(256) as u8).collect();
                        let flags = [1u8, 1, 3][r.below(3) as usize];
                        w.app.roll.ingest(protocol::RollChunk { index: r.below(3) as u32 * 1000 + r.below(50) as u32, timebase: 18 + r.below(3) as u8, vdiv: 6, flags, samples, ..Default::default() });
                        w.app.status.as_mut().unwrap().timebase = if r.below(2) == 0 { 19 } else { 13 };
                    }
                    12 => w.app.spec_view.reset(),
                    _ => {}
                }
                // mouse: only the central area (the top/bottom bars hold buttons that open folders / write files)
                let pos = Pos2::new(r.f(20.0, 980.0), r.f(70.0, 740.0));
                match r.below(5) {
                    0 => w.move_to(pos),
                    1 => {
                        let to = Pos2::new(r.f(20.0, 980.0), r.f(70.0, 740.0));
                        w.drag(pos, to);
                    }
                    2 => {
                        w.move_to(pos);
                        w.step(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, r.f(-400.0, 400.0)), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers: Default::default() }]);
                    }
                    3 => {
                        w.move_to(pos);
                        w.button(true);
                        w.button(false);
                        w.button(true);
                        w.button(false);
                    }
                    _ => w.step(vec![]),
                }
                if it % 500 == 0 {
                    w.step(vec![]);
                }
            }
        }
    }

    /// Crash seen in the field: zoom/pan the spectrum on the linear axis, then switch to the log axis.
    #[test]
    fn spectrum_zoom_then_log_axis_does_not_hang() {
        let mut w = Win::new();
        w.app.show_fft = true;
        for _ in 0..4 {
            w.step(vec![]);
        }
        // zoom in with the wheel over the spectrum, drag it to the left edge
        let p = Pos2::new(400.0, 640.0);
        w.move_to(p);
        for _ in 0..4 {
            w.step(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, 300.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers: Default::default() }]);
        }
        w.drag(p, p + Vec2::new(600.0, 0.0));
        w.app.spec_view.log_freq = true;
        for _ in 0..4 {
            w.step(vec![]);
        }
        w.app.spec_view.log_freq = false;
        w.step(vec![]);
    }

    fn roll_win() -> (Win, u32) {
        let mut w = Win::new();
        w.app.status = Some(Status { version: 4, app_mode: 2, timebase: 19, vdiv: 6, running: true, ..Default::default() });
        w.app.frame.as_mut().unwrap().timebase = 19;
        w.app.fw_version = 4;
        w.app.mode = ViewMode::Deep;
        w.app.deep_view.restart_follow();
        (w, 0)
    }

    fn roll_feed(w: &mut Win, index: &mut u32, n: usize, flags: u8) {
        let samples: Vec<u8> = (0..n).map(|i| (128.0 + ((*index as usize + i) as f32 / 30.0).sin() * 50.0) as u8).collect();
        w.app.roll.ingest(protocol::RollChunk { index: *index, timebase: 19, vdiv: 6, flags, samples, ..Default::default() });
        *index += n as u32;
    }

    fn wheel(w: &mut Win, dy: f32) {
        w.step(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, dy), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers: Default::default() }]);
    }

    /// Reported: zooming out while streaming jumped back to the initial zoom instead of staying zoomed out.
    #[test]
    fn roll_stream_zoom_out_stays_zoomed_out() {
        let (mut w, mut index) = roll_win();
        roll_feed(&mut w, &mut index, 3000, 3);
        for _ in 0..4 {
            w.step(vec![]);
        }
        let plot = w.app.deep_view.plot_rect.expect("stream view was not drawn");
        let span_of = |w: &Win| { let c = w.app.roll.capture().unwrap(); w.app.deep_view.window().map(|(a, b)| b - a).unwrap_or(c.t_last() - c.t_first()) };
        let initial = span_of(&w);
        w.move_to(plot.center());
        let mut last = initial;
        for _ in 0..4 {
            wheel(&mut w, -200.0);                       // zoom out
            for _ in 0..3 {
                w.step(vec![]);
            }
            let now = span_of(&w);
            assert!(now >= last * 0.999, "zooming out made the view smaller: {last} -> {now}");
            last = now;
        }
        assert!(last > initial * 3.0, "never zoomed out: {initial} -> {last}");
        // more samples arrive: the zoomed-out view must not snap back
        roll_feed(&mut w, &mut index, 300, 1);
        for _ in 0..6 {
            w.step(vec![]);
        }
        assert!(span_of(&w) >= last * 0.999, "view snapped back after new samples: {last} -> {}", span_of(&w));
        // zooming in goes towards the newest sample and the view keeps following
        wheel(&mut w, 200.0);
        for _ in 0..3 {
            w.step(vec![]);
        }
        let c = w.app.roll.capture().unwrap();
        if let Some((_, b)) = w.app.deep_view.window() {
            assert!((b - c.t_last()).abs() < c.dt * 2.0, "zoomed view is not at the newest sample");
        }
    }

    #[test]
    fn battery_indicator_renders_for_every_level() {
        let mut w = Win::new();
        for (pct, flags, mv) in [(100u8, 1u8, 4100u16), (60, 0, 3820), (30, 0, 3740), (15, 0, 3680), (5, 0, 3500), (0, 3, 0)] {
            w.app.status = Some(Status { version: 4, app_mode: 2, timebase: 13, vdiv: 6, running: true, has_battery: true, battery_pct: pct, battery_flags: flags, battery_mv: mv, ..Default::default() });
            for _ in 0..3 {
                w.step(vec![]);
            }
        }
        // hovering the icon (top bar) must not panic either
        w.move_to(Pos2::new(600.0, 20.0));
        w.step(vec![]);
    }

    #[test]
    fn roll_stream_view_follows_live_and_stops_following_when_panned() {
        let mut w = Win::new();
        w.app.status = Some(Status { version: 3, app_mode: 2, timebase: 19, vdiv: 6, running: true, ..Default::default() });
        w.app.frame.as_mut().unwrap().timebase = 19;
        w.app.mode = ViewMode::Deep;
        w.app.deep_view.restart_follow();
        let mut index = 0u32;
        let mut feed = |w: &mut Win, n: usize, flags: u8| {
            let samples: Vec<u8> = (0..n).map(|i| (128.0 + ((index as usize + i) as f32 / 30.0).sin() * 50.0) as u8).collect();
            w.app.roll.ingest(protocol::RollChunk { index, timebase: 19, vdiv: 6, flags, samples, ..Default::default() });
            index += n as u32;
        };
        feed(&mut w, 400, 3);
        for _ in 0..4 {
            w.step(vec![]);
        }
        let plot = w.app.deep_view.plot_rect.expect("stream view was not drawn");
        let c0 = w.app.roll.capture().unwrap().clone();
        let (_, end0) = w.app.deep_view.window().unwrap_or((0.0, c0.t_last()));
        assert!((end0 - c0.t_last()).abs() < c0.dt * 2.0, "window end {end0} vs {}", c0.t_last());
        // more samples arrive: the window follows the newest one
        feed(&mut w, 300, 1);
        for _ in 0..3 {
            w.step(vec![]);
        }
        let c1 = w.app.roll.capture().unwrap().clone();
        let (_, end1) = w.app.deep_view.window().unwrap_or((0.0, c1.t_last()));
        assert!((end1 - c1.t_last()).abs() < c1.dt * 2.0, "did not follow: {end1} vs {}", c1.t_last());
        // the user drags the view into the past: following stops
        w.drag(plot.center(), plot.center() + Vec2::new(150.0, 0.0));
        assert!(!w.app.deep_view.follow_end, "dragging should stop following");
        let before = w.app.deep_view.window();
        feed(&mut w, 300, 1);
        for _ in 0..3 {
            w.step(vec![]);
        }
        assert_eq!(w.app.deep_view.window(), before, "view moved although following is off");
    }

    #[test]
    fn full_memory_wheel_zoom_and_pan_in_the_real_window() {
        let mut w = Win::new();
        w.app.mode = ViewMode::Deep;
        w.app.deep_history.push_back(deep_capture());
        for _ in 0..3 {
            w.step(vec![]);
        }
        let plot = w.app.deep_view.plot_rect.expect("deep view was not drawn");
        // wheel zoom
        w.move_to(plot.center());
        w.step(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, 200.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers: Default::default() }]);
        for _ in 0..3 {
            w.step(vec![]);
        }
        let zoomed = w.app.deep_view.window();
        assert!(zoomed.is_some(), "wheel did not zoom");
        // pan by dragging
        w.drag(plot.center(), plot.center() + Vec2::new(-150.0, 0.0));
        let panned = w.app.deep_view.window();
        assert!(panned.is_some() && panned != zoomed, "drag did not pan: {zoomed:?} -> {panned:?}");
    }
}

/// Allocator guard: a single request for more than 1 GiB means a runaway loop. The program never needs that
/// much; it is reported (crash.log) and the process stops, instead of the system killing it silently.
/// In tests it panics instead, which gives a backtrace of the culprit.
mod guard_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    pub struct Guard;
    const LIMIT: usize = 1 << 30;
    fn too_big(n: usize) -> ! {
        #[cfg(test)]
        panic!("huge allocation: {n} bytes");
        #[cfg(not(test))]
        dso510_link::crashlog::huge_allocation(n)
    }
    unsafe impl GlobalAlloc for Guard {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if l.size() > LIMIT {
                too_big(l.size());
            }
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            if n > LIMIT {
                too_big(n);
            }
            unsafe { System.realloc(p, l, n) }
        }
    }
}
#[global_allocator]
static GUARD: guard_alloc::Guard = guard_alloc::Guard;
