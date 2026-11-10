//! Spectrum analysis (FFT) of equally spaced samples: magnitude in dBV (RMS), fundamental with
//! interpolated frequency, harmonics, THD, SFDR and noise floor.

use rustfft::{num_complex::Complex, FftPlanner};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Rectangular,
    Hann,
    BlackmanHarris,
    FlatTop,
}

impl Window {
    pub const ALL: [Window; 4] = [Window::Hann, Window::BlackmanHarris, Window::FlatTop, Window::Rectangular];
    pub fn label(self) -> &'static str {
        match self {
            Window::Rectangular => "Rectangular",
            Window::Hann => "Hann",
            Window::BlackmanHarris => "Blackman-Harris",
            Window::FlatTop => "Flat top",
        }
    }
    /// half width of the main lobe in bins (peak search / exclusion zone)
    fn lobe(self) -> usize {
        match self {
            Window::Rectangular => 1,
            Window::Hann => 2,
            Window::BlackmanHarris => 4,
            Window::FlatTop => 5,
        }
    }
    fn coef(self, i: usize, n: usize) -> f64 {
        let x = 2.0 * std::f64::consts::PI * i as f64 / (n.max(2) - 1) as f64;
        match self {
            Window::Rectangular => 1.0,
            Window::Hann => 0.5 - 0.5 * x.cos(),
            Window::BlackmanHarris => 0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos(),
            Window::FlatTop => {
                0.21557895 - 0.41663158 * x.cos() + 0.277263158 * (2.0 * x).cos() - 0.083578947 * (3.0 * x).cos()
                    + 0.006947368 * (4.0 * x).cos()
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Harmonic {
    pub order: usize,
    pub freq: f64,
    /// RMS volts
    pub vrms: f64,
    /// relative to the fundamental
    pub dbc: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Spectrum {
    /// bin spacing (Hz)
    pub df: f64,
    /// RMS magnitude in dBV per bin, 0..=n/2
    pub db: Vec<f32>,
    pub fundamental: Option<Harmonic>,
    /// orders 2.. below Nyquist
    pub harmonics: Vec<Harmonic>,
    /// total harmonic distortion in percent
    pub thd: Option<f64>,
    /// spurious-free dynamic range (dBc)
    pub sfdr: Option<f64>,
    /// median bin level (dBV)
    pub noise_floor: f32,
    pub samples: usize,
    pub fft_size: usize,
}

impl Spectrum {
    pub fn nyquist(&self) -> f64 {
        self.df * (self.db.len().saturating_sub(1)) as f64
    }
}

fn db_v(vrms: f64) -> f32 {
    (20.0 * vrms.max(1e-9).log10()) as f32
}

/// `v` in volts, `dt` seconds between samples. The mean is removed (DC is shown in bin 0 separately).
pub fn compute(v: &[f64], dt: f64, window: Window) -> Spectrum {
    let len = v.len();
    if len < 16 || dt <= 0.0 {
        return Spectrum::default();
    }
    let n = len.next_power_of_two().min(1 << 16);
    let len = len.min(n);
    let mean = v[..len].iter().sum::<f64>() / len as f64;
    let w: Vec<f64> = (0..len).map(|i| window.coef(i, len)).collect();
    let wsum: f64 = w.iter().sum();
    let wsq: f64 = w.iter().map(|x| x * x).sum();
    let mut buf: Vec<Complex<f64>> = (0..n).map(|i| if i < len { Complex::new((v[i] - mean) * w[i], 0.0) } else { Complex::new(0.0, 0.0) }).collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);
    // peak amplitude of a sine = 2|X| / sum(w); RMS = peak / sqrt(2)
    let half = n / 2;
    let scale = 2.0 / wsum / std::f64::consts::SQRT_2;
    let vrms: Vec<f64> = (0..=half).map(|k| buf[k].norm() * scale).collect();
    let mut db: Vec<f32> = vrms.iter().map(|&x| db_v(x)).collect();
    db[0] = db_v(mean.abs()); // DC level
    let df = 1.0 / (n as f64 * dt);

    let mut sorted = db[1..].to_vec();
    let mut s = Spectrum { df, samples: len, fft_size: n, db, ..Default::default() };
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s.noise_floor = sorted.get(sorted.len() / 2).copied().unwrap_or(-200.0);

    let lobe = window.lobe();
    let first = (lobe + 1).max(2);
    if half > first + 2 {
        // fundamental: strongest bin above DC, frequency by parabolic interpolation of the dB values
        let k0 = (first..half).max_by(|&a, &b| vrms[a].partial_cmp(&vrms[b]).unwrap()).unwrap();
        if s.db[k0] > s.noise_floor + 15.0 {
            let interp = |k: usize| -> f64 {
                if k == 0 || k + 1 > half {
                    return k as f64;
                }
                let (a, b, c) = (s.db[k - 1] as f64, s.db[k] as f64, s.db[k + 1] as f64);
                let d = a - 2.0 * b + c;
                if d.abs() < 1e-12 { k as f64 } else { k as f64 + 0.5 * (a - c) / d }
            };
            // amplitude: RMS sum over the main lobe (window-independent, exact for flat top)
            let band = |k: usize| -> f64 {
                let a = k.saturating_sub(lobe).max(1);
                let b = (k + lobe).min(half);
                let p: f64 = (a..=b).map(|i| buf[i].norm_sqr()).sum();
                // Parseval: Vrms^2 = 2 * sum|X|^2 / (n * sum w^2) over the positive half
                (2.0 * p / (n as f64 * wsq)).sqrt()
            };
            let f0 = interp(k0) * df;
            let v1 = band(k0);
            s.fundamental = Some(Harmonic { order: 1, freq: f0, vrms: v1, dbc: 0.0 });
            let mut hsum = 0.0;
            for order in 2..=10 {
                let target = f0 * order as f64 / df;
                if target + lobe as f64 >= half as f64 {
                    break;
                }
                let c = target.round() as usize;
                let a = c.saturating_sub(lobe).max(1);
                let b = (c + lobe).min(half - 1);
                let k = (a..=b).max_by(|&x, &y| vrms[x].partial_cmp(&vrms[y]).unwrap()).unwrap();
                let vh = band(k);
                hsum += vh * vh;
                s.harmonics.push(Harmonic { order, freq: interp(k) * df, vrms: vh, dbc: 20.0 * (vh / v1).max(1e-12).log10() });
            }
            if !s.harmonics.is_empty() {
                s.thd = Some(hsum.sqrt() / v1 * 100.0);
            }
            // SFDR: strongest bin outside the fundamental's lobe
            let spur = (first..half).filter(|&k| k + lobe < k0 || k > k0 + lobe).map(|k| vrms[k]).fold(0.0, f64::max);
            if spur > 0.0 {
                s.sfdr = Some(20.0 * (vrms[k0] / spur).log10());
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_amplitude_frequency_and_thd() {
        // 1 V peak sine at 1 kHz + 10 % third harmonic, 1 MS/s, 30000 samples
        let dt = 1e-6;
        let v: Vec<f64> = (0..30000)
            .map(|i| {
                let t = i as f64 * dt;
                (2.0 * std::f64::consts::PI * 1000.0 * t).sin() + 0.1 * (2.0 * std::f64::consts::PI * 3000.0 * t).sin() + 0.5
            })
            .collect();
        for w in [Window::Hann, Window::BlackmanHarris, Window::FlatTop] {
            let s = compute(&v, dt, w);
            let f = s.fundamental.clone().unwrap();
            assert!((f.freq - 1000.0).abs() < 5.0, "{w:?} {}", f.freq);
            assert!((f.vrms - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.03, "{w:?} {}", f.vrms);
            assert_eq!(s.db.len(), s.fft_size / 2 + 1, "{w:?} the plot needs the bins");
            let k = (1000.0 / s.df).round() as usize;
            assert!(s.db[k] > s.noise_floor + 40.0, "{w:?} peak {} floor {}", s.db[k], s.noise_floor);
            let thd = s.thd.unwrap();
            assert!((thd - 10.0).abs() < 1.0, "{w:?} thd {thd}");
            assert!((s.harmonics[1].dbc + 20.0).abs() < 1.0, "{w:?} {:?}", s.harmonics[1]);
        }
    }
}
