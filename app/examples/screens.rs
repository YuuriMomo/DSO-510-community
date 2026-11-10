//! Send a few commands with pauses: screens "SET SCREEN 0" "SET SCREEN 2" ... (full screen redraw)
use dso510_link::{device, transport::Conn};
use std::time::Duration;
fn main() {
    let p = device::find_dso().expect("no DSO-510");
    let mut port = Conn::open(&p).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    for c in std::env::args().skip(1) {
        port.write_all(format!("{c}\n").as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        println!("sent: {c}");
    }
}
