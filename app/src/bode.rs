//! Frequency response (Bode magnitude plot) with the scope's own signal generator.
//!
//! The generator sweeps a sine from `f_start` to `f_stop` (logarithmic steps); for every frequency the
//! scope's time base is set to show a few periods, the V/div is adjusted until the signal fills the
//! screen without clipping, and the amplitude is measured from several screen frames. With one channel
//! there is no phase, only the magnitude: the gain is the measured amplitude divided by a reference
//! sweep (probe on the generator output / the circuit input), which also cancels the generator's own
//! frequency response and the input coupling.
use crate::protocol::{Frame, PX_PER_DIV, TIMEBASE_SECONDS, TIME_SCALE};
use std::time::{Duration, Instant};

/// One measured frequency.
#[derive(Clone, Debug)]
pub struct Point {
    /// generator setting
    pub set_hz: f64,
    /// frequency measured by the scope (the generator cannot make every value exactly)
    pub hz: f64,
    /// amplitude, volts peak (half of peak-peak)
    pub amp: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Target {
    Reference,
    Measurement,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    Prepare,
    Settle,
    Collect,
}

struct Run {
    target: Target,
    freqs: Vec<f64>,
    idx: usize,
    stage: Stage,
    since: Instant,
    vdiv: u8,
    tb: u8,
    seen_seq: Option<u16>,
    amps: Vec<f64>,
    hz: Vec<f64>,
    tries: u8,
    points: Vec<Point>,
}

pub struct Bode {
    pub f_start: f64,
    pub f_stop: f64,
    pub steps: usize,
    /// generator amplitude (V, the scope's GAMP setting)
    pub gen_amp: f64,
    pub reference: Vec<Point>,
    pub measured: Vec<Point>,
    run: Option<Run>,
    pub status: String,
    /// commands that put the scope and the generator back as they were before the sweep
    restore: Vec<String>,
}

impl Default for Bode {
    fn default() -> Self {
        Bode { f_start: 20.0, f_stop: 50_000.0, steps: 30, gen_amp: 1.0, reference: Vec::new(), measured: Vec::new(), run: None,
               status: String::new(), restore: Vec::new() }
    }
}

/// time base index whose screen (12 div) holds at least ~2.5 periods; capped at 20 ms/div (roll mode above)
fn timebase_for(hz: f64) -> u8 {
    (0..=17u8).find(|&i| TIMEBASE_SECONDS[i as usize] * TIME_SCALE * 12.0 >= 2.5 / hz).unwrap_or(17)
}

/// amplitude of one frame in ADC codes (peak-peak / 2, from the 2nd / 98th percentile against noise) and
/// whether it clips (the scope limits the trace to 28..228)
fn frame_amplitude(f: &Frame) -> Option<(f64, bool)> {
    let mut v: Vec<u8> = f.trace.iter().enumerate().filter(|(i, _)| f.valid(*i)).map(|(_, &c)| c).collect();
    if v.len() < 50 {
        return None;
    }
    v.sort_unstable();
    let lo = v[v.len() * 2 / 100] as f64;
    let hi = v[v.len() * 98 / 100] as f64;
    let clipped = v[v.len() - 1] >= 228 || v[0] <= 28;
    Some(((hi - lo) / 2.0, clipped))
}

impl Bode {
    pub fn running(&self) -> Option<(Target, usize, usize)> {
        self.run.as_ref().map(|r| (r.target, r.idx, r.freqs.len()))
    }

    pub fn frequencies(&self) -> Vec<f64> {
        let n = self.steps.max(2);
        let (a, b) = (self.f_start.max(1.0).ln(), self.f_stop.max(self.f_start + 1.0).ln());
        let mut v: Vec<f64> = (0..n).map(|i| (a + (b - a) * i as f64 / (n - 1) as f64).exp().round()).collect();
        v.dedup();
        v
    }

    /// start a sweep; `send` gets the commands for the scope, `restore` is sent when the sweep ends or stops
    pub fn start(&mut self, target: Target, vdiv_now: u8, restore: Vec<String>, send: &mut dyn FnMut(String)) {
        self.restore = restore;
        for c in ["SET SCREEN 3", "SET GWAVE 0", "SET GOUT 1"] {
            send(c.into());
        }
        send(format!("SET GAMP {}", (self.gen_amp * 10.0).round().clamp(1.0, 30.0) as i32));
        for c in ["SET SCREEN 2", "SET TMODE 1", "SET RUN 1", "SET TLEVEL 0", "SET OFFSET 0", "SET HPOS 0", "DEEP 0"] {
            send(c.into());
        }
        let freqs = self.frequencies();
        if target == Target::Measurement {
            self.measured.clear();
        } else {
            self.reference.clear();
        }
        self.run = Some(Run { target, freqs, idx: 0, stage: Stage::Prepare, since: Instant::now(), vdiv: vdiv_now.min(9), tb: 0,
                              seen_seq: None, amps: Vec::new(), hz: Vec::new(), tries: 0, points: Vec::new() });
    }

    pub fn stop(&mut self, send: &mut dyn FnMut(String)) {
        if let Some(r) = self.run.take() {
            self.store(r.target, r.points);
            self.status = "stopped".into();
            for c in self.restore.drain(..) {
                send(c);
            }
        }
    }

    fn store(&mut self, target: Target, points: Vec<Point>) {
        match target {
            Target::Reference => self.reference = points,
            Target::Measurement => self.measured = points,
        }
    }

    /// advance the sweep: called on every UI update with the newest screen frame
    pub fn tick(&mut self, frame: Option<&Frame>, send: &mut dyn FnMut(String)) {
        let Some(r) = self.run.as_mut() else { return };
        let now = Instant::now();
        match r.stage {
            Stage::Prepare => {
                let f = r.freqs[r.idx];
                r.tb = timebase_for(f);
                send(format!("SET GFREQ {}", f as i32));
                send(format!("SET TB {}", r.tb));
                send(format!("SET VDIV {}", r.vdiv));
                r.stage = Stage::Settle;
                r.since = now;
                r.amps.clear();
                r.hz.clear();
            }
            Stage::Settle => {
                // two frequency-counter windows (0.5 s each) and the AC coupling settle
                if now.duration_since(r.since) >= Duration::from_millis(1100) {
                    r.stage = Stage::Collect;
                    r.since = now;
                    r.seen_seq = frame.map(|f| f.seq);
                }
            }
            Stage::Collect => {
                let Some(f) = frame else { return };
                if Some(f.seq) == r.seen_seq || f.timebase != r.tb || f.vdiv != r.vdiv {
                    if now.duration_since(r.since) > Duration::from_secs(4) {
                        r.stage = Stage::Prepare;             // the scope did not follow: send the settings again
                    }
                    return;
                }
                r.seen_seq = Some(f.seq);
                let Some((amp_codes, clipped)) = frame_amplitude(f) else { return };
                // auto range: no clipping, and the signal over ~1/3 of the screen
                if clipped && r.vdiv < 9 && r.tries < 8 {
                    r.vdiv += 1;
                    r.tries += 1;
                    r.stage = Stage::Prepare;
                    return;
                }
                if !clipped && amp_codes < 16.0 && r.vdiv > 0 && r.tries < 8 {
                    r.vdiv -= 1;
                    r.tries += 1;
                    r.stage = Stage::Prepare;
                    return;
                }
                r.amps.push(amp_codes * f.volts_per_div() / PX_PER_DIV);
                if f.freq_raw > 0 {
                    r.hz.push(f.freq_raw as f64);
                }
                if r.amps.len() >= 6 {
                    let set_hz = r.freqs[r.idx];
                    let amp = r.amps.iter().sum::<f64>() / r.amps.len() as f64;
                    // the generator only makes some frequencies exactly: the measured value, if plausible
                    let hz = if r.hz.is_empty() { set_hz } else { r.hz.iter().sum::<f64>() / r.hz.len() as f64 };
                    let hz = if (hz / set_hz - 1.0).abs() < 0.2 { hz } else { set_hz };
                    r.points.push(Point { set_hz, hz, amp });
                    r.idx += 1;
                    r.tries = 0;
                    r.stage = Stage::Prepare;
                    if r.idx >= r.freqs.len() {
                        let r = self.run.take().unwrap();
                        let n = r.points.len();
                        self.store(r.target, r.points);
                        self.status = format!("done: {n} points");
                        for c in self.restore.drain(..) {
                            send(c);
                        }
                    }
                }
            }
        }
    }

    /// gain in dB of the measurement against the reference (the same generator settings), else against
    /// the generator amplitude
    pub fn gain_db(&self) -> Vec<(f64, f64)> {
        self.measured
            .iter()
            .map(|p| {
                let r = self.reference.iter().find(|r| r.set_hz == p.set_hz).map(|r| r.amp).filter(|&a| a > 0.0);
                let base = r.unwrap_or(self.gen_amp);
                (p.hz, 20.0 * (p.amp.max(1e-9) / base).log10())
            })
            .collect()
    }

    pub fn has_matching_reference(&self) -> bool {
        !self.reference.is_empty() && self.measured.iter().all(|p| self.reference.iter().any(|r| r.set_hz == p.set_hz))
    }

    /// frequencies where the gain crosses (maximum - 3 dB), linear interpolation on log f
    pub fn cutoffs(&self) -> (Option<(f64, f64)>, Vec<f64>) {
        let g = self.gain_db();
        let Some(&(fmax, gmax)) = g.iter().max_by(|a, b| a.1.total_cmp(&b.1)) else { return (None, Vec::new()) };
        let level = gmax - 3.0;
        let mut out = Vec::new();
        for w in g.windows(2) {
            let ((f1, g1), (f2, g2)) = (w[0], w[1]);
            if (g1 - level) * (g2 - level) < 0.0 {
                let t = (level - g1) / (g2 - g1);
                out.push((f1.ln() + t * (f2.ln() - f1.ln())).exp());
            }
        }
        (Some((fmax, gmax)), out)
    }

    pub fn csv(&self) -> String {
        let mut s = String::from("set_hz;measured_hz;amplitude_v_peak;reference_v_peak;gain_db\n");
        let g = self.gain_db();
        for (p, (_, db)) in self.measured.iter().zip(g) {
            let r = self.reference.iter().find(|r| r.set_hz == p.set_hz).map(|r| format!("{:.6}", r.amp)).unwrap_or_default();
            s += &format!("{:.0};{:.1};{:.6};{};{:.2}\n", p.set_hz, p.hz, p.amp, r, db);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_are_log_spaced_and_round() {
        let b = Bode { f_start: 20.0, f_stop: 20000.0, steps: 4, ..Default::default() };
        assert_eq!(b.frequencies(), vec![20.0, 200.0, 2000.0, 20000.0]);
    }

    #[test]
    fn timebase_shows_a_few_periods() {
        for f in [20.0, 100.0, 1000.0, 10000.0, 50000.0] {
            let tb = timebase_for(f) as usize;
            let screen = TIMEBASE_SECONDS[tb] * TIME_SCALE * 12.0;
            assert!(screen * f >= 2.5 && (tb == 0 || TIMEBASE_SECONDS[tb - 1] * TIME_SCALE * 12.0 * f < 2.5), "{f} Hz -> tb {tb}");
        }
    }

    #[test]
    fn cutoff_of_a_first_order_low_pass() {
        let fc = 1000.0;
        let mut b = Bode::default();
        for f in [10.0, 100.0, 300.0, 700.0, 1000.0, 1500.0, 3000.0, 10000.0] {
            let gain = 1.0 / (1.0 + (f / fc as f64).powi(2)).sqrt();
            b.reference.push(Point { set_hz: f, hz: f, amp: 1.0 });
            b.measured.push(Point { set_hz: f, hz: f, amp: gain });
        }
        let (max, cut) = b.cutoffs();
        assert!(max.unwrap().1.abs() < 0.01);
        assert_eq!(cut.len(), 1);
        assert!((cut[0] / fc - 1.0).abs() < 0.05, "{cut:?}");
    }
}

// ---------------------------------------------------------------------------------------------------
// plot

use crate::i18n::tr;
use crate::measure::eng;
use crate::scope::{AXIS_COLOR, GRID_COLOR};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke};

const BG: Color32 = Color32::from_rgb(8, 10, 12);
const GAIN_COLOR: Color32 = Color32::from_rgb(90, 200, 255);
const REF_COLOR: Color32 = Color32::from_rgb(150, 150, 150);
const CUT_COLOR: Color32 = Color32::from_rgb(255, 170, 60);

impl Bode {
    /// gain (dB) over log frequency; the reference amplitude (dB re its first point) faint behind it
    pub fn show(&self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        let (resp, painter) = ui.allocate_painter(avail, Sense::hover());
        let full = resp.rect;
        painter.rect_filled(full, 6.0, BG);
        let gain = self.gain_db();
        let refc: Vec<(f64, f64)> = match self.reference.first() {
            Some(r0) if r0.amp > 0.0 => self.reference.iter().map(|p| (p.hz, 20.0 * (p.amp.max(1e-9) / r0.amp).log10())).collect(),
            _ => Vec::new(),
        };
        let progress = self.running().map(|(t, i, n)| {
            let what = if t == Target::Reference { tr("reference") } else { tr("measurement") };
            format!("{what}: {} / {n}", i)
        });
        if gain.is_empty() && refc.is_empty() {
            let msg = progress.clone().unwrap_or_else(|| tr("No frequency response yet: see \"How to use\" in the panel on the right").to_string());
            painter.text(full.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), Color32::from_gray(140));
            return;
        }
        let plot = Rect::from_min_max(Pos2::new(full.left() + 56.0, full.top() + 24.0), Pos2::new(full.right() - 16.0, full.bottom() - 26.0));
        if plot.width() < 60.0 || plot.height() < 40.0 {
            return;
        }
        let (f0, f1) = (self.f_start.min(gain.first().map(|g| g.0).unwrap_or(f64::MAX)).max(1.0), self.f_stop.max(gain.last().map(|g| g.0).unwrap_or(0.0)));
        let (lf0, lf1) = (f0.ln(), f1.max(f0 * 1.01).ln());
        let all = gain.iter().chain(refc.iter()).map(|g| g.1);
        let (mut dmin, mut dmax) = all.fold((f64::MAX, f64::MIN), |(a, b), d| (a.min(d), b.max(d)));
        dmax = (dmax / 6.0).ceil() * 6.0 + 3.0;
        dmin = ((dmin / 6.0).floor() * 6.0 - 3.0).min(dmax - 12.0);
        let x = |f: f64| plot.left() + ((f.max(1e-9).ln() - lf0) / (lf1 - lf0)) as f32 * plot.width();
        let y = |d: f64| plot.bottom() - ((d - dmin) / (dmax - dmin)) as f32 * plot.height();
        // grid: dB every 6 (or 12 / 20), decades and 2 / 5
        let step = if dmax - dmin > 80.0 { 20.0 } else if dmax - dmin > 40.0 { 12.0 } else { 6.0 };
        let mut d = (dmin / step).ceil() * step;
        while d <= dmax {
            let yy = y(d);
            painter.line_segment([Pos2::new(plot.left(), yy), Pos2::new(plot.right(), yy)], Stroke::new(1.0, if d == 0.0 { AXIS_COLOR } else { GRID_COLOR }));
            painter.text(Pos2::new(plot.left() - 6.0, yy), Align2::RIGHT_CENTER, format!("{d:+.0} dB"), FontId::monospace(10.0), Color32::from_gray(150));
            d += step;
        }
        let mut dec = 10f64.powf(f0.log10().floor());
        while dec <= f1 {
            for m in [1.0, 2.0, 5.0] {
                let f = dec * m;
                if f < f0 || f > f1 {
                    continue;
                }
                let xx = x(f);
                painter.line_segment([Pos2::new(xx, plot.top()), Pos2::new(xx, plot.bottom())], Stroke::new(1.0, if m == 1.0 { AXIS_COLOR } else { GRID_COLOR }));
                painter.text(Pos2::new(xx, plot.bottom() + 5.0), Align2::CENTER_TOP, eng(f, "Hz"), FontId::monospace(10.0), Color32::from_gray(150));
            }
            dec *= 10.0;
        }
        let line = |pts: &[(f64, f64)], c: Color32, w: f32| {
            let p: Vec<Pos2> = pts.iter().map(|&(f, d)| Pos2::new(x(f), y(d))).collect();
            if p.len() > 1 {
                painter.add(Shape::line(p.clone(), Stroke::new(w, c)));
            }
            for q in p {
                painter.circle_filled(q, 2.5, c);
            }
        };
        if !refc.is_empty() && !self.has_matching_reference() || gain.is_empty() {
            line(&refc, REF_COLOR, 1.0);
        }
        line(&gain, GAIN_COLOR, 2.0);
        // maximum and -3 dB points
        let (max, cuts) = self.cutoffs();
        let mut caption = Vec::new();
        if let Some((fm, gm)) = max {
            painter.add(Shape::dashed_line(&[Pos2::new(plot.left(), y(gm - 3.0)), Pos2::new(plot.right(), y(gm - 3.0))], Stroke::new(1.0, CUT_COLOR), 4.0, 4.0));
            caption.push(format!("{} {} ({:+.1} dB)", tr("max at"), eng(fm, "Hz"), gm));
        }
        for c in &cuts {
            let xx = x(*c);
            painter.add(Shape::dashed_line(&[Pos2::new(xx, plot.top()), Pos2::new(xx, plot.bottom())], Stroke::new(1.0, CUT_COLOR), 4.0, 4.0));
            caption.push(format!("-3 dB: {}", eng(*c, "Hz")));
        }
        if !self.has_matching_reference() && !gain.is_empty() {
            caption.push(tr("no reference: gain against the generator setting (approximate)").to_string());
        }
        if let Some(p) = progress {
            caption.push(p);
        }
        painter.text(Pos2::new(plot.left(), full.top() + 6.0), Align2::LEFT_TOP, caption.join("   "), FontId::proportional(12.0), CUT_COLOR);
        // readout under the mouse: the nearest point
        if let Some(m) = resp.hover_pos().filter(|m| plot.contains(*m)) {
            if let Some(&(f, d)) = gain.iter().min_by(|a, b| (x(a.0) - m.x).abs().total_cmp(&(x(b.0) - m.x).abs())) {
                let p = Pos2::new(x(f), y(d));
                painter.circle_stroke(p, 5.0, Stroke::new(1.5, Color32::WHITE));
                let amp = self.measured.iter().find(|q| q.hz == f).map(|q| eng(q.amp * 2.0, "Vpp")).unwrap_or_default();
                painter.text(p + egui::vec2(8.0, -8.0), Align2::LEFT_BOTTOM, format!("{}  {:+.2} dB  {}", eng(f, "Hz"), d, amp),
                             FontId::monospace(11.0), Color32::WHITE);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// connection diagram for the help: step 1 probe on the circuit input, step 2 on its output

const WIRE: Color32 = Color32::from_rgb(230, 200, 60);    // generator signal
const PROBE: Color32 = Color32::from_rgb(235, 80, 80);    // probe tip
const GND: Color32 = Color32::from_rgb(140, 140, 140);
const BOX: Color32 = Color32::from_rgb(120, 130, 140);

/// `step` 1: reference (probe on the circuit input), 2: measurement (probe on the output)
pub fn connection_diagram(ui: &mut egui::Ui, step: u8) {
    let w = ui.available_width().clamp(240.0, 420.0);
    let (resp, p) = ui.allocate_painter(egui::vec2(w, 150.0), Sense::hover());
    let r = resp.rect;
    p.rect_filled(r, 6.0, BG);
    let s = w / 320.0;                                          // drawn for 320 px, scaled
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * s, r.top() + y);
    let text = |pos: Pos2, t: &str, c: Color32, align: Align2| {
        p.text(pos, align, t, FontId::proportional(11.0), c);
    };
    let stroke = |c: Color32| Stroke::new(2.0, c);

    // the scope
    let scope = Rect::from_min_max(at(8.0, 22.0), at(92.0, 122.0));
    p.rect_stroke(scope, 4.0, Stroke::new(1.5, BOX), egui::StrokeKind::Inside);
    text(at(50.0, 16.0), "DSO-510", Color32::from_gray(200), Align2::CENTER_BOTTOM);
    let gen_out = at(92.0, 44.0);
    let ch = at(92.0, 76.0);
    let gnd_s = at(92.0, 108.0);
    for (pt, lbl) in [(gen_out, tr("generator out")), (ch, tr("probe input")), (gnd_s, tr("ground"))] {
        p.circle_filled(pt, 3.5, Color32::from_gray(220));
        text(pt - egui::vec2(6.0, 0.0), lbl, Color32::from_gray(170), Align2::RIGHT_CENTER);
    }

    // the circuit: an RC low-pass as the example
    let cbox = Rect::from_min_max(at(170.0, 22.0), at(300.0, 122.0));
    p.rect_stroke(cbox, 4.0, Stroke::new(1.5, BOX), egui::StrokeKind::Inside);
    text(at(235.0, 16.0), tr("your circuit (e.g. RC low-pass)"), Color32::from_gray(200), Align2::CENTER_BOTTOM);
    let cin = at(170.0, 44.0);
    let cout = at(300.0, 44.0);
    let cgnd = at(170.0, 108.0);
    // resistor (zigzag) from IN to the node, capacitor from the node to ground
    let node = at(262.0, 44.0);
    p.line_segment([cin, at(196.0, 44.0)], stroke(Color32::from_gray(200)));
    let zig: Vec<Pos2> = (0..=8).map(|i| at(196.0 + i as f32 * 5.0, 44.0 + if i == 0 || i == 8 { 0.0 } else if i % 2 == 1 { -5.0 } else { 5.0 })).collect();
    p.add(Shape::line(zig, stroke(Color32::from_gray(200))));
    p.line_segment([at(236.0, 44.0), cout], stroke(Color32::from_gray(200)));
    text(at(216.0, 34.0), "R", Color32::from_gray(170), Align2::CENTER_BOTTOM);
    p.line_segment([node, at(262.0, 72.0)], stroke(Color32::from_gray(200)));
    p.line_segment([at(252.0, 72.0), at(272.0, 72.0)], stroke(Color32::from_gray(200)));
    p.line_segment([at(252.0, 78.0), at(272.0, 78.0)], stroke(Color32::from_gray(200)));
    p.line_segment([at(262.0, 78.0), at(262.0, 108.0)], stroke(Color32::from_gray(200)));
    text(at(276.0, 75.0), "C", Color32::from_gray(170), Align2::LEFT_CENTER);
    p.line_segment([cgnd, at(262.0, 108.0)], stroke(GND));
    for (pt, lbl, a) in [(cin, tr("IN"), Align2::LEFT_BOTTOM), (cout, tr("OUT"), Align2::RIGHT_BOTTOM)] {
        p.circle_filled(pt, 3.5, Color32::from_gray(220));
        text(pt + egui::vec2(if a == Align2::LEFT_BOTTOM { 4.0 } else { -4.0 }, -4.0), lbl, Color32::from_gray(170), a);
    }

    // wires: generator -> IN, ground -> ground
    p.line_segment([gen_out, cin], stroke(WIRE));
    p.line_segment([gnd_s, cgnd], stroke(GND));
    text(at(131.0, 40.0), tr("signal"), WIRE, Align2::CENTER_BOTTOM);
    text(at(124.0, 92.0), tr("ground"), GND, Align2::CENTER_BOTTOM);
    text(at(124.0, 104.0), tr("(probe clip)"), GND, Align2::CENTER_BOTTOM);

    // the probe: from the probe input to IN (step 1) or OUT (step 2)
    let target = if step == 1 { cin + egui::vec2(10.0 * s, 0.0) } else { cout };
    let bend = if step == 1 { at(150.0, 76.0) } else { at(150.0, 136.0) };
    let path = if step == 1 {
        vec![ch, bend, Pos2::new(target.x, bend.y), target + egui::vec2(0.0, 6.0)]
    } else {
        vec![ch, at(158.0, 76.0), at(158.0, 136.0), at(310.0, 136.0), Pos2::new(at(310.0, 0.0).x, cout.y), cout + egui::vec2(6.0, 0.0)]
    };
    p.add(Shape::line(path, Stroke::new(2.0, PROBE)));
    p.circle_filled(target, 4.0, PROBE);
    let label = if step == 1 { tr("probe here (step 1: reference)") } else { tr("probe here (step 2: measurement)") };
    let lp = if step == 1 { at(235.0, 140.0) } else { at(235.0, 150.0) };
    text(lp, label, PROBE, Align2::CENTER_BOTTOM);
}
