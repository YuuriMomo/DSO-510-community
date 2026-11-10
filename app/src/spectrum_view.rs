//! Spectrum plot: magnitude (dBV) over frequency, linear or logarithmic axis, wheel zoom / drag pan,
//! fundamental and harmonic markers, readout under the mouse.

use crate::i18n::tr;
use crate::measure::eng;
use crate::scope::{AXIS_COLOR, GRID_COLOR, TRIGGER_COLOR};
use crate::spectrum::Spectrum;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};

const BG: Color32 = Color32::from_rgb(8, 10, 12);
const SPEC_COLOR: Color32 = Color32::from_rgb(90, 200, 255);

pub struct SpectrumView {
    /// visible frequency range (Hz); None = 0 .. Nyquist
    range: Option<(f64, f64)>,
    pub log_freq: bool,
    /// axis type of the last frame: a zoom range of the other axis type is dropped
    last_log: bool,
    /// dB span shown below the top line
    pub db_span: f32,
}

impl Default for SpectrumView {
    fn default() -> Self {
        Self { range: None, log_freq: false, last_log: false, db_span: 80.0 }
    }
}

fn nice_step(span: f64, target: f64) -> f64 {
    if !(span > 0.0) || !span.is_finite() || !(target > 0.0) {
        return 1.0;
    }
    let raw = span / target;
    let p = 10f64.powf(raw.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if raw <= m * p {
            return m * p;
        }
    }
    10.0 * p
}

impl SpectrumView {
    pub fn reset(&mut self) {
        self.range = None;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, s: Option<&Spectrum>) {
        let avail = ui.available_size();
        let (resp, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
        let full = resp.rect;
        painter.rect_filled(full, 6.0, BG);
        let Some(s) = s.filter(|s| s.db.len() > 8) else {
            painter.text(full.center(), Align2::CENTER_CENTER, tr("No spectrum yet"), FontId::proportional(14.0), Color32::from_gray(140));
            return;
        };
        let plot = Rect::from_min_max(Pos2::new(full.left() + 56.0, full.top() + 10.0), Pos2::new(full.right() - 16.0, full.bottom() - 26.0));
        if plot.width() < 60.0 || plot.height() < 40.0 {
            return;
        }
        let nyq = s.nyquist();
        let fmin_log = s.df.max(nyq / 1e5);
        if self.last_log != self.log_freq {
            self.last_log = self.log_freq;
            self.range = None;                       // a linear zoom (from 0 Hz) makes no sense on a log axis
        }
        let (mut f0, mut f1) = self.range.unwrap_or((if self.log_freq { fmin_log } else { 0.0 }, nyq));
        if self.log_freq {
            f0 = f0.max(fmin_log);
        }

        // interaction: wheel zoom around the mouse, drag pan, double-click: whole range
        let to_u = |f: f64, log: bool, a: f64, b: f64| if log { (f.max(1e-12).ln() - a.ln()) / (b.ln() - a.ln()) } else { (f - a) / (b - a) };
        let from_u = |u: f64, log: bool, a: f64, b: f64| if log { (a.ln() + u * (b.ln() - a.ln())).exp() } else { a + u * (b - a) };
        if let Some(p) = resp.hover_pos().filter(|p| plot.contains(*p)) {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                let k = (-(scroll as f64) / 200.0).exp();
                let u = ((p.x - plot.left()) / plot.width()) as f64;
                let fm = from_u(u, self.log_freq, f0, f1);
                let (mut a, mut b) = if self.log_freq {
                    ((fm.ln() - (fm.ln() - f0.ln()) * k).exp(), (fm.ln() + (f1.ln() - fm.ln()) * k).exp())
                } else {
                    (fm - (fm - f0) * k, fm + (f1 - fm) * k)
                };
                let lo = if self.log_freq { fmin_log } else { 0.0 };
                a = a.max(lo);
                b = b.min(nyq);
                if b - a > s.df * 8.0 {
                    self.range = Some((a, b));
                }
            }
        }
        if resp.dragged() {
            let du = -(resp.drag_delta().x / plot.width()) as f64;
            let lo = if self.log_freq { fmin_log } else { 0.0 };
            let (a, b) = if self.log_freq {
                let r = (f1.ln() - f0.ln()) * du;
                ((f0.ln() + r).exp(), (f1.ln() + r).exp())
            } else {
                let r = (f1 - f0) * du;
                (f0 + r, f1 + r)
            };
            if a >= lo && b <= nyq {
                self.range = Some((a, b));
            }
        }
        if resp.double_clicked() {
            self.range = None;
        }
        if let Some(r) = self.range {
            (f0, f1) = r;
        }
        // a valid axis whatever happened above: log axes need f0 > 0, and f1 must stay above f0
        if self.log_freq {
            f0 = f0.max(fmin_log);
        }
        if !(f1 > f0 * 1.0001) || !f0.is_finite() || !f1.is_finite() {
            self.range = None;
            (f0, f1) = (if self.log_freq { fmin_log } else { 0.0 }, nyq);
        }

        let x_of = |f: f64| plot.left() + to_u(f, self.log_freq, f0, f1) as f32 * plot.width();
        let top = ((s.db.iter().skip(1).cloned().fold(-200.0f32, f32::max) / 10.0).ceil() * 10.0 + 10.0).min(60.0);
        let bottom = top - self.db_span;
        let y_of = |d: f32| plot.top() + ((top - d) / (top - bottom)).clamp(0.0, 1.0) * plot.height();

        // grid
        let mut d = top;
        while d >= bottom - 0.1 {
            let y = y_of(d);
            painter.add(Shape::dashed_line(&[Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.0, GRID_COLOR), 3.0, 3.0));
            painter.text(Pos2::new(plot.left() - 6.0, y), Align2::RIGHT_CENTER, format!("{d:.0} dBV"), FontId::monospace(10.0), Color32::from_gray(150));
            d -= if self.db_span > 100.0 { 20.0 } else { 10.0 };
        }
        if self.log_freq {
            let mut dec = 10f64.powf(f0.log10().floor());
            let mut guard = 0;
            while dec > 0.0 && dec <= f1 && guard < 40 {
                guard += 1;
                for m in 1..10 {
                    let f = dec * m as f64;
                    if f < f0 || f > f1 {
                        continue;
                    }
                    let x = x_of(f);
                    let major = m == 1;
                    painter.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, if major { AXIS_COLOR } else { GRID_COLOR }));
                    if major || m == 2 || m == 5 {
                        painter.text(Pos2::new(x, plot.bottom() + 5.0), Align2::CENTER_TOP, eng(f, "Hz"), FontId::monospace(10.0), Color32::from_gray(150));
                    }
                }
                dec *= 10.0;
            }
        } else {
            let step = nice_step(f1 - f0, (plot.width() / 110.0).max(2.0) as f64);
            let mut f = (f0 / step).ceil() * step;
            let mut guard = 0;
            while f <= f1 && guard < 400 {
                guard += 1;
                let x = x_of(f);
                painter.add(Shape::dashed_line(&[Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, GRID_COLOR), 3.0, 3.0));
                painter.text(Pos2::new(x, plot.bottom() + 5.0), Align2::CENTER_TOP, eng(f, "Hz"), FontId::monospace(10.0), Color32::from_gray(150));
                f += step;
            }
        }
        painter.rect_stroke(plot, 0.0, Stroke::new(1.0, AXIS_COLOR), egui::StrokeKind::Middle);

        // spectrum: per screen column the maximum of the bins it covers (peaks stay visible)
        let clip = painter.with_clip_rect(plot);
        let w = plot.width().max(1.0) as usize;
        let mut pts = Vec::with_capacity(w);
        for x in 0..w {
            let fa = from_u(x as f64 / w as f64, self.log_freq, f0, f1);
            let fb = from_u((x + 1) as f64 / w as f64, self.log_freq, f0, f1);
            let a = ((fa / s.df).floor() as usize).clamp(1, s.db.len() - 1);
            let b = ((fb / s.df).ceil() as usize).clamp(a + 1, s.db.len());
            let m = s.db[a..b].iter().cloned().fold(-300.0f32, f32::max);
            pts.push(Pos2::new(plot.left() + x as f32 + 0.5, y_of(m)));
        }
        clip.add(Shape::line(pts, Stroke::new(1.3, SPEC_COLOR)));
        let nf = y_of(s.noise_floor);
        clip.add(Shape::dashed_line(&[Pos2::new(plot.left(), nf), Pos2::new(plot.right(), nf)], Stroke::new(1.0, Color32::from_gray(90)), 4.0, 6.0));

        // markers
        let mark = |f: f64, d: f32, label: String, col: Color32| {
            let x = x_of(f);
            if x < plot.left() || x > plot.right() {
                return;
            }
            let y = y_of(d) - 3.0;
            painter.add(Shape::convex_polygon(vec![Pos2::new(x - 5.0, y - 9.0), Pos2::new(x + 5.0, y - 9.0), Pos2::new(x, y)], col, Stroke::NONE));
            painter.text(Pos2::new(x, y - 11.0), Align2::CENTER_BOTTOM, label, FontId::monospace(10.0), col);
        };
        if let Some(fund) = &s.fundamental {
            let d1 = 20.0 * fund.vrms.max(1e-9).log10();
            for h in &s.harmonics {
                if h.dbc > -70.0 {
                    mark(h.freq, (d1 + h.dbc) as f32, format!("{}", h.order), Color32::from_rgb(200, 160, 255));
                }
            }
            mark(fund.freq, d1 as f32, eng(fund.freq, "Hz"), TRIGGER_COLOR);
        }

        // readout
        if let Some(p) = resp.hover_pos().filter(|p| plot.contains(*p)) {
            let f = from_u(((p.x - plot.left()) / plot.width()) as f64, self.log_freq, f0, f1);
            let k = ((f / s.df).round() as usize).min(s.db.len() - 1);
            clip.line_segment([Pos2::new(p.x, plot.top()), Pos2::new(p.x, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(40)));
            painter.text(
                Pos2::new(plot.right() - 6.0, plot.top() + 6.0),
                Align2::RIGHT_TOP,
                format!("{}  {:.1} dBV", eng(k as f64 * s.df, "Hz"), s.db[k]),
                FontId::monospace(11.0),
                Color32::from_rgb(120, 210, 255),
            );
        }
        painter.text(
            Pos2::new(plot.left() + 6.0, plot.top() + 6.0),
            Align2::LEFT_TOP,
            format!("FFT {} · {} {} · Δf {}", s.fft_size, s.samples, tr("samples"), eng(s.df, "Hz")),
            FontId::monospace(11.0),
            Color32::from_gray(170),
        );
        let _ = Vec2::ZERO;
    }
}
