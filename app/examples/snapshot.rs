//! Connects, waits for a frame and saves it as PNG + CSV (same code as the GUI buttons).
use dso510_link::{export, link::{Event, Link}, measure, protocol::Message};
use std::time::{Duration, Instant};

fn main() {
    let link = Link::spawn(|| {});
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(6) {
        if let Ok(Event::Message(Message::Frame(f))) = link.events.recv_timeout(Duration::from_millis(200)) {
            let m = measure::measure(&f);
            println!("png: {:?}", export::save_png(&f, &m, "DSO-510").map(|p| p.display().to_string()));
            println!("csv: {:?}", export::save_csv(&f).map(|p| p.display().to_string()));
            println!("measure: {m:?}");
            return;
        }
    }
    println!("no frame");
}
