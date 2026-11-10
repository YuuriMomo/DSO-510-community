//! Wire protocol of the DSO-510 "PC mode" (firmware: fw/src/usb_link.c, spec: docs/usb_link.md).
//!
//! Every message: b"DSO", type: u8, len: u16 LE, payload[len], crc: u16 LE
//! crc = CRC-16/CCITT-FALSE (init 0xFFFF, poly 0x1021) over type, len and payload.
//!
//! Device -> PC: 'F' frame (screen trace + scope settings), 'S' status (v2), 'T' text, 'R' raw samples.
//! PC -> device: text lines, e.g. "SET VDIV 6", "GET", "KEY 2", "RAW", "INFO".

pub const TRACE_POINTS: usize = 301;
pub const RAW_SAMPLES: usize = 30100;
/// Screen geometry: the trace window is 301 x 201 px, 12 x 8 divisions of 25 px,
/// vertical centre at pixel value 128 (firmware draws column x at y = 248 - trace[x]).
pub const PX_PER_DIV: f64 = 25.0;
/// The scope's sampling timers run 4.17 % slower than the time-base labels say: the 240 MHz core cannot
/// make 50 MS/s, the real rate is 48 MS/s (24/25) and the whole time axis scales with it. Measured on
/// hardware against the PC clock (roll mode: 480.2 samples/s for a nominal 500) and against the 1 kHz
/// calibrator. The firmware hides it on its own display by computing frequency with /24 instead of /25.
/// Every time on the PC = nominal time × this factor.
pub const TIME_SCALE: f64 = 25.0 / 24.0;
pub const CENTRE_PX: f64 = 128.0;

/// Time per division, index = `timebase` (table at 0x08035378 in the firmware).
pub const TIMEBASE_LABELS: [&str; 27] = [
    "50 ns", "100 ns", "250 ns", "500 ns", "1 µs", "2 µs", "5 µs", "10 µs", "20 µs", "50 µs", "100 µs",
    "200 µs", "500 µs", "1 ms", "2 ms", "5 ms", "10 ms", "20 ms", "50 ms", "100 ms", "200 ms", "500 ms",
    "1 s", "2 s", "5 s", "10 s", "20 s",
];
pub const TIMEBASE_SECONDS: [f64; 27] = [
    50e-9, 100e-9, 250e-9, 500e-9, 1e-6, 2e-6, 5e-6, 10e-6, 20e-6, 50e-6, 100e-6, 200e-6, 500e-6, 1e-3,
    2e-3, 5e-3, 10e-3, 20e-3, 50e-3, 100e-3, 200e-3, 500e-3, 1.0, 2.0, 5.0, 10.0, 20.0,
];

/// Volts per division, index = probe * 3 + vdiv (table at 0x0801029C).
pub const VDIV_LABELS: [&str; 13] = [
    "10 mV", "20 mV", "50 mV", "100 mV", "200 mV", "500 mV", "1 V", "2 V", "5 V", "10 V", "20 V", "50 V", "100 V",
];
pub const VDIV_VOLTS: [f64; 13] = [0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0];

pub const COUPLING_LABELS: [&str; 2] = ["AC", "DC"];
pub const TRIG_MODE_LABELS: [&str; 3] = ["Normal", "Auto", "Single"];
pub const TRIG_EDGE_LABELS: [&str; 2] = ["Rising", "Falling"];
pub const ADJUST_LABELS: [&str; 3] = ["V/div + time", "Offset + position", "Trigger + position"];
/// Generator waveforms, index = `gen_wave` (English column of the table at 0x080351E0).
pub const WAVE_LABELS: [&str; 13] = [
    "Sine", "Square", "Sawtooth", "Half-wave", "Full-wave", "Step", "Reverse step", "Exponential rise",
    "Exponential decay", "DC", "Multi-audio", "Sinc pulse", "Lorentz",
];
pub const AUTO_OFF_LABELS: [&str; 4] = ["Off", "15 min", "30 min", "1 h"];

pub fn screen_name(app_mode: u8) -> &'static str {
    match app_mode {
        0 => "Main menu",
        2 => "Oscilloscope",
        3 => "Signal generator",
        4 => "Settings",
        5 => "Sub-screen 5",
        6 => "Screenshot viewer",
        7 => "USB Sharing",
        8 => "Language",
        9 => "Oscilloscope menu",
        10..=12 => "Saving / gallery",
        _ => "Unknown",
    }
}

/// `trace` pixel value -> volts, for a given V/div and vertical offset (screen pixels).
pub fn px_to_volts(px: f64, volts_per_div: f64, offset: i8) -> f64 {
    (px - CENTRE_PX - offset as f64) / PX_PER_DIV * volts_per_div
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub seq: u16,
    pub app_mode: u8,
    pub timebase: u8,
    pub vdiv: u8,
    pub probe: u8,
    pub coupling: u8,
    pub trig_mode: u8,
    pub trig_edge: u8,
    pub running: bool,
    pub trig_level: i16,
    pub offset: i8,
    pub freq_raw: i32,
    pub trace_start: u16,
    pub trace_end: u16,
    pub trace: Vec<u8>,
}

impl Frame {
    pub fn vdiv_index(&self) -> usize {
        (self.probe as usize * 3 + self.vdiv as usize).min(VDIV_VOLTS.len() - 1)
    }
    pub fn volts_per_div(&self) -> f64 {
        VDIV_VOLTS[self.vdiv_index()]
    }
    pub fn vdiv_label(&self) -> &'static str {
        VDIV_LABELS[self.vdiv_index()]
    }
    /// nominal time per division (the label on the scope)
    pub fn seconds_per_div(&self) -> f64 {
        TIMEBASE_SECONDS[(self.timebase as usize).min(TIMEBASE_SECONDS.len() - 1)]
    }
    /// real time between two trace columns (TIME_SCALE applied)
    pub fn seconds_per_px(&self) -> f64 {
        self.seconds_per_div() / PX_PER_DIV * TIME_SCALE
    }
    pub fn timebase_label(&self) -> &'static str {
        TIMEBASE_LABELS[(self.timebase as usize).min(TIMEBASE_LABELS.len() - 1)]
    }
    /// Is column `i` valid (roll mode fills the trace progressively)?
    pub fn valid(&self, i: usize) -> bool {
        let (a, b) = (self.trace_start as usize, self.trace_end as usize);
        if a < b { i >= a && i < b } else { true }
    }
    /// Screen trace converted to (seconds, volts), invalid columns skipped, offset removed.
    pub fn samples(&self) -> Vec<[f64; 2]> {
        let v_div = self.volts_per_div();
        let dt = self.seconds_per_px();
        self.trace
            .iter()
            .enumerate()
            .filter(|(i, _)| self.valid(*i))
            .map(|(i, &px)| [i as f64 * dt, px_to_volts(px as f64, v_div, self.offset)])
            .collect()
    }
    /// Trigger level in volts.
    pub fn trigger_volts(&self) -> f64 {
        (self.trig_level as f64 - self.offset as f64) / PX_PER_DIV * self.volts_per_div()
    }
}

/// Complete instrument state (firmware usb_link v2). All fields of the wire format are kept.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub version: u8,
    pub app_mode: u8,
    pub menu_sel_a: u8,
    pub adjust_target: u8,
    pub timebase: u8,
    pub vdiv: u8,
    pub probe: u8,
    pub coupling: u8,
    pub trig_mode: u8,
    pub trig_edge: u8,
    pub running: bool,
    pub record_state: u8,
    pub trig_level: i16,
    pub hpos: i16,
    pub offset: i8,
    pub gallery_count: u8,
    pub freq_raw: i32,
    pub gen_freq: u32,
    pub gen_wave: u8,
    pub gen_duty: u8,
    pub gen_ampl: u8,
    pub gen_out: bool,
    pub beep: u8,
    pub light: u8,
    pub auto_off: u8,
    pub language: u8,
    pub measure_mask: u16,
    pub busy: bool,
    pub theme: u8,
    /// firmware usb_link v4.1 reports the battery (status payload of 44 bytes)
    pub has_battery: bool,
    /// level the scope's own icon uses: 100, 90, 80 ... 30, 15, 5
    pub battery_pct: u8,
    /// bit 0: charger / USB power present, bit 1: the firmware's charging flag
    pub battery_flags: u8,
    /// battery voltage in mV (0 = unknown)
    pub battery_mv: u16,
    pub battery_raw: u16,
}

impl Status {
    /// Charger or USB power connected: the battery voltage is raised while it charges.
    pub fn charging(&self) -> bool {
        self.battery_flags & 1 != 0
    }
}

/// Header of one deep-memory capture ('D', firmware v2.1, "DEEP 1"); the samples follow as 'R' chunks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeepInfo {
    pub seq: u16,
    /// build_trace start index (Single mode: circular DMA write position)
    pub start: u16,
    /// trigger index found by the firmware, 0xFFFF = none
    pub trig: u16,
    pub samples: u16,
    /// samples per screen pixel at the captured time base
    pub spp_capture: f32,
    /// samples per pixel of the shown time base (zoom while stopped)
    pub spp_view: f32,
    pub timebase: u8,
    pub timebase_applied: u8,
    /// time base < 4: four phase-shifted DMA blocks of samples/4
    pub interleave: bool,
    pub trig_mode: u8,
    pub trig_edge: u8,
    pub vdiv: u8,
    pub probe: u8,
    pub coupling: u8,
    pub trig_level: i16,
    pub hpos: i16,
    pub offset: i8,
    pub running: bool,
    pub trace_start: u16,
    pub trace_end: u16,
}

/// Roll mode (50 ms/div and slower): new screen columns streamed by firmware usb_link v3.1 ('P').
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RollChunk {
    /// stream counter of the first sample
    pub index: u32,
    pub timebase: u8,
    pub vdiv: u8,
    pub probe: u8,
    pub coupling: u8,
    pub offset: i8,
    /// bit 0: running, bit 1: a new record starts here
    pub flags: u8,
    pub samples: Vec<u8>,
}

#[derive(Debug)]
pub enum Message {
    Frame(Frame),
    Status(Status),
    Text(String),
    Deep(DeepInfo),
    Roll(RollChunk),
    Raw { offset: u32, data: Vec<u8> },
    Unknown(u8),
}

pub fn crc16(data: &[u8], mut crc: u16) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// Incremental stream parser: feed bytes, get complete, CRC-checked messages.
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    pub crc_errors: u64,
}

impl Parser {
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<Message>) {
        self.buf.extend_from_slice(data);
        loop {
            let Some(start) = self.buf.windows(3).position(|w| w == b"DSO") else {
                let keep = self.buf.len().min(2);
                self.buf.drain(..self.buf.len() - keep);
                return;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            if self.buf.len() < 6 {
                return;
            }
            let len = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
            if len > 4096 {
                self.buf.drain(..3);
                continue;
            }
            if self.buf.len() < 6 + len + 2 {
                return;
            }
            let crc = crc16(&self.buf[3..6 + len], 0xFFFF);
            let got = u16::from_le_bytes([self.buf[6 + len], self.buf[7 + len]]);
            if crc != got {
                self.crc_errors += 1;
                self.buf.drain(..3);
                continue;
            }
            let ty = self.buf[3];
            let payload = self.buf[6..6 + len].to_vec();
            self.buf.drain(..6 + len + 2);
            out.push(decode(ty, &payload));
        }
    }
}

fn decode(ty: u8, p: &[u8]) -> Message {
    let u16at = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
    let i16at = |o: usize| i16::from_le_bytes([p[o], p[o + 1]]);
    let u32at = |o: usize| u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
    match ty {
        b'F' if p.len() >= 24 => {
            let points = u16at(22) as usize;
            Message::Frame(Frame {
                seq: u16at(0),
                app_mode: p[2],
                timebase: p[3],
                vdiv: p[4],
                probe: p[5],
                coupling: p[6],
                trig_mode: p[7],
                trig_edge: p[8],
                running: p[9] != 0,
                trig_level: i16at(10),
                offset: p[12] as i8,
                freq_raw: u32at(14) as i32,
                trace_start: u16at(18),
                trace_end: u16at(20),
                trace: p[24..(24 + points).min(p.len())].to_vec(),
            })
        }
        b'S' if p.len() >= 38 => Message::Status(Status {
            version: p[0],
            app_mode: p[1],
            menu_sel_a: p[2],
            adjust_target: p[3],
            timebase: p[4],
            vdiv: p[5],
            probe: p[6],
            coupling: p[7],
            trig_mode: p[8],
            trig_edge: p[9],
            running: p[10] != 0,
            record_state: p[11],
            trig_level: i16at(12),
            hpos: i16at(14),
            offset: p[16] as i8,
            gallery_count: p[17],
            freq_raw: u32at(18) as i32,
            gen_freq: u32at(22),
            gen_wave: p[26],
            gen_duty: p[27],
            gen_ampl: p[28],
            gen_out: p[29] != 0,
            beep: p[30],
            light: p[31],
            auto_off: p[32],
            language: p[33],
            measure_mask: u16at(34),
            busy: p[36] != 0,
            theme: p[37],
            has_battery: p.len() >= 44,
            battery_pct: p.get(38).copied().unwrap_or(0),
            battery_flags: p.get(39).copied().unwrap_or(0),
            battery_mv: if p.len() >= 44 { u16at(40) } else { 0 },
            battery_raw: if p.len() >= 44 { u16at(42) } else { 0 },
        }),
        b'D' if p.len() >= 34 => {
        let f32at = |o: usize| f32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
        Message::Deep(DeepInfo {
            seq: u16at(0),
            start: u16at(2),
            trig: u16at(4),
            samples: u16at(6),
            spp_capture: f32at(8),
            spp_view: f32at(12),
            timebase: p[16],
            timebase_applied: p[17],
            interleave: p[18] != 0,
            trig_mode: p[19],
            trig_edge: p[20],
            vdiv: p[21],
            probe: p[22],
            coupling: p[23],
            trig_level: i16at(24),
            hpos: i16at(26),
            offset: p[28] as i8,
            running: p[29] != 0,
            trace_start: u16at(30),
            trace_end: u16at(32),
        })
        }
        b'P' if p.len() >= 12 => {
            let n = u16at(10) as usize;
            Message::Roll(RollChunk {
                index: u32at(0),
                timebase: p[4],
                vdiv: p[5],
                probe: p[6],
                coupling: p[7],
                offset: p[8] as i8,
                flags: p[9],
                samples: p[12..(12 + n).min(p.len())].to_vec(),
            })
        }
        b'T' => Message::Text(String::from_utf8_lossy(p).into_owned()),
        b'R' if p.len() >= 4 => Message::Raw { offset: u32at(0), data: p[4..].to_vec() },
        other => Message::Unknown(other),
    }
}

/// A "simple screenshot" file saved by the scope (2:/Screenshot simple file/N.bin) is the bare
/// 301-byte screen trace without settings.
pub fn frame_from_screenshot_file(data: &[u8]) -> Option<Frame> {
    if data.len() < TRACE_POINTS {
        return None;
    }
    Some(Frame { trace: data[..TRACE_POINTS].to_vec(), timebase: 13, vdiv: 6, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn encode(ty: u8, payload: &[u8]) -> Vec<u8> {
        let mut m = b"DSO".to_vec();
        m.push(ty);
        m.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        m.extend_from_slice(payload);
        let crc = crc16(&m[3..], 0xFFFF);
        m.extend_from_slice(&crc.to_le_bytes());
        m
    }

    #[test]
    fn crc_known_value() {
        assert_eq!(crc16(b"123456789", 0xFFFF), 0x29B1); // CRC-16/CCITT-FALSE check value
    }

    #[test]
    fn parses_frame_and_status_split_across_reads_with_garbage() {
        let mut p = vec![0u8; 24];
        p[3] = 13;
        p[4] = 6;
        p[9] = 1;
        p[22..24].copy_from_slice(&(TRACE_POINTS as u16).to_le_bytes());
        p.extend((0..TRACE_POINTS).map(|i| (i % 256) as u8));
        let mut s = vec![0u8; 44];
        s[0] = 4;
        s[1] = 3;
        s[38] = 80;
        s[39] = 1;
        s[40..42].copy_from_slice(&3912u16.to_le_bytes());
        s[42..44].copy_from_slice(&2428u16.to_le_bytes());
        s[22..26].copy_from_slice(&12345u32.to_le_bytes());
        s[26] = 1;
        let mut stream = b"xxDS".to_vec();
        stream.extend(encode(b'F', &p));
        stream.extend(encode(b'S', &s));
        stream.extend(encode(b'T', b"PONG"));
        let mut parser = Parser::default();
        let mut out = Vec::new();
        for chunk in stream.chunks(7) {
            parser.feed(chunk, &mut out);
        }
        assert_eq!(out.len(), 3);
        match &out[0] {
            Message::Frame(f) => {
                assert_eq!(f.trace.len(), TRACE_POINTS);
                assert_eq!(f.timebase_label(), "1 ms");
                assert_eq!(f.vdiv_label(), "1 V");
                assert!(f.running);
            }
            m => panic!("{m:?}"),
        }
        match &out[1] {
            Message::Status(st) => {
                assert_eq!(st.version, 4);
                assert!(st.has_battery && st.charging());
                assert_eq!((st.battery_pct, st.battery_mv, st.battery_raw), (80, 3912, 2428));
                assert_eq!(st.app_mode, 3);
                assert_eq!(st.gen_freq, 12345);
                assert_eq!(WAVE_LABELS[st.gen_wave as usize], "Square");
            }
            m => panic!("{m:?}"),
        }
        assert!(matches!(&out[2], Message::Text(t) if t == "PONG"));
        assert_eq!(parser.crc_errors, 0);
    }

    #[test]
    fn old_status_without_battery_is_still_accepted() {
        let mut s = vec![0u8; 38];
        s[0] = 3;
        let mut out = Vec::new();
        Parser::default().feed(&encode(b'S', &s), &mut out);
        match &out[..] {
            [Message::Status(st)] => assert!(!st.has_battery && st.battery_pct == 0 && st.battery_mv == 0),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn rejects_bad_crc() {
        let mut m = encode(b'T', b"hello");
        let n = m.len();
        m[n - 1] ^= 0xFF;
        let mut parser = Parser::default();
        let mut out = Vec::new();
        parser.feed(&m, &mut out);
        assert!(out.is_empty());
        assert_eq!(parser.crc_errors, 1);
    }

    #[test]
    fn volts_conversion() {
        // centre pixel = 0 V; one division up = 1 V/div
        assert_eq!(px_to_volts(128.0, 1.0, 0), 0.0);
        assert_eq!(px_to_volts(153.0, 1.0, 0), 1.0);
        // offset moves the trace on screen but not the voltage
        assert_eq!(px_to_volts(153.0, 1.0, 25), 0.0);
    }
}
