//! Battery as the scope reports it (firmware usb_link v4.1): prints a few status messages.
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

fn main() {
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let (mut parser, mut out) = (Parser::default(), Vec::new());
    let mut buf = vec![0u8; 65536];
    let t0 = Instant::now();
    let mut shown = 0;
    while t0.elapsed() < Duration::from_secs(6) && shown < 6 {
        let Ok(n) = port.read(&mut buf) else { continue };
        parser.feed(&buf[..n], &mut out);
        for m in out.drain(..) {
            if let Message::Status(s) = m {
                if !s.has_battery {
                    println!("firmware v{} does not report the battery (flash the v4.1 build)", s.version);
                    return;
                }
                println!(
                    "fw v{}  battery {:3} %  {:4} mV (raw {:4})  flags {:#04b}  charger {}",
                    s.version, s.battery_pct, s.battery_mv, s.battery_raw, s.battery_flags, s.charging()
                );
                shown += 1;
            }
        }
    }
}
