//! RAW dump test at stream level: counts messages per type, CRC errors and received raw offsets.
use dso510_link::{device, protocol::{Message, Parser}};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn main() {
    let p = device::find_dso().expect("no DSO-510");
    let mut port = serialport::new(&p.path, 115_200).timeout(Duration::from_millis(20)).open().unwrap();
    port.write_data_terminal_ready(true).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    port.write_all(b"SET RUN 0\nRAW\n").unwrap();
    let mut parser = Parser::default();
    let mut buf = [0u8; 16384];
    let (mut bytes, mut frames, mut status, mut raw) = (0usize, 0, 0, 0usize);
    let mut offs = Vec::new();
    let t0 = Instant::now();
    let mut out = Vec::new();
    while t0.elapsed() < Duration::from_secs(4) {
        if let Ok(n) = port.read(&mut buf) {
            bytes += n;
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) {
                match m {
                    Message::Frame(_) => frames += 1,
                    Message::Status(_) => status += 1,
                    Message::Raw { offset, data } => { raw += data.len(); offs.push(offset); }
                    _ => {}
                }
            }
        }
    }
    port.write_all(b"SET RUN 1\n").unwrap();
    println!("{bytes} bytes in 4 s ({:.1} kB/s), frames {frames}, status {status}, raw {raw} B in {} chunks, crc errors {}", bytes as f64 / 4096.0, offs.len(), parser.crc_errors);
    println!("offsets: {offs:?}");
}
