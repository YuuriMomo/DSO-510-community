//! Serial link thread: (re)connects to the scope, reads messages, writes command lines.
//!
//! The thread owns the port. With `auto` set it keeps looking for a DSO-510 and reconnects by itself
//! after the cable is unplugged or the scope restarts.

use crate::device::{self, PortChoice};
use crate::protocol::{Message, Parser};
use crate::transport::Conn;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

pub enum Event {
    Connected(PortChoice),
    Message(Message),
    WriteError(String),
    Disconnected(String),
}

enum Control {
    Send(String),
    /// Connect to this port (None = automatic DSO-510 discovery).
    Connect(Option<PortChoice>),
    Disconnect,
}

pub struct Link {
    ctl: Sender<Control>,
    pub events: Receiver<Event>,
}

impl Link {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Link {
        let (ctl, ctl_rx) = mpsc::channel::<Control>();
        let (ev_tx, events) = mpsc::channel::<Event>();
        thread::spawn(move || worker(ctl_rx, ev_tx, repaint));
        Link { ctl, events }
    }
    /// A link without a worker thread (tests): commands go nowhere, no events arrive.
    pub fn detached() -> Link {
        let (ctl, _ctl_rx) = mpsc::channel::<Control>();
        let (_ev_tx, events) = mpsc::channel::<Event>();
        Link { ctl, events }
    }
    pub fn send(&self, cmd: impl Into<String>) {
        let _ = self.ctl.send(Control::Send(cmd.into()));
    }
    pub fn connect(&self, port: Option<PortChoice>) {
        let _ = self.ctl.send(Control::Connect(port));
    }
    pub fn disconnect(&self) {
        let _ = self.ctl.send(Control::Disconnect);
    }
}


fn worker(ctl: Receiver<Control>, ev: Sender<Event>, repaint: impl Fn()) {
    // None = idle, Some(None) = automatic, Some(Some(p)) = fixed port
    let mut wanted: Option<Option<PortChoice>> = Some(None);
    let mut port: Option<Conn> = None;
    let mut parser = Parser::default();
    let mut buf = vec![0u8; 65536];
    let mut msgs = Vec::new();
    let mut last_try = Instant::now() - Duration::from_secs(10);
    loop {
        // control messages
        loop {
            match ctl.try_recv() {
                Ok(Control::Send(cmd)) => {
                    if let Some(p) = port.as_mut() {
                        if let Err(e) = p.write_all(format!("{cmd}\n").as_bytes()) {
                            let _ = ev.send(Event::WriteError(format!("{cmd}: {e}")));
                        }
                    }
                }
                Ok(Control::Connect(p)) => {
                    port = None;
                    wanted = Some(p);
                    last_try = Instant::now() - Duration::from_secs(10);
                }
                Ok(Control::Disconnect) => {
                    if port.take().is_some() {
                        let _ = ev.send(Event::Disconnected("disconnected by user".into()));
                    }
                    wanted = None;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // (re)connect
        if port.is_none() {
            if let Some(target) = &wanted {
                if last_try.elapsed() >= Duration::from_millis(800) {
                    last_try = Instant::now();
                    let choice = match target {
                        Some(p) => Some(p.clone()),
                        None => device::find_dso(),
                    };
                    if let Some(c) = choice {
                        match Conn::open(&c) {
                            Ok(p) => {
                                port = Some(p);
                                parser = Parser::default();
                                let _ = ev.send(Event::Connected(c));
                                if let Some(p) = port.as_mut() {
                                    let _ = p.write_all(b"INFO\nGET\n");
                                }
                                repaint();
                            }
                            Err(e) => {
                                if target.is_some() {
                                    let _ = ev.send(Event::Disconnected(e));
                                    wanted = None;
                                }
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(30));
            continue;
        }

        // read
        let p = port.as_mut().unwrap();
        match p.read(&mut buf) {
            Ok(n) if n > 0 => {
                parser.feed(&buf[..n], &mut msgs);
                let any = !msgs.is_empty();
                for m in msgs.drain(..) {
                    if ev.send(Event::Message(m)).is_err() {
                        return;
                    }
                }
                if any {
                    repaint();
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                port = None;
                let _ = ev.send(Event::Disconnected(e.to_string()));
                repaint();
            }
        }
    }
}
