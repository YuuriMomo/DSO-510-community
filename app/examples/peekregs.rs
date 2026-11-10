//! Read a few registers / words: peekregs <hex addr> [<hex addr> ...] (4 bytes each)
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};
fn main() {
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    let (mut parser, mut out, mut buf) = (Parser::default(), Vec::new(), vec![0u8; 65536]);
    std::thread::sleep(Duration::from_millis(300));
    for a in std::env::args().skip(1) {
        port.write_all(format!("PEEK {a} 4\n").as_bytes()).unwrap();
        let t0 = Instant::now();
        let mut got = None;
        while t0.elapsed() < Duration::from_millis(500) && got.is_none() {
            let Ok(n) = port.read(&mut buf) else { continue };
            parser.feed(&buf[..n], &mut out);
            for m in out.drain(..) { if let Message::Text(t) = m { if t.starts_with("PEEK") { got = Some(t) } } }
        }
        println!("{a}: {}", got.unwrap_or("no reply".into()));
    }
}
