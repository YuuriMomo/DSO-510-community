//! Start-up picture of the scope (firmware v4.20 "LOGO" commands): any image made into the 320 x 240
//! baseline JPEG the scope's decoder reads, and the upload in acknowledged chunks.
//!
//! Protocol: "LOGO BEGIN <bytes>" -> "LOGO READY"; "LOGOD <hex offset> <hex bytes>" -> "LOGOD <bytes so far>"
//! (at most two lines in flight: the scope's receive buffer holds 512 bytes and has no flow control);
//! "LOGO SAVE <hex crc32>" -> "LOGO WRITING" ... "LOGO DONE" (the file is written by the display task).
//! "LOGO DELETE" -> "LOGO DELETING" ... "LOGO DONE": the factory animation again.

use image::{imageops::FilterType, DynamicImage, GenericImageView, RgbImage};
use std::time::{Duration, Instant};

pub const W: u32 = 320;
pub const H: u32 = 240;
/// the scope's upload buffer (boot_logo.c LOGO_MAX)
pub const MAX_BYTES: usize = 61440;
/// bytes per LOGOD line (192 hex characters: fits the scope's 208-character command line)
const CHUNK: usize = 96;
const IN_FLIGHT: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    /// fill the screen, cut off what sticks out
    Crop,
    /// whole picture, black bars
    Bars,
}

/// The picture as the scope will show it, and its JPEG file.
pub struct Prepared {
    pub rgb: RgbImage,
    pub jpeg: Vec<u8>,
    pub quality: u8,
}

pub fn prepare(img: &DynamicImage, fit: Fit) -> Result<Prepared, String> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let rgb = match fit {
        Fit::Crop => img.resize_to_fill(W, H, FilterType::Lanczos3).to_rgb8(),
        Fit::Bars => {
            let small = img.resize(W, H, FilterType::Lanczos3).to_rgb8();
            let mut out = RgbImage::new(W, H);
            let (x, y) = ((W - small.width()) / 2, (H - small.height()) / 2);
            image::imageops::overlay(&mut out, &small, x as i64, y as i64);
            out
        }
    };
    // baseline JPEG (the scope's TJpgDec cannot read progressive ones); lower quality if too large
    for quality in [92u8, 85, 75, 60, 45] {
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode_image(&rgb)
            .map_err(|e| e.to_string())?;
        if jpeg.len() <= MAX_BYTES {
            return Ok(Prepared { rgb, jpeg, quality });
        }
    }
    Err("picture too large even at low quality".into())
}

pub fn load(path: &std::path::Path) -> Result<DynamicImage, String> {
    image::open(path).map_err(|e| e.to_string())
}

/// image files the start-up picture accepts (by extension)
pub fn is_image(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).is_some_and(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "gif"))
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}

#[derive(Clone, PartialEq, Debug)]
pub enum State {
    Begin,
    Sending,
    Writing,
    Done,
    Failed(String),
}

/// Upload (or delete) driven by the scope's replies; `step` returns the command lines to send now.
pub struct Upload {
    data: Vec<u8>,
    pub state: State,
    /// bytes the scope confirmed
    pub acked: usize,
    /// next byte to send
    sent: usize,
    last: Instant,
    started: bool,
    delete: bool,
    format: bool,
    retries: u32,
}

impl Upload {
    pub fn new(jpeg: Vec<u8>) -> Upload {
        Upload { data: jpeg, state: State::Begin, acked: 0, sent: 0, last: Instant::now(), started: false, delete: false, format: false, retries: 0 }
    }
    pub fn delete() -> Upload {
        Upload { delete: true, ..Upload::new(Vec::new()) }
    }
    /// quick format of the scope's drive 2: (logo, screenshots): the repair when it is full or broken
    pub fn format() -> Upload {
        Upload { format: true, ..Upload::new(Vec::new()) }
    }
    pub fn is_delete(&self) -> bool {
        self.delete
    }
    pub fn is_format(&self) -> bool {
        self.format
    }
    pub fn total(&self) -> usize {
        self.data.len()
    }
    pub fn finished(&self) -> bool {
        matches!(self.state, State::Done | State::Failed(_))
    }
    /// 0..1
    pub fn progress(&self) -> f32 {
        match self.state {
            State::Begin => 0.0,
            State::Sending => self.acked as f32 / self.data.len().max(1) as f32 * 0.95,
            State::Writing => 0.97,
            State::Done | State::Failed(_) => 1.0,
        }
    }

    /// a text message from the scope
    pub fn reply(&mut self, t: &str) {
        if self.finished() {
            return;
        }
        let t = t.trim();
        if t.starts_with("ERR LOGOD") {
            // a line after a lost one (the scope takes them only in order): send again from what it has
            self.sent = self.acked;
        } else if t.starts_with("ERR LOGO") {
            self.state = State::Failed(t.to_string());
        } else if t == "ERR unknown command" && self.state == State::Begin {
            self.state = State::Failed("firmware v4.20 or newer needed".into());
        } else if t == "LOGO READY" && self.state == State::Begin {
            self.state = State::Sending;
            self.last = Instant::now();
        } else if let Some(n) = t.strip_prefix("LOGOD ").and_then(|n| n.trim().parse::<usize>().ok()) {
            if n > self.acked {
                self.acked = n.min(self.data.len());
                self.last = Instant::now();
                self.retries = 0;
            }
        } else if t == "LOGO WRITING" || t == "LOGO DELETING" || t == "LOGO FORMATTING" {
            self.state = State::Writing;
            self.last = Instant::now();
        } else if t == "LOGO DONE" && self.state == State::Writing {
            self.state = State::Done;
        }
    }

    pub fn step(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        match self.state {
            State::Begin if !self.started => {
                self.started = true;
                self.last = Instant::now();
                out.push(if self.format {
                    "LOGO FORMAT 2".to_string()
                } else if self.delete {
                    "LOGO DELETE".to_string()
                } else {
                    format!("LOGO BEGIN {}", self.data.len())
                });
            }
            State::Sending => {
                if self.last.elapsed() > TIMEOUT {
                    // a line or its reply got lost: send again from what the scope has
                    self.retries += 1;
                    if self.retries > 5 {
                        self.state = State::Failed("no answer from the scope".into());
                        return out;
                    }
                    self.sent = self.acked;
                    self.last = Instant::now();
                }
                if self.acked == self.data.len() {
                    out.push(format!("LOGO SAVE {:08X}", crc32(&self.data)));
                    self.state = State::Writing;
                    self.last = Instant::now();
                    return out;
                }
                while self.sent < self.data.len() && self.sent < self.acked + IN_FLIGHT * CHUNK {
                    let end = (self.sent + CHUNK).min(self.data.len());
                    let hex: String = self.data[self.sent..end].iter().map(|b| format!("{b:02X}")).collect();
                    out.push(format!("LOGOD {:X} {hex}", self.sent));
                    self.sent = end;
                }
            }
            State::Begin | State::Writing if self.last.elapsed() > Duration::from_secs(10) => {
                self.state = State::Failed("no answer from the scope".into());
            }
            _ => {}
        }
        out
    }
}

/// the scope could not write the file: its drive 2: is full (253) or its folders are broken (5)
pub fn needs_repair(error: &str) -> bool {
    error.contains("ERR LOGO fs 253") || error.contains("ERR LOGO fs 5")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn prepared_picture_is_a_baseline_jpeg_of_the_screen_size() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(1000, 500, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])));
        for fit in [Fit::Crop, Fit::Bars] {
            let p = prepare(&img, fit).unwrap();
            assert_eq!(p.rgb.dimensions(), (W, H));
            assert!(p.jpeg.len() <= MAX_BYTES);
            // SOF0 marker (baseline), no SOF2 (progressive)
            assert!(p.jpeg.windows(2).any(|w| w == [0xFF, 0xC0]), "not baseline");
            assert!(!p.jpeg.windows(2).any(|w| w == [0xFF, 0xC2]), "progressive");
            let back = image::load_from_memory(&p.jpeg).unwrap();
            assert_eq!(back.dimensions(), (W, H));
        }
    }

    /// the scope side as in boot_logo.c / usb_link.c, with a lost line in between
    #[test]
    fn upload_against_a_model_of_the_scope() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let mut up = Upload::new(data.clone());
        let mut scope: Vec<u8> = Vec::new();
        let (mut size, mut got, mut saved) = (0usize, 0usize, false);
        let mut lost_one = false;
        for _ in 0..2000 {
            for cmd in up.step() {
                let reply = if let Some(n) = cmd.strip_prefix("LOGO BEGIN ") {
                    size = n.parse().unwrap();
                    scope = vec![0; size];
                    "LOGO READY".to_string()
                } else if let Some(rest) = cmd.strip_prefix("LOGOD ") {
                    let (off, hex) = rest.split_once(' ').unwrap();
                    let off = usize::from_str_radix(off, 16).unwrap();
                    if off == 480 && !lost_one {
                        lost_one = true;                       // this line never arrives
                        continue;
                    }
                    if off > got {
                        "ERR LOGOD".to_string()
                    } else {
                        let bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect();
                        scope[off..off + bytes.len()].copy_from_slice(&bytes);
                        got = got.max(off + bytes.len());
                        format!("LOGOD {got}")
                    }
                } else if let Some(c) = cmd.strip_prefix("LOGO SAVE ") {
                    assert_eq!(u32::from_str_radix(c, 16).unwrap(), crc32(&scope[..size]));
                    saved = true;
                    "LOGO WRITING".to_string()
                } else {
                    panic!("unexpected {cmd}");
                };
                up.reply(&reply);
            }
            if saved && up.state == State::Writing {
                up.reply("LOGO DONE");
            }
            if up.finished() {
                break;
            }
            // time passes: let the timeout of the lost line run out
            up.last -= Duration::from_millis(300);
        }
        assert_eq!(up.state, State::Done);
        assert_eq!(scope, data);
    }
}
