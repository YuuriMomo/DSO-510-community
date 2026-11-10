//! Saving captures: PNG screenshot (own renderer, independent of the window), CSV, sessions.
//! Files go to Documents\DSO-510 with a timestamp in the name.

use crate::measure::{self, Measurements};
use crate::protocol::{self, Frame, CENTRE_PX};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn captures_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("Documents").join("DSO-510");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Local-time-free timestamp "YYYYMMDD-HHMMSS" (UTC) for file names.
pub fn stamp() -> String {
    let s = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, secs) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    // civil date from days since 1970-01-01 (H. Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

// ---------------------------------------------------------------------------------------------
// PNG

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        let mut px = vec![0u8; w * h * 3];
        for c in px.chunks_mut(3) {
            c.copy_from_slice(&bg);
        }
        Self { w, h, px }
    }
    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 3;
        for k in 0..3 {
            self.px[i + k] = (self.px[i + k] as f32 * (1.0 - a) + c[k] as f32 * a) as u8;
        }
    }
    fn dot(&mut self, x: f32, y: f32, c: [u8; 3], r: f32) {
        let (x0, x1, y0, y1) = ((x - r).floor() as i32, (x + r).ceil() as i32, (y - r).floor() as i32, (y + r).ceil() as i32);
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                let d = ((xx as f32 - x).powi(2) + (yy as f32 - y).powi(2)).sqrt();
                let a = (r + 0.5 - d).clamp(0.0, 1.0);
                if a > 0.0 {
                    self.blend(xx, yy, c, a);
                }
            }
        }
    }
    fn line(&mut self, a: (f32, f32), b: (f32, f32), c: [u8; 3], r: f32) {
        let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) * 2.0).ceil().max(1.0) as usize;
        for i in 0..=n {
            let t = i as f32 / n as f32;
            self.dot(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, c, r);
        }
    }
    fn dashed(&mut self, a: (f32, f32), b: (f32, f32), c: [u8; 3]) {
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let n = (len / 4.0) as usize;
        for i in (0..n).step_by(2) {
            let t0 = i as f32 / n as f32;
            let t1 = (i + 1) as f32 / n as f32;
            self.line((a.0 + (b.0 - a.0) * t0, a.1 + (b.1 - a.1) * t0), (a.0 + (b.0 - a.0) * t1, a.1 + (b.1 - a.1) * t1), c, 0.5);
        }
    }
    /// 5x7 bitmap text (digits, letters, a few symbols), scale s.
    fn text(&mut self, x: f32, y: f32, s: &str, c: [u8; 3], scale: f32) {
        let mut cx = x;
        for ch in s.chars() {
            if let Some(rows) = glyph(ch) {
                for (ry, row) in rows.iter().enumerate() {
                    for rx in 0..5 {
                        if row & (0x10 >> rx) != 0 {
                            for dy in 0..scale as i32 {
                                for dx in 0..scale as i32 {
                                    self.blend((cx + rx as f32 * scale) as i32 + dx, (y + ry as f32 * scale) as i32 + dy, c, 1.0);
                                }
                            }
                        }
                    }
                }
            }
            cx += 6.0 * scale;
        }
    }
}

fn glyph(c: char) -> Option<[u8; 7]> {
    let c = c.to_ascii_uppercase();
    Some(match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        'µ' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x1E, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '/' => [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x00],
        '=' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '×' => [0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x00],
        ' ' => [0; 7],
        _ => return None,
    })
}

/// Renders the frame like the scope screen (1200 x 900) with measurements underneath and saves a PNG.
pub fn save_png(f: &Frame, m: &Measurements, title: &str) -> std::io::Result<PathBuf> {
    let (w, h) = (1200usize, 900usize);
    let mut cv = Canvas::new(w, h, [8, 10, 12]);
    let (l, t, pw, ph) = (60.0f32, 70.0f32, 1080.0f32, 720.0f32);
    let grid = [55, 60, 66];
    let axis = [90, 96, 104];
    for i in 0..=12 {
        let x = l + pw * i as f32 / 12.0;
        cv.dashed((x, t), (x, t + ph), if i == 6 { axis } else { grid });
    }
    for j in 0..=8 {
        let y = t + ph * j as f32 / 8.0;
        cv.dashed((l, y), (l + pw, y), if j == 4 { axis } else { grid });
    }
    let sx = |x: f32| l + x / 300.0 * pw;
    let sy = |v: f32| t + ph - (v - 28.0) / 200.0 * ph;
    let yellow = [255, 214, 10];
    let mut prev: Option<(f32, f32)> = None;
    for (i, &px) in f.trace.iter().enumerate() {
        if !f.valid(i) {
            prev = None;
            continue;
        }
        let p = (sx(i as f32), sy(px as f32).clamp(t, t + ph));
        if let Some(q) = prev {
            cv.line(q, p, yellow, 1.3);
        }
        prev = Some(p);
    }
    let ty = sy((CENTRE_PX as f32) + f.trig_level as f32).clamp(t, t + ph);
    cv.line((l + pw + 4.0, ty), (l + pw + 20.0, ty - 8.0), [255, 120, 40], 1.5);
    cv.line((l + pw + 4.0, ty), (l + pw + 20.0, ty + 8.0), [255, 120, 40], 1.5);

    let white = [220, 220, 220];
    cv.text(l, 24.0, title, white, 3.0);
    let info = format!(
        "CH1 {}/DIV {}   {}/DIV   TRIG {} {} {}",
        f.vdiv_label(),
        protocol::COUPLING_LABELS.get(f.coupling as usize).copied().unwrap_or("?"),
        f.timebase_label(),
        protocol::TRIG_MODE_LABELS.get(f.trig_mode as usize).copied().unwrap_or("?"),
        if f.trig_edge == 0 { "RISING" } else { "FALLING" },
        measure::eng(f.trigger_volts(), "V")
    );
    cv.text(l, t + ph + 18.0, &info, yellow, 2.0);
    let fmt_opt = |x: Option<f64>, u: &str| x.map(|v| measure::eng(v, u)).unwrap_or_else(|| "---".into());
    let line1 = format!("VPP {}  VMAX {}  VMIN {}  VAVG {}  VRMS {}",
        measure::eng(m.vpp, "V"), measure::eng(m.vmax, "V"), measure::eng(m.vmin, "V"), measure::eng(m.vavg, "V"), measure::eng(m.vrms, "V"));
    let line2 = format!("FREQ {}  PERIOD {}  DUTY {}",
        fmt_opt(m.freq.or(m.freq_counter), "HZ"), fmt_opt(m.period, "S"), m.duty.map(|d| format!("{d:.1}%")).unwrap_or_else(|| "---".into()));
    cv.text(l, t + ph + 46.0, &line1, white, 2.0);
    cv.text(l, t + ph + 70.0, &line2, white, 2.0);

    let path = captures_dir().join(format!("dso510-{}.png", stamp()));
    let file = std::fs::File::create(&path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(std::io::Error::other)?;
    wr.write_image_data(&cv.px).map_err(std::io::Error::other)?;
    Ok(path)
}

// ---------------------------------------------------------------------------------------------
// CSV

pub fn save_csv(f: &Frame) -> std::io::Result<PathBuf> {
    let path = captures_dir().join(format!("dso510-{}.csv", stamp()));
    let mut s = String::from("# DSO-510 frame\n");
    s.push_str(&format!("# V/div={} time/div={} coupling={} probe=x{} trigger={} {} level={}\n",
        f.vdiv_label(), f.timebase_label(),
        protocol::COUPLING_LABELS.get(f.coupling as usize).copied().unwrap_or("?"),
        if f.probe == 1 { 10 } else { 1 },
        protocol::TRIG_MODE_LABELS.get(f.trig_mode as usize).copied().unwrap_or("?"),
        protocol::TRIG_EDGE_LABELS.get(f.trig_edge as usize).copied().unwrap_or("?"),
        measure::eng(f.trigger_volts(), "V")));
    s.push_str("time_s,volts,trace_px\n");
    let dt = f.seconds_per_px();
    for (i, &px) in f.trace.iter().enumerate() {
        if f.valid(i) {
            s.push_str(&format!("{:.9},{:.6},{}\n", i as f64 * dt, protocol::px_to_volts(px as f64, f.volts_per_div(), f.offset), px));
        }
    }
    std::fs::write(&path, s)?;
    Ok(path)
}

/// Deep-memory capture (samples [a, b)) as CSV: time relative to the trigger, volts, ADC code.
pub fn save_deep_csv(c: &crate::deep::DeepCapture, a: usize, b: usize) -> std::io::Result<PathBuf> {
    let path = captures_dir().join(format!("dso510-deep-{}.csv", stamp()));
    let (a, b) = (a.min(c.len()), b.min(c.len()));
    let i = &c.info;
    let mut s = String::with_capacity((b - a) * 28 + 400);
    s.push_str("# DSO-510 deep memory capture
");
    s.push_str(&format!(
        "# received={} samples={} of {} sample_rate={} V/div={} time/div={} coupling={} probe=x{} trigger={} {} level={}
",
        c.when, b - a, c.len(), measure::eng(c.sample_rate(), "Sa/s"),
        measure::eng(c.volts_per_div(), "V"),
        protocol::TIMEBASE_LABELS[(i.timebase_applied as usize).min(26)],
        protocol::COUPLING_LABELS.get(i.coupling as usize).copied().unwrap_or("?"),
        if i.probe == 1 { 10 } else { 1 },
        protocol::TRIG_MODE_LABELS.get(i.trig_mode as usize).copied().unwrap_or("?"),
        protocol::TRIG_EDGE_LABELS.get(i.trig_edge as usize).copied().unwrap_or("?"),
        measure::eng(c.trigger_volts(), "V")));
    s.push_str("time_s,volts,adc_code
");
    for k in a..b {
        s.push_str(&format!("{:.10e},{:.5},{}
", c.time(k as f64), c.volts(c.codes[k]), c.codes[k]));
    }
    std::fs::write(&path, s)?;
    Ok(path)
}

pub fn save_raw(data: &[u8]) -> std::io::Result<PathBuf> {
    let path = captures_dir().join(format!("dso510-raw-{}.bin", stamp()));
    std::fs::write(&path, data)?;
    Ok(path)
}

// ---------------------------------------------------------------------------------------------
// session: all frames of the history, binary ("DSO510S1", count, then 24-byte header + 301 bytes each)

const MAGIC: &[u8; 8] = b"DSO510S1";

fn frame_bytes(f: &Frame) -> Vec<u8> {
    let mut b = Vec::with_capacity(24 + protocol::TRACE_POINTS);
    b.extend_from_slice(&f.seq.to_le_bytes());
    b.extend_from_slice(&[f.app_mode, f.timebase, f.vdiv, f.probe, f.coupling, f.trig_mode, f.trig_edge, f.running as u8]);
    b.extend_from_slice(&f.trig_level.to_le_bytes());
    b.push(f.offset as u8);
    b.push(0);
    b.extend_from_slice(&f.freq_raw.to_le_bytes());
    b.extend_from_slice(&f.trace_start.to_le_bytes());
    b.extend_from_slice(&f.trace_end.to_le_bytes());
    let mut trace = f.trace.clone();
    trace.resize(protocol::TRACE_POINTS, 0);
    b.extend_from_slice(&(protocol::TRACE_POINTS as u16).to_le_bytes());
    b.extend_from_slice(&trace);
    b
}

pub fn save_session<'a>(frames: impl ExactSizeIterator<Item = &'a Frame>) -> std::io::Result<PathBuf> {
    let path = captures_dir().join(format!("dso510-session-{}.dsos", stamp()));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path)?);
    out.write_all(MAGIC)?;
    out.write_all(&(frames.len() as u32).to_le_bytes())?;
    for f in frames {
        out.write_all(&frame_bytes(f))?;
    }
    Ok(path)
}

pub fn load_session(path: &std::path::Path) -> std::io::Result<Vec<Frame>> {
    let mut data = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut data)?;
    if data.len() < 12 || &data[..8] != MAGIC {
        return Err(std::io::Error::other("not a DSO-510 session file"));
    }
    let n = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let rec = 24 + protocol::TRACE_POINTS;
    let mut frames = Vec::with_capacity(n);
    let mut parser = protocol::Parser::default();
    for i in 0..n {
        let o = 12 + i * rec;
        if o + rec > data.len() {
            break;
        }
        // reuse the wire decoder: wrap the record as an 'F' message
        let payload = &data[o..o + rec];
        let mut msg = b"DSOF".to_vec();
        msg.extend_from_slice(&(rec as u16).to_le_bytes());
        msg.extend_from_slice(payload);
        let crc = protocol::crc16(&msg[3..], 0xFFFF);
        msg.extend_from_slice(&crc.to_le_bytes());
        let mut out = Vec::new();
        parser.feed(&msg, &mut out);
        for m in out {
            if let protocol::Message::Frame(f) = m {
                frames.push(f);
            }
        }
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trip() {
        let f = Frame { seq: 7, timebase: 13, vdiv: 6, trig_level: -12, offset: 5, trace: (0..301).map(|i| (i % 200) as u8).collect(), ..Default::default() };
        let dir = std::env::temp_dir().join(format!("dso510-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.dsos");
        let mut out = std::fs::File::create(&p).unwrap();
        out.write_all(MAGIC).unwrap();
        out.write_all(&1u32.to_le_bytes()).unwrap();
        out.write_all(&frame_bytes(&f)).unwrap();
        drop(out);
        let back = load_session(&p).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].seq, 7);
        assert_eq!(back[0].trig_level, -12);
        assert_eq!(back[0].trace, f.trace);
    }

    #[test]
    fn stamp_format() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "-");
    }
}
