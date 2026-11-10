//! The scope screen: grid, trace, persistence, trigger / position markers and measurement cursors,
//! drawn like the instrument itself (12 x 8 divisions of 25 trace pixels).

use crate::i18n::tr;
use crate::protocol::{self, Frame, CENTRE_PX, PX_PER_DIV};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};

pub const TRACE_COLOR: Color32 = Color32::from_rgb(255, 214, 10);
pub const GRID_COLOR: Color32 = Color32::from_rgb(55, 60, 66);
pub const AXIS_COLOR: Color32 = Color32::from_rgb(90, 96, 104);
pub const TRIGGER_COLOR: Color32 = Color32::from_rgb(255, 120, 40);
pub const CURSOR_T_COLOR: Color32 = Color32::from_rgb(70, 200, 255);
pub const CURSOR_V_COLOR: Color32 = Color32::from_rgb(230, 90, 230);
const BG: Color32 = Color32::from_rgb(8, 10, 12);

/// Screen-pixel domain of the trace: x 0..300, y (trace value) 28..228.
const X_MAX: f32 = 300.0;
const Y_MIN: f32 = (CENTRE_PX - 4.0 * PX_PER_DIV) as f32; // 28
const Y_MAX: f32 = (CENTRE_PX + 4.0 * PX_PER_DIV) as f32; // 228

#[derive(Clone, Debug)]
pub struct Cursors {
    pub time_on: bool,
    pub volt_on: bool,
    /// trace columns
    pub t: [f32; 2],
    /// trace pixel values
    pub v: [f32; 2],
}

impl Default for Cursors {
    fn default() -> Self {
        Self { time_on: false, volt_on: false, t: [75.0, 225.0], v: [153.0, 103.0] }
    }
}

#[derive(Default)]
enum Drag {
    #[default]
    None,
    Time(usize),
    Volt(usize),
    Trigger,
    Offset,
    Position,
}

#[derive(Default)]
pub struct ScopeView {
    drag: Drag,
    /// trigger level being dragged (trace pixels relative to the centre)
    drag_level: Option<f32>,
    /// ground marker being dragged (offset, trace pixels)
    drag_offset: Option<f32>,
    /// trigger position marker being dragged (hpos)
    drag_hpos: Option<f32>,
    /// plot rectangle of the last frame (tests, hit-testing from outside)
    pub plot_rect: Option<Rect>,
}

pub struct ScopeOutput {
    /// New trigger level (device units) when a drag of the trigger marker ended.
    pub set_trigger_level: Option<i16>,
    /// New vertical offset when the ground marker was dragged.
    pub set_offset: Option<i8>,
    /// New horizontal position when the top marker was dragged.
    pub set_hpos: Option<i16>,
}

pub struct ScopeInput<'a> {
    pub frame: Option<&'a Frame>,
    pub persistence: &'a [&'a Frame],
    pub cursors: &'a mut Cursors,
    pub hpos: Option<i16>,
    pub show_trigger: bool,
    pub can_set_trigger: bool,
    pub badge: Option<(&'a str, Color32)>,
}

impl ScopeView {
    pub fn show(&mut self, ui: &mut egui::Ui, inp: ScopeInput) -> ScopeOutput {
        let mut out = ScopeOutput { set_trigger_level: None, set_offset: None, set_hpos: None };
        let avail = ui.available_size();
        let (resp, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
        let full = resp.rect;
        painter.rect_filled(full, 6.0, BG);

        // keep the 12:8 aspect ratio, leave room for labels
        let margin = Vec2::new(54.0, 34.0);
        let inner = full.shrink2(margin);
        let w = inner.width().min(inner.height() * 1.5);
        let h = w / 1.5;
        let plot = Rect::from_center_size(inner.center(), Vec2::new(w, h));
        self.plot_rect = Some(plot);

        let to_screen = |x: f32, y: f32| -> Pos2 {
            Pos2::new(plot.left() + x / X_MAX * plot.width(), plot.bottom() - (y - Y_MIN) / (Y_MAX - Y_MIN) * plot.height())
        };
        let from_screen = |p: Pos2| -> (f32, f32) {
            (
                ((p.x - plot.left()) / plot.width() * X_MAX).clamp(0.0, X_MAX),
                (Y_MIN + (plot.bottom() - p.y) / plot.height() * (Y_MAX - Y_MIN)).clamp(Y_MIN - 20.0, Y_MAX + 20.0),
            )
        };

        // grid
        for i in 0..=12 {
            let x = plot.left() + plot.width() * i as f32 / 12.0;
            let stroke = Stroke::new(if i == 6 { 1.2 } else { 1.0 }, if i == 6 { AXIS_COLOR } else { GRID_COLOR });
            painter.add(Shape::dashed_line(&[Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], stroke, 3.0, 3.0));
        }
        for j in 0..=8 {
            let y = plot.top() + plot.height() * j as f32 / 8.0;
            let stroke = Stroke::new(if j == 4 { 1.2 } else { 1.0 }, if j == 4 { AXIS_COLOR } else { GRID_COLOR });
            painter.add(Shape::dashed_line(&[Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], stroke, 3.0, 3.0));
        }
        // minor ticks on the centre axes
        for k in 0..=60 {
            let x = plot.left() + plot.width() * k as f32 / 60.0;
            painter.line_segment([Pos2::new(x, plot.center().y - 3.0), Pos2::new(x, plot.center().y + 3.0)], Stroke::new(1.0, AXIS_COLOR));
        }
        for k in 0..=40 {
            let y = plot.top() + plot.height() * k as f32 / 40.0;
            painter.line_segment([Pos2::new(plot.center().x - 3.0, y), Pos2::new(plot.center().x + 3.0, y)], Stroke::new(1.0, AXIS_COLOR));
        }
        painter.rect_stroke(plot, 0.0, Stroke::new(1.0, AXIS_COLOR), egui::StrokeKind::Middle);

        let clip = painter.with_clip_rect(plot.expand(1.0));
        let trace_shape = |f: &Frame, color: Color32, width: f32| -> Vec<Shape> {
            let mut shapes = Vec::new();
            let mut run: Vec<Pos2> = Vec::new();
            for (i, &px) in f.trace.iter().enumerate() {
                if f.valid(i) {
                    run.push(to_screen(i as f32, px as f32));
                } else if run.len() > 1 {
                    shapes.push(Shape::line(std::mem::take(&mut run), Stroke::new(width, color)));
                } else {
                    run.clear();
                }
            }
            if run.len() > 1 {
                shapes.push(Shape::line(run, Stroke::new(width, color)));
            }
            shapes
        };

        // persistence (older frames first, fading)
        let n = inp.persistence.len();
        for (k, f) in inp.persistence.iter().enumerate() {
            let alpha = (18.0 + 70.0 * (k + 1) as f32 / n.max(1) as f32) as u8;
            let c = Color32::from_rgba_unmultiplied(TRACE_COLOR.r(), TRACE_COLOR.g(), TRACE_COLOR.b(), alpha);
            for s in trace_shape(f, c, 1.0) {
                clip.add(s);
            }
        }

        if let Some(f) = inp.frame {
            for s in trace_shape(f, TRACE_COLOR, 1.6) {
                clip.add(s);
            }
        } else {
            painter.text(plot.center(), Align2::CENTER_CENTER, tr("waiting for data…"), FontId::proportional(18.0), AXIS_COLOR);
        }

        // ground marker (where 0 V is drawn = centre + offset)
        let mut ground_marker = None;
        if let Some(f) = inp.frame {
            let gy = self.drag_offset.map(|o| CENTRE_PX as f32 + o).unwrap_or((CENTRE_PX + f.offset as f64) as f32);
            if (Y_MIN..=Y_MAX).contains(&gy) {
                let p = to_screen(0.0, gy);
                painter.add(Shape::convex_polygon(
                    vec![Pos2::new(p.x - 12.0, p.y - 6.0), Pos2::new(p.x - 2.0, p.y), Pos2::new(p.x - 12.0, p.y + 6.0)],
                    TRACE_COLOR,
                    Stroke::NONE,
                ));
                painter.text(Pos2::new(p.x - 22.0, p.y), Align2::RIGHT_CENTER, "1", FontId::monospace(11.0), TRACE_COLOR);
                ground_marker = Some(Rect::from_center_size(Pos2::new(p.x - 10.0, p.y), Vec2::new(26.0, 18.0)));
                if self.drag_offset.is_some() {
                    clip.add(Shape::dashed_line(&[to_screen(0.0, gy), to_screen(X_MAX, gy)], Stroke::new(1.0, TRACE_COLOR), 6.0, 4.0));
                }
            }
        }

        // trigger level marker (right edge), draggable
        let mut trigger_marker = None;
        if let Some(f) = inp.frame {
            if inp.show_trigger {
                let lvl = self.drag_level.unwrap_or(f.trig_level as f32);
                let y = (CENTRE_PX as f32 + lvl).clamp(Y_MIN, Y_MAX);
                let p = to_screen(X_MAX, y);
                let col = if self.drag_level.is_some() { Color32::WHITE } else { TRIGGER_COLOR };
                painter.add(Shape::convex_polygon(
                    vec![Pos2::new(p.x + 2.0, p.y), Pos2::new(p.x + 13.0, p.y - 7.0), Pos2::new(p.x + 13.0, p.y + 7.0)],
                    col,
                    Stroke::NONE,
                ));
                painter.text(Pos2::new(p.x + 16.0, p.y), Align2::LEFT_CENTER, "T", FontId::monospace(11.0), col);
                if self.drag_level.is_some() {
                    clip.add(Shape::dashed_line(&[to_screen(0.0, y), to_screen(X_MAX, y)], Stroke::new(1.0, col), 6.0, 4.0));
                }
                trigger_marker = Some(Rect::from_center_size(Pos2::new(p.x + 8.0, p.y), Vec2::new(22.0, 18.0)));
            }
        }

        // trigger position marker (top)
        let mut position_marker = None;
        if let Some(hp) = inp.hpos {
            let x = (150.0 + self.drag_hpos.unwrap_or(hp as f32)).clamp(0.0, X_MAX);
            let p = to_screen(x, Y_MAX);
            painter.add(Shape::convex_polygon(
                vec![Pos2::new(p.x - 6.0, p.y - 12.0), Pos2::new(p.x + 6.0, p.y - 12.0), Pos2::new(p.x, p.y - 2.0)],
                TRIGGER_COLOR,
                Stroke::NONE,
            ));
            position_marker = Some(Rect::from_center_size(Pos2::new(p.x, p.y - 7.0), Vec2::new(18.0, 18.0)));
            if self.drag_hpos.is_some() {
                clip.add(Shape::dashed_line(&[to_screen(x, Y_MIN), to_screen(x, Y_MAX)], Stroke::new(1.0, TRIGGER_COLOR), 6.0, 4.0));
            }
        }

        // cursors
        let c = &mut *inp.cursors;
        if c.time_on {
            for (i, &t) in c.t.iter().enumerate() {
                let a = to_screen(t, Y_MIN);
                let b = to_screen(t, Y_MAX);
                painter.add(Shape::dashed_line(&[a, b], Stroke::new(1.2, CURSOR_T_COLOR), 5.0, 3.0));
                painter.text(Pos2::new(b.x, b.y - 2.0), Align2::CENTER_BOTTOM, format!("T{}", i + 1), FontId::monospace(11.0), CURSOR_T_COLOR);
            }
        }
        if c.volt_on {
            for (i, &v) in c.v.iter().enumerate() {
                let a = to_screen(0.0, v);
                let b = to_screen(X_MAX, v);
                painter.add(Shape::dashed_line(&[a, b], Stroke::new(1.2, CURSOR_V_COLOR), 5.0, 3.0));
                painter.text(Pos2::new(a.x - 4.0, a.y), Align2::RIGHT_CENTER, format!("V{}", i + 1), FontId::monospace(11.0), CURSOR_V_COLOR);
            }
        }

        // labels: V/div, time/div, state
        if let Some(f) = inp.frame {
            let font = FontId::monospace(13.0);
            painter.text(Pos2::new(plot.left(), plot.bottom() + 6.0), Align2::LEFT_TOP,
                format!("CH1 {}/div  {}{}", f.vdiv_label(), protocol::COUPLING_LABELS.get(f.coupling as usize).copied().unwrap_or("?"),
                        if f.probe == 1 { "  ×10" } else { "" }),
                font.clone(), TRACE_COLOR);
            painter.text(Pos2::new(plot.center().x, plot.bottom() + 6.0), Align2::CENTER_TOP,
                format!("{}/div", f.timebase_label()), font.clone(), Color32::from_gray(220));
            painter.text(Pos2::new(plot.right(), plot.bottom() + 6.0), Align2::RIGHT_TOP,
                format!("T {} {}  {}", tr(protocol::TRIG_MODE_LABELS.get(f.trig_mode as usize).copied().unwrap_or("?")),
                        if f.trig_edge == 0 { "↑" } else { "↓" }, crate::measure::eng(f.trigger_volts(), "V")),
                font.clone(), TRIGGER_COLOR);
        }
        if let Some((text, color)) = inp.badge {
            let r = Rect::from_min_size(Pos2::new(plot.left(), plot.top() - 28.0), Vec2::new(70.0, 22.0));
            painter.rect_filled(r, 4.0, color);
            painter.text(r.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), Color32::BLACK);
        }

        // --- interaction ---------------------------------------------------------------
        // what is under point p: a marker, else the closest cursor line within a few pixels
        let (cur_t, cur_v, t_on, v_on) = (c.t, c.v, c.time_on, c.volt_on);
        let pick = |p: Pos2| -> Drag {
            if inp.can_set_trigger {
                if trigger_marker.is_some_and(|r: Rect| r.expand(4.0).contains(p)) {
                    return Drag::Trigger;
                }
                if ground_marker.is_some_and(|r: Rect| r.expand(4.0).contains(p)) {
                    return Drag::Offset;
                }
                if position_marker.is_some_and(|r: Rect| r.expand(4.0).contains(p)) {
                    return Drag::Position;
                }
            }
            if !plot.expand(6.0).contains(p) {
                return Drag::None;
            }
            let mut best = (12.0f32, Drag::None);
            if t_on {
                for i in 0..2 {
                    let d = (to_screen(cur_t[i], Y_MIN).x - p.x).abs();
                    if d < best.0 {
                        best = (d, Drag::Time(i));
                    }
                }
            }
            if v_on {
                for i in 0..2 {
                    let d = (to_screen(0.0, cur_v[i]).y - p.y).abs();
                    if d < best.0 {
                        best = (d, Drag::Volt(i));
                    }
                }
            }
            best.1
        };
        // a drag starts where the button went down (egui reports it only after a few pixels of movement)
        let origin = ui.input(|i| i.pointer.press_origin());
        if resp.drag_started() {
            if let Some(p) = origin.or(resp.interact_pointer_pos()) {
                self.drag = pick(p);
                match self.drag {
                    Drag::Trigger => self.drag_level = inp.frame.map(|f| f.trig_level as f32),
                    Drag::Offset => self.drag_offset = inp.frame.map(|f| f.offset as f32),
                    Drag::Position => self.drag_hpos = inp.hpos.map(|h| h as f32),
                    _ => {}
                }
            }
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (x, y) = from_screen(p);
                match self.drag {
                    Drag::Time(i) => c.t[i] = x,
                    Drag::Volt(i) => c.v[i] = y.clamp(Y_MIN, Y_MAX),
                    Drag::Trigger => self.drag_level = Some((y - CENTRE_PX as f32).clamp(-100.0, 100.0)),
                    Drag::Offset => self.drag_offset = Some((y - CENTRE_PX as f32).clamp(-100.0, 100.0)),
                    Drag::Position => self.drag_hpos = Some((x - 150.0).clamp(-150.0, 150.0)),
                    Drag::None => {}
                }
            }
        }
        if resp.drag_stopped() {
            match self.drag {
                Drag::Trigger => out.set_trigger_level = self.drag_level.map(|l| l.round() as i16),
                Drag::Offset => out.set_offset = self.drag_offset.map(|o| o.round() as i8),
                Drag::Position => out.set_hpos = self.drag_hpos.map(|h| h.round() as i16),
                _ => {}
            }
            self.drag = Drag::None;
            self.drag_level = None;
            self.drag_offset = None;
            self.drag_hpos = None;
        }
        // the mouse pointer shows what can be dragged
        let icon_for = |d: &Drag| match d {
            Drag::Time(_) | Drag::Position => Some(egui::CursorIcon::ResizeHorizontal),
            Drag::Volt(_) | Drag::Trigger | Drag::Offset => Some(egui::CursorIcon::ResizeVertical),
            Drag::None => None,
        };
        let hover_item = if matches!(self.drag, Drag::None) { resp.hover_pos().map(&pick) } else { None };
        if let Some(icon) = icon_for(&self.drag).or_else(|| hover_item.as_ref().and_then(icon_for)) {
            ui.ctx().set_cursor_icon(icon);
        }
        if let Some(p) = resp.hover_pos() {
            if plot.contains(p) {
                if let Some(f) = inp.frame {
                    let (x, y) = from_screen(p);
                    let t = x as f64 * f.seconds_per_px();
                    let v = protocol::px_to_volts(y as f64, f.volts_per_div(), f.offset);
                    painter.text(Pos2::new(plot.right() - 4.0, plot.top() + 4.0), Align2::RIGHT_TOP,
                        format!("{}  {}", crate::measure::eng(t, "s"), crate::measure::eng(v, "V")),
                        FontId::monospace(12.0), Color32::from_gray(200));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;
    use eframe::egui::{Event, PointerButton, RawInput};

    fn test_frame() -> Frame {
        let trace: Vec<u8> = (0..301).map(|i| (128.0 + (i as f32 / 20.0).sin() * 40.0) as u8).collect();
        Frame { trace, timebase: 13, vdiv: 6, trace_end: 301, ..Default::default() }
    }

    /// Runs the widget headless with scripted mouse input, like the real window would.
    struct Rig {
        ctx: egui::Context,
        view: ScopeView,
        cursors: Cursors,
        t: f64,
        pos: Pos2,
        last: Option<ScopeOutput>,
    }

    impl Rig {
        fn new() -> Self {
            let mut r = Rig { ctx: egui::Context::default(), view: ScopeView::default(), cursors: Cursors::default(), t: 0.0, pos: Pos2::new(5.0, 5.0), last: None };
            for _ in 0..3 {
                r.step(vec![]);
            }
            r
        }
        fn step(&mut self, events: Vec<Event>) {
            let mut input = RawInput::default();
            input.screen_rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0)));
            input.time = Some(self.t);
            self.t += 0.016;
            input.events = events;
            let f = test_frame();
            let (view, cursors) = (&mut self.view, &mut self.cursors);
            let mut out = None;
            let _ = self.ctx.run_ui(input, |ui| {
                out = Some(view.show(ui, ScopeInput { frame: Some(&f), persistence: &[], cursors, hpos: Some(0), show_trigger: true, can_set_trigger: true, badge: None }));
            });
            // keep what any frame reported (the value is returned on the frame the button goes up)
            let out = out.unwrap();
            let acc = self.last.get_or_insert(ScopeOutput { set_trigger_level: None, set_offset: None, set_hpos: None });
            acc.set_trigger_level = out.set_trigger_level.or(acc.set_trigger_level);
            acc.set_offset = out.set_offset.or(acc.set_offset);
            acc.set_hpos = out.set_hpos.or(acc.set_hpos);
        }
        fn move_to(&mut self, p: Pos2) {
            self.pos = p;
            self.step(vec![Event::PointerMoved(p)]);
        }
        fn button(&mut self, pressed: bool) {
            let pos = self.pos;
            self.step(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() }]);
        }
        /// press at `from`, move to `to` in small steps, release
        fn drag(&mut self, from: Pos2, to: Pos2) -> ScopeOutput {
            self.last = None;
            self.move_to(from);
            self.move_to(from);
            self.button(true);
            for i in 1..=10 {
                self.move_to(from + (to - from) * (i as f32 / 10.0));
            }
            self.step(vec![]);
            self.button(false);
            self.step(vec![]);
            self.last.take().unwrap()
        }
        fn at(&self, x: f32, y: f32) -> Pos2 {
            let plot = self.view.plot_rect.unwrap();
            Pos2::new(plot.left() + x / X_MAX * plot.width(), plot.bottom() - (y - Y_MIN) / (Y_MAX - Y_MIN) * plot.height())
        }
    }

    #[test]
    fn trigger_marker_can_be_dragged() {
        let mut r = Rig::new();
        let from = r.at(X_MAX, CENTRE_PX as f32) + Vec2::new(8.0, 0.0);
        let to = from + Vec2::new(0.0, -30.0);
        let out = r.drag(from, to);
        assert!(out.set_trigger_level.is_some_and(|l| l > 10), "trigger level after drag: {:?}", out.set_trigger_level);
    }

    #[test]
    fn ground_marker_can_be_dragged() {
        let mut r = Rig::new();
        let from = r.at(0.0, CENTRE_PX as f32) + Vec2::new(-10.0, 0.0);
        let out = r.drag(from, from + Vec2::new(0.0, 25.0));
        assert!(out.set_offset.is_some_and(|o| o < -5), "offset after drag: {:?}", out.set_offset);
    }

    #[test]
    fn position_marker_can_be_dragged() {
        let mut r = Rig::new();
        let from = r.at(150.0, Y_MAX) + Vec2::new(0.0, -7.0);
        let out = r.drag(from, from + Vec2::new(40.0, 0.0));
        assert!(out.set_hpos.is_some_and(|h| h > 10), "position after drag: {:?}", out.set_hpos);
    }

    #[test]
    fn cursors_can_be_dragged() {
        let mut r = Rig::new();
        r.cursors.time_on = true;
        r.cursors.volt_on = true;
        r.step(vec![]);
        let (t0, v0) = (r.cursors.t[0], r.cursors.v[0]);
        r.drag(r.at(t0, 128.0), r.at(t0, 128.0) + Vec2::new(40.0, 0.0));
        assert!(r.cursors.t[0] > t0 + 10.0, "time cursor {} -> {}", t0, r.cursors.t[0]);
        r.drag(r.at(5.0, v0), r.at(5.0, v0) + Vec2::new(0.0, 20.0));
        assert!(r.cursors.v[0] < v0 - 5.0, "voltage cursor {} -> {}", v0, r.cursors.v[0]);
    }
}
