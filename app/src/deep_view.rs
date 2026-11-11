//! Deep memory viewer: the complete 30100-sample record with zoom (mouse wheel), pan (drag), a time
//! selection (right-drag, or drag on the time axis) for measurements and zoom, an overview strip, peak-preserving min/max drawing, trigger / screen-window markers and a readout.

use crate::deep::DeepCapture;
use crate::i18n::{tr, trf};
use crate::measure::eng;
use crate::protocol::{CENTRE_PX, PX_PER_DIV};
use crate::scope::{AXIS_COLOR, CURSOR_T_COLOR, CURSOR_V_COLOR, GRID_COLOR, TRACE_COLOR, TRIGGER_COLOR};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};

const BG: Color32 = Color32::from_rgb(8, 10, 12);
const SCREEN_BAND: Color32 = Color32::from_rgba_premultiplied(30, 60, 90, 40);
const Y_MIN: f32 = (CENTRE_PX - 4.0 * PX_PER_DIV) as f32; // code at the bottom grid line
const Y_MAX: f32 = (CENTRE_PX + 4.0 * PX_PER_DIV) as f32;
const OVERVIEW_H: f32 = 42.0;
const SELECT_FILL: Color32 = Color32::from_rgba_premultiplied(40, 90, 60, 50);
const SELECT_EDGE: Color32 = Color32::from_rgb(110, 220, 150);

#[derive(Clone, Copy, Default, PartialEq)]
enum Drag {
    #[default]
    None,
    Overview,
    Pan,
    CursorT(usize),
    CursorV(usize),
    Trigger,
    /// a new time selection from where the button went down
    Select,
    /// an edge of the selection
    SelEdge(usize),
}

#[derive(Default)]
pub struct DeepView {
    /// visible time window (seconds, trigger = 0); None = whole record
    window: Option<(f64, f64)>,
    drag: Drag,
    /// measurement cursors: switched on from the side panel; times in seconds from the trigger,
    /// voltages as ADC codes (screen pixels)
    pub t_on: bool,
    pub v_on: bool,
    pub cur_t: [f64; 2],
    pub cur_v: [f32; 2],
    cursors_ready: bool,
    /// trigger level being dragged (pixels from the centre)
    trig_drag: Option<f32>,
    /// stream records: keep the newest sample at the right edge (switched off by panning)
    pub follow_end: bool,
    /// the next frame picks the initial (one screen wide) window of a new stream record
    follow_init: bool,
    /// plot rectangle of the last frame (tests)
    pub plot_rect: Option<Rect>,
    /// time selection (seconds from the trigger, start < end): measurements, FFT and "CSV (visible)" use it
    pub selection: Option<(f64, f64)>,
    sel_anchor: f64,
}

pub struct DeepOutput {
    /// visible sample range [a, b)
    pub visible: (usize, usize),
    /// new trigger level (device units) when its line was dragged
    pub set_trigger_level: Option<i16>,
    /// `visible` is the time selection, not the whole window
    pub selected: bool,
}

/// "nice" tick spacing (1, 2, 5 x 10^n) for about `target` ticks over `span`
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

impl DeepView {
    /// visible time window (None = whole record)
    pub fn window(&self) -> Option<(f64, f64)> {
        self.window
    }
    /// Start following a (new) stream record: newest sample at the right edge, 300 samples (one screen) wide.
    pub fn restart_follow(&mut self) {
        self.follow_end = true;
        self.follow_init = true;
        self.window = None;
    }

    pub fn fit_all(&mut self) {
        self.window = None;
    }
    /// show exactly the selected time range (a little margin on both sides)
    pub fn zoom_to_selection(&mut self, c: &DeepCapture) {
        if let Some((a, b)) = self.selection {
            let m = (b - a) * 0.02;
            self.follow_end = false;
            self.set_window(c, a - m, b + m);
        }
    }
    pub fn show_screen_window(&mut self, c: &DeepCapture) {
        self.window = Some(c.screen_window());
    }
    pub fn zoom(&mut self, c: &DeepCapture, factor: f64) {
        let (t0, t1) = self.window_of(c);
        let mid = if self.follow_end && c.stream { t1 } else { (t0 + t1) / 2.0 };
        self.set_window(c, mid - (mid - t0) * factor, mid + (t1 - mid) * factor);
    }

    fn window_of(&self, c: &DeepCapture) -> (f64, f64) {
        self.window.unwrap_or((c.t_first(), c.t_last()))
    }

    fn set_window(&mut self, c: &DeepCapture, mut t0: f64, mut t1: f64) {
        let (lo, hi) = (c.t_first(), c.t_last());
        let min_span = c.dt * 8.0;
        if t1 - t0 < min_span {
            let m = (t0 + t1) / 2.0;
            t0 = m - min_span / 2.0;
            t1 = m + min_span / 2.0;
        }
        let span = (t1 - t0).min(hi - lo);
        if t0 < lo {
            t0 = lo;
            t1 = lo + span;
        }
        if t1 > hi {
            t1 = hi;
            t0 = hi - span;
        }
        self.window = if t0 <= lo + c.dt * 0.5 && t1 >= hi - c.dt * 0.5 { None } else { Some((t0, t1)) };
    }

    pub fn show(&mut self, ui: &mut egui::Ui, c: Option<&DeepCapture>, badge: Option<(&str, Color32)>, can_set_trigger: bool) -> DeepOutput {
        let avail = ui.available_size();
        let (resp, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
        let full = resp.rect;
        painter.rect_filled(full, 6.0, BG);
        let mut out = DeepOutput { visible: (0, 0), set_trigger_level: None, selected: false };

        let Some(c) = c.filter(|c| c.len() > 8) else {
            painter.text(full.center(), Align2::CENTER_CENTER, tr("Waiting for a deep-memory capture…\n(scope screen; 50 ns … 20 ms/div as a record, 50 ms/div and slower as a stream)"), FontId::proportional(16.0), Color32::from_gray(140));
            return out;
        };

        if self.follow_end && c.stream {
            let total = c.t_last() - c.t_first();
            if self.follow_init && c.len() > 8 {
                // a new record: start one screen (300 columns) wide, then the user's zoom is kept
                self.follow_init = false;
                self.window = Some((c.t_last() - 300.0 * c.dt, c.t_last()));
            }
            // no window = the whole record (zoomed out): it stays that way while the record grows
            if let Some((a, b)) = self.window {
                let span = (b - a).clamp(c.dt * 8.0, total.max(c.dt * 8.0));
                self.window = if span >= total - c.dt * 0.5 { None } else { Some((c.t_last() - span, c.t_last())) };
            }
        }
        let overview = Rect::from_min_size(full.min + Vec2::new(60.0, 8.0), Vec2::new(full.width() - 80.0, OVERVIEW_H));
        let plot = Rect::from_min_max(Pos2::new(full.left() + 60.0, overview.bottom() + 14.0), Pos2::new(full.right() - 20.0, full.bottom() - 34.0));
        if plot.width() < 50.0 || plot.height() < 50.0 {
            return out;
        }

        self.plot_rect = Some(plot);
        // ---- interaction ----
        let (mut t0, mut t1) = self.window_of(c);
        if let Some(p) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 && plot.expand2(Vec2::new(0.0, 30.0)).contains(p) {
                let f = (-(scroll as f64) / 200.0).exp();
                let tm = if self.follow_end && c.stream {
                    t1                                   // following: zoom around the newest sample, the right edge
                } else {
                    t0 + (t1 - t0) * ((p.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64
                };
                self.set_window(c, tm - (tm - t0) * f, tm + (t1 - tm) * f);
            }
        }
        let all = (c.t_first(), c.t_last());
        let ov_t = |x: f32| all.0 + (all.1 - all.0) * ((x - overview.left()) / overview.width()).clamp(0.0, 1.0) as f64;
        if !self.cursors_ready {
            self.cur_t = [t0 + (t1 - t0) * 0.25, t0 + (t1 - t0) * 0.75];
            self.cur_v = [CENTRE_PX as f32 + PX_PER_DIV as f32, CENTRE_PX as f32 - PX_PER_DIV as f32];
            self.cursors_ready = true;
        }
        let code_at = |y: f32| Y_MIN + (plot.bottom() - y) / plot.height() * (Y_MAX - Y_MIN);
        let t_at = |x: f32| t0 + (t1 - t0) * ((x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64;
        // what is under the mouse: the overview strip, a cursor line, the trigger level, else pan
        let (cur_t, cur_v, t_on, v_on) = (self.cur_t, self.cur_v, self.t_on, self.v_on);
        let trig_px = self.trig_drag.unwrap_or(c.info.trig_level as f32);
        let selection = self.selection;
        // the time axis under the plot: dragging there selects
        let axis = Rect::from_x_y_ranges(plot.x_range(), plot.bottom() + 2.0..=full.bottom());
        let pick = |p: Pos2| -> Drag {
            if overview.expand(4.0).contains(p) {
                return Drag::Overview;
            }
            if axis.contains(p) {
                return Drag::Select;
            }
            let mut best = (9.0f32, Drag::Pan);
            if plot.expand(14.0).contains(p) {
                if let Some((a, b)) = selection {
                    for (i, t) in [a, b].into_iter().enumerate() {
                        let x = plot.left() + ((t - t0) / (t1 - t0)) as f32 * plot.width();
                        if (x - p.x).abs() < best.0 {
                            best = ((x - p.x).abs(), Drag::SelEdge(i));
                        }
                    }
                }
                if t_on {
                    for i in 0..2 {
                        let x = plot.left() + ((cur_t[i] - t0) / (t1 - t0)) as f32 * plot.width();
                        if (x - p.x).abs() < best.0 {
                            best = ((x - p.x).abs(), Drag::CursorT(i));
                        }
                    }
                }
                if v_on {
                    for i in 0..2 {
                        let y = plot.bottom() - (cur_v[i] - Y_MIN) / (Y_MAX - Y_MIN) * plot.height();
                        if (y - p.y).abs() < best.0 {
                            best = ((y - p.y).abs(), Drag::CursorV(i));
                        }
                    }
                }
                if can_set_trigger && !c.stream {
                    let y = plot.bottom() - (CENTRE_PX as f32 + trig_px - Y_MIN) / (Y_MAX - Y_MIN) * plot.height();
                    if (y - p.y).abs() < best.0 {
                        best = ((y - p.y).abs(), Drag::Trigger);
                    }
                }
            }
            best.1
        };
        // a drag starts where the button went down (egui reports it only after a few pixels of movement)
        if resp.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
            self.drag = origin.map(&pick).unwrap_or(Drag::None);
            // right button or Shift: always a new selection
            let select = resp.dragged_by(egui::PointerButton::Secondary) || ui.input(|i| i.modifiers.shift);
            if select && !matches!(self.drag, Drag::Overview) {
                self.drag = Drag::Select;
            }
            if let (Drag::Select, Some(p)) = (self.drag, origin) {
                self.sel_anchor = t_at(p.x);
                self.selection = None;
            }
            if self.drag == Drag::Trigger {
                self.trig_drag = Some(c.info.trig_level as f32);
            }
            if matches!(self.drag, Drag::Pan | Drag::Overview) {
                self.follow_end = false;                 // the user looks at the past
            }
        }
        if resp.dragged() {
            let ptr = resp.interact_pointer_pos();
            match (self.drag, ptr) {
                (Drag::Overview, Some(p)) => {
                    let span = t1 - t0;
                    let m = ov_t(p.x);
                    self.set_window(c, m - span / 2.0, m + span / 2.0);
                }
                (Drag::Pan, _) => {
                    let dx = resp.drag_delta().x as f64;
                    let dt = -dx / plot.width() as f64 * (t1 - t0);
                    self.set_window(c, t0 + dt, t1 + dt);
                }
                (Drag::CursorT(i), Some(p)) => self.cur_t[i] = t_at(p.x),
                (Drag::CursorV(i), Some(p)) => self.cur_v[i] = code_at(p.y).clamp(Y_MIN, Y_MAX),
                (Drag::Trigger, Some(p)) => self.trig_drag = Some((code_at(p.y) - CENTRE_PX as f32).clamp(-100.0, 100.0)),
                (Drag::Select, Some(p)) => {
                    let t = t_at(p.x);
                    self.selection = Some((self.sel_anchor.min(t), self.sel_anchor.max(t)));
                }
                (Drag::SelEdge(i), Some(p)) => {
                    if let Some(sel) = self.selection.as_mut() {
                        if i == 0 { sel.0 = t_at(p.x) } else { sel.1 = t_at(p.x) }
                    }
                }
                _ => {}
            }
        }
        if resp.drag_stopped() {
            if self.drag == Drag::Trigger {
                out.set_trigger_level = self.trig_drag.map(|l| l.round() as i16);
            }
            if matches!(self.drag, Drag::Select | Drag::SelEdge(_)) {
                // edges in order; narrower than 3 pixels or 2 samples: no selection (a click)
                self.selection = self.selection
                    .map(|(a, b)| (a.min(b), a.max(b)))
                    .filter(|(a, b)| (b - a) > ((t1 - t0) * 3.0 / plot.width() as f64).max(c.dt * 2.0));
            }
            self.drag = Drag::None;
            self.trig_drag = None;
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos().filter(|p| overview.contains(*p)) {
                let span = t1 - t0;
                let m = ov_t(p.x);
                self.set_window(c, m - span / 2.0, m + span / 2.0);
            }
        }
        // right click or Esc: no selection
        if resp.secondary_clicked() || (resp.hovered() && ui.input(|i| i.key_pressed(egui::Key::Escape))) {
            self.selection = None;
        }
        if resp.double_clicked() {
            let in_sel = resp.interact_pointer_pos().zip(self.selection).is_some_and(|(p, (a, b))| (a..=b).contains(&t_at(p.x)) && plot.contains(p));
            if in_sel {
                self.zoom_to_selection(c);
            } else {
                self.window = None;
            }
        }
        // the mouse pointer shows what can be dragged
        let over = if self.drag == Drag::None { resp.hover_pos().map(&pick) } else { Some(self.drag) };
        match over {
            Some(Drag::CursorT(_)) | Some(Drag::SelEdge(_)) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal),
            Some(Drag::Select) => ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair),
            Some(Drag::CursorV(_)) | Some(Drag::Trigger) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical),
            Some(Drag::Overview) => ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand),
            Some(Drag::Pan) if self.window.is_some() => ui.ctx().set_cursor_icon(if self.drag == Drag::Pan { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab }),
            _ => {}
        }
        (t0, t1) = self.window_of(c);

        let x_of = |t: f64| plot.left() + ((t - t0) / (t1 - t0)) as f32 * plot.width();
        let y_of = |code: f32| plot.bottom() - (code - Y_MIN) / (Y_MAX - Y_MIN) * plot.height();

        // ---- overview strip ----
        painter.rect_filled(overview, 3.0, Color32::from_rgb(16, 19, 23));
        let ow = overview.width().max(1.0) as usize;
        let n = c.len();
        let oy = |code: u8| overview.bottom() - 2.0 - (code as f32 / 255.0) * (overview.height() - 4.0);
        let mut env = Vec::with_capacity(ow * 2);
        for x in 0..ow {
            let a = x * n / ow;
            let b = ((x + 1) * n / ow).max(a + 1);
            if let Some((lo, hi)) = c.min_max(a, b) {
                let px = overview.left() + x as f32;
                env.push(Shape::line_segment([Pos2::new(px, oy(lo)), Pos2::new(px, oy(hi) - 0.5)], Stroke::new(1.0, Color32::from_rgb(150, 125, 20))));
            }
        }
        painter.extend(env);
        let ox = |t: f64| overview.left() + ((t - all.0) / (all.1 - all.0)) as f32 * overview.width();
        let (s0, s1) = c.screen_window();
        painter.rect_filled(Rect::from_x_y_ranges(ox(s0).max(overview.left())..=ox(s1).min(overview.right()), overview.y_range()), 0.0, SCREEN_BAND);
        painter.line_segment([Pos2::new(ox(0.0), overview.top()), Pos2::new(ox(0.0), overview.bottom())], Stroke::new(1.0, TRIGGER_COLOR));
        let vis = Rect::from_x_y_ranges(ox(t0)..=ox(t1).max(ox(t0) + 2.0), overview.y_range());
        painter.rect_stroke(vis, 2.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Middle);
        painter.text(overview.left_center() - Vec2::new(6.0, 0.0), Align2::RIGHT_CENTER, tr("record"), FontId::proportional(11.0), Color32::from_gray(130));

        // ---- grid ----
        for j in 0..=8 {
            let y = plot.top() + plot.height() * j as f32 / 8.0;
            let stroke = Stroke::new(if j == 4 { 1.2 } else { 1.0 }, if j == 4 { AXIS_COLOR } else { GRID_COLOR });
            painter.add(Shape::dashed_line(&[Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], stroke, 3.0, 3.0));
            let code = Y_MAX - (Y_MAX - Y_MIN) * j as f32 / 8.0;
            painter.text(Pos2::new(plot.left() - 6.0, y), Align2::RIGHT_CENTER, eng(c.volts(code as u8), "V"), FontId::monospace(10.0), Color32::from_gray(150));
        }
        let step = nice_step(t1 - t0, (plot.width() / 110.0).max(2.0) as f64);
        let mut t = (t0 / step).ceil() * step;
        let mut guard = 0;
        while t <= t1 && guard < 400 {
            guard += 1;
            let x = x_of(t);
            let zero = t.abs() < step * 1e-6;
            painter.add(Shape::dashed_line(&[Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, if zero { AXIS_COLOR } else { GRID_COLOR }), 3.0, 3.0));
            painter.text(Pos2::new(x, plot.bottom() + 6.0), Align2::CENTER_TOP, eng(if zero { 0.0 } else { t }, "s"), FontId::monospace(10.0), Color32::from_gray(150));
            t += step;
        }
        painter.rect_stroke(plot, 0.0, Stroke::new(1.0, AXIS_COLOR), egui::StrokeKind::Middle);

        let clip = painter.with_clip_rect(plot.expand(1.0));
        // what the scope screen shows
        let band = Rect::from_x_y_ranges(x_of(s0).max(plot.left())..=x_of(s1).min(plot.right()), plot.y_range());
        if band.width() > 0.0 {
            clip.rect_filled(band, 0.0, SCREEN_BAND);
        }
        if let Some((a, b)) = self.selection {
            let (xa, xb) = (x_of(a), x_of(b));
            clip.rect_filled(Rect::from_x_y_ranges(xa.max(plot.left() - 1.0)..=xb.min(plot.right() + 1.0), plot.y_range()), 0.0, SELECT_FILL);
            for x in [xa, xb] {
                clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.5, SELECT_EDGE));
            }
            // also on the overview strip
            painter.rect_filled(Rect::from_x_y_ranges(ox(a).max(overview.left())..=ox(b).min(overview.right()).max(ox(a) + 1.0), overview.y_range()), 0.0, SELECT_FILL);
        }

        // ---- samples ----
        let i0 = c.index_at(t0).floor().max(0.0) as usize;
        let i1 = (c.index_at(t1).ceil() as usize + 1).min(n);
        out.visible = (i0, i1);
        if let Some((a, b)) = self.selection {
            let sa = c.index_at(a).ceil().clamp(0.0, n as f64) as usize;
            let sb = ((c.index_at(b).floor() + 1.0).clamp(0.0, n as f64) as usize).max(sa);
            if sb > sa + 1 {
                out.visible = (sa, sb);
                out.selected = true;
            }
        }
        let spp = (i1 - i0) as f32 / plot.width();
        if spp > 1.5 {
            // peak-preserving: one min/max segment per screen column, joined
            let w = plot.width() as usize;
            let mut shapes = Vec::with_capacity(w + 1);
            for x in 0..w {
                let ta = t0 + (t1 - t0) * x as f64 / w as f64;
                let tb = t0 + (t1 - t0) * (x + 1) as f64 / w as f64;
                let a = c.index_at(ta).floor().max(0.0) as usize;
                let b = (c.index_at(tb).ceil() as usize).min(n).max(a + 1);
                if let Some((lo, hi)) = c.min_max(a, b) {
                    let px = plot.left() + x as f32 + 0.5;
                    shapes.push(Shape::line_segment([Pos2::new(px, y_of(lo as f32)), Pos2::new(px, y_of(hi as f32) - 0.5)], Stroke::new(1.0, TRACE_COLOR)));
                }
            }
            clip.extend(shapes);
        } else {
            let pts: Vec<Pos2> = (i0..i1).map(|i| Pos2::new(x_of(c.time(i as f64)), y_of(c.codes[i] as f32))).collect();
            if spp < 0.25 {
                for p in &pts {
                    clip.circle_filled(*p, 2.0, TRACE_COLOR);
                }
            }
            clip.add(Shape::line(pts, Stroke::new(1.4, TRACE_COLOR)));
        }

        // ---- markers ----
        let tx = x_of(0.0);
        if !c.stream && tx >= plot.left() && tx <= plot.right() {
            clip.add(Shape::dashed_line(&[Pos2::new(tx, plot.top()), Pos2::new(tx, plot.bottom())], Stroke::new(1.2, TRIGGER_COLOR), 6.0, 4.0));
            painter.text(Pos2::new(tx, plot.top() - 2.0), Align2::CENTER_BOTTOM, "T", FontId::proportional(12.0), TRIGGER_COLOR);
        }
        let ly = y_of(CENTRE_PX as f32 + self.trig_drag.unwrap_or(c.info.trig_level as f32));
        if !c.stream && ly >= plot.top() && ly <= plot.bottom() {
            let col = if self.trig_drag.is_some() { Color32::WHITE } else { TRIGGER_COLOR.gamma_multiply(0.7) };
            clip.add(Shape::dashed_line(&[Pos2::new(plot.left(), ly), Pos2::new(plot.right(), ly)], Stroke::new(1.0, col), 6.0, 6.0));
            if can_set_trigger && !c.stream {
                painter.add(Shape::convex_polygon(
                    vec![Pos2::new(plot.right() + 2.0, ly), Pos2::new(plot.right() + 13.0, ly - 7.0), Pos2::new(plot.right() + 13.0, ly + 7.0)],
                    col,
                    Stroke::NONE,
                ));
            }
        }
        if self.t_on {
            for (i, &t) in self.cur_t.iter().enumerate() {
                let x = x_of(t);
                clip.add(Shape::dashed_line(&[Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.3, CURSOR_T_COLOR), 5.0, 3.0));
                painter.text(Pos2::new(x, plot.bottom() - 2.0), Align2::CENTER_BOTTOM, format!("T{}", i + 1), FontId::monospace(11.0), CURSOR_T_COLOR);
            }
        }
        if self.v_on {
            for (i, &v) in self.cur_v.iter().enumerate() {
                let y = y_of(v);
                clip.add(Shape::dashed_line(&[Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.3, CURSOR_V_COLOR), 5.0, 3.0));
                painter.text(Pos2::new(plot.left() + 3.0, y - 2.0), Align2::LEFT_BOTTOM, format!("V{}", i + 1), FontId::monospace(11.0), CURSOR_V_COLOR);
            }
        }
        let gy = y_of(CENTRE_PX as f32 + c.info.offset as f32);
        if gy >= plot.top() && gy <= plot.bottom() {
            painter.text(Pos2::new(plot.right() + 3.0, gy), Align2::LEFT_CENTER, "◀", FontId::proportional(12.0), Color32::from_rgb(90, 200, 120));
        }

        // ---- info line ----
        let info = trf(
            "{} samples · {}/s · {} per sample · window {} ({}×)",
            &[&n, &eng(c.sample_rate(), "Sa"), &eng(c.dt, "s"), &eng(t1 - t0, "s"), &format!("{:.0}", ((all.1 - all.0) / (t1 - t0)).max(1.0))],
        );
        painter.text(Pos2::new(plot.left() + 6.0, plot.top() + 6.0), Align2::LEFT_TOP, info, FontId::monospace(11.0), Color32::from_gray(170));
        if let Some((text, col)) = badge {
            let r = painter.text(Pos2::new(plot.right() - 8.0, plot.top() + 6.0), Align2::RIGHT_TOP, text, FontId::proportional(13.0), Color32::BLACK);
            painter.rect_filled(r.expand(4.0), 3.0, col);
            painter.text(Pos2::new(plot.right() - 8.0, plot.top() + 6.0), Align2::RIGHT_TOP, text, FontId::proportional(13.0), Color32::BLACK);
        }

        // ---- selection read-out: at its top, inside the plot ----
        if let Some((a, b)) = self.selection {
            let (sa, sb) = out.visible;
            let dt = b - a;
            let text = trf("Δt {}   1/Δt {}   {} samples", &[&eng(dt, "s"), &eng(1.0 / dt, "Hz"), &(if out.selected { sb - sa } else { 0 })]);
            let xm = ((x_of(a) + x_of(b)) / 2.0).min(plot.right() - 120.0).max(plot.left() + 120.0).min(plot.right()); // (narrow plot: no panic)
            let r = painter.text(Pos2::new(xm, plot.top() + 26.0), Align2::CENTER_TOP, &text, FontId::monospace(11.0), Color32::BLACK);
            painter.rect_filled(r.expand(3.0), 3.0, SELECT_EDGE);
            painter.text(Pos2::new(xm, plot.top() + 26.0), Align2::CENTER_TOP, text, FontId::monospace(11.0), Color32::BLACK);
        }

        // ---- readout under the mouse ----
        if let Some(p) = resp.hover_pos().filter(|p| plot.contains(*p)) {
            let t = t0 + (t1 - t0) * ((p.x - plot.left()) / plot.width()) as f64;
            let i = c.index_at(t).round().clamp(0.0, (n - 1) as f64) as usize;
            let code = Y_MIN + (plot.bottom() - p.y) / plot.height() * (Y_MAX - Y_MIN);
            clip.line_segment([Pos2::new(p.x, plot.top()), Pos2::new(p.x, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(40)));
            let s = trf("t {}   cursor {}   sample #{}: {}", &[&eng(t, "s"), &eng(c.volts(code.clamp(0.0, 255.0) as u8), "V"), &i, &eng(c.volts(c.codes[i]), "V")]);
            painter.text(Pos2::new(plot.left() + 6.0, plot.bottom() - 6.0), Align2::LEFT_BOTTOM, s, FontId::monospace(11.0), Color32::from_rgb(120, 210, 255));
        }
        out
    }
}
