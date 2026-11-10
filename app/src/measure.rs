//! Automatic measurements on a received frame (computed on the PC, from the screen trace).

use crate::protocol::Frame;

#[derive(Clone, Debug, Default)]
pub struct Measurements {
    pub vmax: f64,
    pub vmin: f64,
    pub vpp: f64,
    pub vavg: f64,
    pub vrms: f64,
    pub vtop: f64,
    pub vbase: f64,
    pub amplitude: f64,
    /// From mid-level crossings of the visible trace (None if fewer than one period is visible).
    pub period: Option<f64>,
    pub freq: Option<f64>,
    pub duty: Option<f64>,
    pub rise: Option<f64>,
    pub fall: Option<f64>,
    /// The scope's hardware frequency counter.
    pub freq_counter: Option<f64>,
    pub clipped: bool,
    /// overshoot above the top level / preshoot below the base level, % of the amplitude
    pub overshoot: Option<f64>,
    pub preshoot: Option<f64>,
    /// average positive / negative pulse width (mid-level crossings)
    pub pwidth: Option<f64>,
    pub nwidth: Option<f64>,
    /// number of rising and falling edges
    pub edges: usize,
}

/// Value above which a fraction `q` of the samples lies (simple histogram percentile).
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

pub fn measure(f: &Frame) -> Measurements {
    let v: Vec<f64> = f.samples().iter().map(|p| p[1]).collect();
    let mut m = measure_samples(&v, f.seconds_per_px(), f.volts_per_div());
    m.clipped = f.trace.iter().enumerate().any(|(i, &p)| f.valid(i) && (p <= 28 || p >= 228));
    if f.freq_raw > 0 {
        m.freq_counter = Some(f.freq_raw as f64);
    }
    m
}

/// Measurements over equally spaced samples (volts, `dt` seconds apart): screen trace or deep memory.
pub fn measure_samples(v: &[f64], dt: f64, volts_per_div: f64) -> Measurements {
    let mut m = Measurements::default();
    if v.len() < 2 {
        return m;
    }
    m.vmax = v.iter().cloned().fold(f64::MIN, f64::max);
    m.vmin = v.iter().cloned().fold(f64::MAX, f64::min);
    m.vpp = m.vmax - m.vmin;
    m.vavg = v.iter().sum::<f64>() / v.len() as f64;
    m.vrms = (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();
    let mut sorted = v.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    m.vtop = percentile(&sorted, 0.95);
    m.vbase = percentile(&sorted, 0.05);
    m.amplitude = m.vtop - m.vbase;

    // crossings of the mid level with hysteresis (10 % of the amplitude)
    if m.amplitude > 0.0 && m.amplitude > volts_per_div * 0.2 {
        let mid = (m.vtop + m.vbase) / 2.0;
        let hyst = m.amplitude * 0.1;
        // (sample index where detected, fractional crossing of the mid level); the first transition
        // out of the unknown initial state is not an edge
        let mut state: Option<bool> = None;
        let mut rising: Vec<(usize, f64)> = Vec::new();
        let mut falling: Vec<(usize, f64)> = Vec::new();
        for (i, &x) in v.iter().enumerate().skip(1) {
            if x > mid + hyst && state != Some(true) {
                if state == Some(false) {
                    rising.push((i, crossing_back(v, i, mid)));
                }
                state = Some(true);
            } else if x < mid - hyst && state != Some(false) {
                if state == Some(true) {
                    falling.push((i, crossing_back(v, i, mid)));
                }
                state = Some(false);
            }
        }
        let period_from = |e: &[(usize, f64)]| -> Option<f64> {
            (e.len() >= 2).then(|| (e[e.len() - 1].1 - e[0].1) / (e.len() - 1) as f64)
        };
        let period = period_from(&rising).or_else(|| period_from(&falling));
        m.period = period.map(|p| p * dt);
        m.freq = m.period.filter(|p| *p > 0.0).map(|p| 1.0 / p);
        // duty: average over every complete period
        let duties: Vec<f64> = rising
            .windows(2)
            .filter_map(|w| {
                let (r0, r1) = (w[0].1, w[1].1);
                falling.iter().find(|f| f.1 > r0 && f.1 < r1).map(|f| (f.1 - r0) / (r1 - r0) * 100.0)
            })
            .collect();
        let avg = |x: Vec<f64>| (!x.is_empty()).then(|| x.iter().sum::<f64>() / x.len() as f64 * dt);
        m.pwidth = avg(rising.iter().filter_map(|r| falling.iter().find(|f| f.1 > r.1).map(|f| f.1 - r.1)).collect());
        m.nwidth = avg(falling.iter().filter_map(|f| rising.iter().find(|r| r.1 > f.1).map(|r| r.1 - f.1)).collect());
        m.edges = rising.len() + falling.len();
        m.overshoot = Some(((m.vmax - m.vtop) / m.amplitude * 100.0).max(0.0));
        m.preshoot = Some(((m.vbase - m.vmin) / m.amplitude * 100.0).max(0.0));
        if !duties.is_empty() {
            m.duty = Some((duties.iter().sum::<f64>() / duties.len() as f64).clamp(0.0, 100.0));
        }
        // 10..90 % times: median over all complete edges
        let lo = m.vbase + m.amplitude * 0.1;
        let hi = m.vbase + m.amplitude * 0.9;
        let max_len = period.map(|p| (p / 2.0) as usize).unwrap_or(v.len()).max(2);
        m.rise = median(rising.iter().filter_map(|e| edge_span(v, e.0, lo, hi, true, max_len)).collect()).map(|n| n * dt);
        m.fall = median(falling.iter().filter_map(|e| edge_span(v, e.0, lo, hi, false, max_len)).collect()).map(|n| n * dt);
    }
    m
}

/// Fractional index where the signal crossed `level` last before sample i.
fn crossing_back(v: &[f64], i: usize, level: f64) -> f64 {
    let above = v[i] > level;
    let mut j = i;
    while j > 0 && (v[j - 1] > level) == above {
        j -= 1;
    }
    if j == 0 { 0.0 } else { interp(v, j, level) }
}

/// Fractional sample index where the signal crosses `level` between i-1 and i.
fn interp(v: &[f64], i: usize, level: f64) -> f64 {
    let (a, b) = (v[i - 1], v[i]);
    if (b - a).abs() < 1e-12 { i as f64 } else { (i - 1) as f64 + (level - a) / (b - a) }
}

/// Duration in samples of the edge around sample i from the 10 % to the 90 % level (or back for a
/// falling edge). None if the edge is cut by the record ends or longer than `max_len`.
fn edge_span(v: &[f64], i: usize, lo: f64, hi: f64, rising: bool, max_len: usize) -> Option<f64> {
    let (start_lvl, end_lvl) = if rising { (lo, hi) } else { (hi, lo) };
    let before = |x: f64| if rising { x > start_lvl } else { x < start_lvl };
    let not_done = |x: f64| if rising { x < end_lvl } else { x > end_lvl };
    let mut a = i;
    while a > 0 && before(v[a - 1]) {
        a -= 1;
        if i - a > max_len {
            return None;
        }
    }
    if a == 0 {
        return None;
    }
    let mut b = i;
    while b < v.len() && not_done(v[b]) {
        b += 1;
        if b - i > max_len {
            return None;
        }
    }
    if b >= v.len() {
        return None;
    }
    let t0 = interp(v, a, start_lvl);
    let t1 = if b == 0 { 0.0 } else { interp(v, b, end_lvl) };
    Some((t1 - t0).max(0.0))
}

fn median(mut x: Vec<f64>) -> Option<f64> {
    if x.is_empty() {
        return None;
    }
    x.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(x[x.len() / 2])
}

/// Engineering formatting: 1.23 mV, 4.56 kHz, ...
pub fn eng(value: f64, unit: &str) -> String {
    if !value.is_finite() {
        return format!("--- {unit}");
    }
    let a = value.abs();
    let (scale, prefix) = if a == 0.0 {
        (1.0, "")
    } else if a < 1e-6 {
        (1e9, "n")
    } else if a < 1e-3 {
        (1e6, "µ")
    } else if a < 1.0 {
        (1e3, "m")
    } else if a < 1e3 {
        (1.0, "")
    } else if a < 1e6 {
        (1e-3, "k")
    } else {
        (1e-6, "M")
    };
    let x = value * scale;
    let digits = if x.abs() >= 100.0 { 1 } else if x.abs() >= 10.0 { 2 } else { 3 };
    format!("{x:.digits$} {prefix}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TRACE_POINTS;

    #[test]
    fn square_wave_measurements() {
        // 1 V/div, 1 ms/div: square wave 0..2 V (50 px), period 50 px = 2 ms, duty 50 %
        let trace: Vec<u8> = (0..TRACE_POINTS).map(|i| if (i / 25) % 2 == 0 { 178 } else { 128 }).collect();
        let f = Frame { trace, timebase: 13, vdiv: 6, trace_start: 0, trace_end: 0, ..Default::default() };
        let m = measure(&f);
        assert!((m.vmax - 2.0).abs() < 1e-9);
        assert!((m.vmin - 0.0).abs() < 1e-9);
        assert!((m.period.unwrap() - 2e-3 * crate::protocol::TIME_SCALE).abs() < 1e-5, "{:?}", m.period);
        assert!((m.duty.unwrap() - 50.0).abs() < 3.0, "{:?}", m.duty);
    }

    #[test]
    fn eng_format() {
        assert_eq!(eng(0.00123, "V"), "1.230 mV");
        assert_eq!(eng(12345.0, "Hz"), "12.35 kHz");
    }
}
