//! Deep memory: one complete acquisition (30100 ADC samples) as sent by firmware usb_link v3 ("DEEP 1").
//!
//! Layout verified on hardware (pc/dso510-link/examples/deeptest.rs):
//! - sample code = screen pixel (the trace the scope draws is the ADC code, 25 px/div, centre 128);
//! - screen column x shows sample `trig + (x - 150 - hpos) * spp_view`;
//! - time base < 4: four DMA blocks of 7525 samples, interleaved sample n = block[(r + n) % 4][n / 4],
//!   the block phase `r` changes from capture to capture and is found from the signal itself;
//! - Single mode: circular buffer, oldest sample at `start`;
//! - linear captures start with 1..3 stale samples per DMA block (ADC pipeline), they are dropped.

use crate::measure::{self, Measurements};
use crate::protocol::{px_to_volts, DeepInfo, PX_PER_DIV, TIMEBASE_SECONDS, TIME_SCALE, VDIV_VOLTS};

#[derive(Clone, Debug)]
pub struct DeepCapture {
    pub info: DeepInfo,
    /// ADC codes in time order
    pub codes: Vec<u8>,
    /// index of the trigger point in `codes` (time 0)
    pub trig: f64,
    /// seconds between samples
    pub dt: f64,
    /// local time of reception (export::stamp format)
    pub when: String,
    /// roll-mode stream (time 0 = start of the record) instead of a triggered acquisition
    pub stream: bool,
}

const BLOCKS: usize = 4;
/// samples per DMA block dropped at the start of a linear capture (stale ADC pipeline output)
const STALE_SAMPLES: usize = 4;

impl DeepCapture {
    pub fn assemble(info: DeepInfo, raw: &[u8]) -> Self {
        let n = raw.len();
        let start = info.start as usize % n.max(1);
        let codes: Vec<u8> = if info.interleave && n % BLOCKS == 0 {
            let bl = n / BLOCKS;
            let bstart = start / BLOCKS;
            let block = |b: usize, i: usize| raw[b * bl + (bstart + i) % bl];
            // phase: the rotation with the smoothest result
            let rough = |r: usize| -> u64 {
                (1..n).map(|k| (block((r + k) % BLOCKS, k / BLOCKS) as i32 - block((r + k - 1) % BLOCKS, (k - 1) / BLOCKS) as i32).unsigned_abs() as u64).sum()
            };
            let r = (0..BLOCKS).min_by_key(|&r| rough(r)).unwrap_or(0);
            (0..n).map(|k| block((r + k) % BLOCKS, k / BLOCKS)).collect()
        } else {
            raw[start..].iter().chain(raw[..start].iter()).copied().collect()
        };
        let trig = if info.trig == 0xFFFF {
            n as f64 / 2.0
        } else {
            ((info.trig as usize + n - start) % n.max(1)) as f64
        };
        // the external ADC is pipelined: the first samples after DMA start are stale (linear capture only)
        let (codes, trig) = if start == 0 && n > 64 {
            let skip = if info.interleave { STALE_SAMPLES * BLOCKS } else { STALE_SAMPLES };
            (codes[skip..].to_vec(), trig - skip as f64)
        } else {
            (codes, trig)
        };
        let spp = if info.spp_capture > 0.0 { info.spp_capture as f64 } else { 1.0 };
        let dt = TIMEBASE_SECONDS[(info.timebase_applied as usize).min(26)] / PX_PER_DIV / spp * TIME_SCALE;
        DeepCapture { info, codes, trig, dt, when: crate::export::stamp(), stream: false }
    }

    pub fn volts_per_div(&self) -> f64 {
        VDIV_VOLTS[(self.info.probe as usize * 3 + self.info.vdiv as usize).min(VDIV_VOLTS.len() - 1)]
    }
    pub fn volts(&self, code: u8) -> f64 {
        px_to_volts(code as f64, self.volts_per_div(), self.info.offset)
    }
    pub fn len(&self) -> usize {
        self.codes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }
    /// time of sample i relative to the trigger
    pub fn time(&self, i: f64) -> f64 {
        (i - self.trig) * self.dt
    }
    pub fn index_at(&self, t: f64) -> f64 {
        t / self.dt + self.trig
    }
    pub fn t_first(&self) -> f64 {
        self.time(0.0)
    }
    pub fn t_last(&self) -> f64 {
        self.time(self.len().saturating_sub(1) as f64)
    }
    /// Time window shown on the scope's own screen.
    pub fn screen_window(&self) -> (f64, f64) {
        if self.stream {                                  // the scope shows the newest 301 columns
            let last = self.len().saturating_sub(1) as f64;
            return (self.time((last - 300.0).max(0.0)), self.time(last));
        }
        let tpx = TIMEBASE_SECONDS[(self.info.timebase as usize).min(26)] / PX_PER_DIV * TIME_SCALE;
        let h = self.info.hpos as f64;
        ((-150.0 - h) * tpx, (150.0 - h) * tpx)
    }
    pub fn sample_rate(&self) -> f64 {
        1.0 / self.dt
    }
    pub fn trigger_volts(&self) -> f64 {
        (self.info.trig_level as f64 - self.info.offset as f64) / PX_PER_DIV * self.volts_per_div()
    }
    /// Measurements over samples [a, b) (whole record by default).
    pub fn measure_range(&self, a: usize, b: usize) -> Measurements {
        let (a, b) = (a.min(self.len()), b.min(self.len()));
        if a >= b {
            return Measurements::default();
        }
        let v: Vec<f64> = self.codes[a..b].iter().map(|&c| self.volts(c)).collect();
        let mut m = measure::measure_samples(&v, self.dt, self.volts_per_div());
        m.clipped = self.codes[a..b].iter().any(|&c| c <= 28 || c >= 228);
        m
    }
    pub fn measure(&self) -> Measurements {
        self.measure_range(0, self.len())
    }
    /// min/max of the codes in [a, b)
    pub fn min_max(&self, a: usize, b: usize) -> Option<(u8, u8)> {
        let s = self.codes.get(a.min(self.len())..b.min(self.len()))?;
        Some((*s.iter().min()?, *s.iter().max()?))
    }
}

/// Longest roll record kept (samples); at 500 samples/s (50 ms/div) that is over an hour.
pub const ROLL_MAX_SAMPLES: usize = 2_000_000;

/// Builds one long record from the roll-mode stream. A new record starts when the settings that give the
/// samples their meaning change (time base, V/div, probe, coupling, offset) or the firmware says so.
#[derive(Default)]
pub struct RollStream {
    cap: Option<DeepCapture>,
    key: Option<(u8, u8, u8, u8, i8)>,
    next: u32,
    /// grows with every received sample (cache key for measurements)
    pub version: u64,
    /// samples that never arrived (replaced by the last value), over all records
    pub lost: u64,
}

impl RollStream {
    pub fn capture(&self) -> Option<&DeepCapture> {
        self.cap.as_ref()
    }

    pub fn clear(&mut self) {
        *self = RollStream::default();
    }

    /// Returns true when this chunk started a new record.
    pub fn ingest(&mut self, c: crate::protocol::RollChunk) -> bool {
        let key = (c.timebase, c.vdiv, c.probe, c.coupling, c.offset);
        let restart = c.flags & 2 != 0;
        let mut fresh = false;
        if self.cap.is_none() || self.key != Some(key) || restart || c.index < self.next {
            let tb = (c.timebase as usize).min(26);
            let info = DeepInfo {
                timebase: c.timebase,
                timebase_applied: c.timebase,
                vdiv: c.vdiv,
                probe: c.probe,
                coupling: c.coupling,
                offset: c.offset,
                spp_capture: 1.0,
                spp_view: 1.0,
                running: true,
                ..Default::default()
            };
            self.cap = Some(DeepCapture {
                info,
                codes: Vec::new(),
                trig: 0.0,
                dt: TIMEBASE_SECONDS[tb] / PX_PER_DIV * TIME_SCALE,
                when: crate::export::stamp(),
                stream: true,
            });
            self.key = Some(key);
            self.next = c.index;
            fresh = true;
        }
        let Some(cap) = self.cap.as_mut() else { return fresh };
        if c.index > self.next {
            // samples were lost on the way: repeat the last value so that the time axis stays right
            let gap = ((c.index - self.next) as usize).min(100_000);
            self.lost += gap as u64;
            let fill = cap.codes.last().copied().unwrap_or(128);
            cap.codes.extend(std::iter::repeat_n(fill, gap));
        }
        cap.codes.extend_from_slice(&c.samples);
        self.next = c.index.wrapping_add(c.samples.len() as u32);
        if cap.codes.len() > ROLL_MAX_SAMPLES {
            let drop = cap.codes.len() - ROLL_MAX_SAMPLES + ROLL_MAX_SAMPLES / 20;
            cap.codes.drain(..drop);
            cap.trig -= drop as f64;                      // time 0 stays where it was
        }
        cap.info.running = c.flags & 1 != 0;
        self.version += 1;
        fresh
    }
}

/// Collects the 'D' header and the following 'R' chunks into captures.
#[derive(Default)]
pub struct Assembler {
    cur: Option<(DeepInfo, Vec<u8>, usize)>,
    pub incomplete: u64,
}

impl Assembler {
    pub fn header(&mut self, d: DeepInfo) {
        if self.cur.is_some() {
            self.incomplete += 1;
        }
        let n = d.samples as usize;
        self.cur = Some((d, vec![0; n], 0));
    }
    /// Returns a finished capture. Chunks that do not follow a header (e.g. a manual RAW dump) are ignored.
    pub fn chunk(&mut self, offset: u32, data: &[u8]) -> Option<DeepCapture> {
        let (_, raw, got) = self.cur.as_mut()?;
        let o = offset as usize;
        if o != *got || o + data.len() > raw.len() {
            self.incomplete += 1;
            self.cur = None;
            return None;
        }
        raw[o..o + data.len()].copy_from_slice(data);
        *got += data.len();
        if *got >= raw.len() {
            let (d, raw, _) = self.cur.take()?;
            return Some(DeepCapture::assemble(d, &raw));
        }
        None
    }
    pub fn in_progress(&self) -> bool {
        self.cur.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(index: u32, tb: u8, flags: u8, samples: &[u8]) -> crate::protocol::RollChunk {
        crate::protocol::RollChunk { index, timebase: tb, vdiv: 6, flags, samples: samples.to_vec(), ..Default::default() }
    }

    #[test]
    fn roll_stream_builds_one_long_record() {
        let mut r = RollStream::default();
        assert!(r.ingest(chunk(0, 18, 3, &[1, 2, 3])));
        assert!(!r.ingest(chunk(3, 18, 1, &[4, 5])));
        let c = r.capture().unwrap();
        assert_eq!(c.codes, vec![1, 2, 3, 4, 5]);
        assert!((c.dt - 2e-3 * TIME_SCALE).abs() < 1e-12, "{}", c.dt); // 50 ms/div = 2 ms per column, nominal
        assert!(c.stream && c.time(0.0) == 0.0);
        // lost samples are filled with the last value
        r.ingest(chunk(8, 18, 1, &[9]));
        assert_eq!(r.capture().unwrap().codes, vec![1, 2, 3, 4, 5, 5, 5, 5, 9]);
        // a changed time base starts a new record
        assert!(r.ingest(chunk(9, 19, 1, &[7])));
        assert_eq!(r.capture().unwrap().codes, vec![7]);
        // so does a restart flag and a counter that went back
        assert!(r.ingest(chunk(10, 19, 3, &[8])));
        assert!(r.ingest(chunk(0, 19, 1, &[1])));
    }

    #[test]
    fn roll_record_is_capped_and_keeps_time_zero() {
        let mut r = RollStream::default();
        r.ingest(chunk(0, 18, 3, &[]));
        let block = vec![100u8; 100_000];
        let mut idx = 0u32;
        for _ in 0..21 {
            r.ingest(chunk(idx, 18, 1, &block));
            idx += block.len() as u32;
        }
        let c = r.capture().unwrap();
        assert!(c.len() <= ROLL_MAX_SAMPLES);
        assert!(c.trig < 0.0, "oldest samples were dropped, time 0 moved off the start");
    }

    fn info(interleave: bool, start: u16, trig: u16) -> DeepInfo {
        DeepInfo { samples: 400, spp_capture: 1.0, spp_view: 1.0, timebase: 4, timebase_applied: 4, interleave, start, trig, vdiv: 6, ..Default::default() }
    }

    #[test]
    fn interleave_phase_recovered() {
        // ramp 0..399 split into 4 blocks with phase 2
        let n = 400;
        let bl = n / 4;
        let mut raw = vec![0u8; n];
        for k in 0..n {
            let b = (2 + k) % 4;
            raw[b * bl + k / 4] = (k / 2) as u8;
        }
        let c = DeepCapture::assemble(info(true, 0, 100), &raw);
        assert!(c.codes.windows(2).all(|w| w[1] >= w[0]), "{:?}", &c.codes[..16]);
    }

    #[test]
    fn circular_start_and_trigger() {
        let raw: Vec<u8> = (0..400u32).map(|i| ((i + 400 - 100) % 400 / 2) as u8).collect();
        let c = DeepCapture::assemble(info(false, 100, 150), &raw);
        assert_eq!(c.codes[0], 0);
        assert_eq!(c.trig, 50.0);
        assert!((c.time(50.0)).abs() < 1e-15);
        assert!((c.dt - 1e-6 / 25.0 * TIME_SCALE).abs() < 1e-15);
    }

    #[test]
    fn assembler_needs_contiguous_chunks() {
        let mut a = Assembler::default();
        a.header(info(false, 0, 0));
        assert!(a.chunk(0, &[1; 200]).is_none());
        assert!(a.chunk(200, &[2; 200]).is_some());
        assert!(a.chunk(0, &[1; 200]).is_none()); // no header: ignored
    }
}
