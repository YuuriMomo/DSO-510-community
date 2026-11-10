//! Snapshot of the scope's acquisition state for the emulator (tools/emu.py): with the scope stopped,
//! saves the raw sample buffer, RAM 0x20000000..0x20000400 (all osc_* variables) and the drawn trace.
//!   memsnap <out_dir> [commands before stopping...]
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

struct Io { port: Conn, parser: Parser, out: Vec<Message>, buf: Vec<u8> }
impl Io {
    fn cmd(&mut self, c: &str, wait_ms: u64) -> (Vec<String>, Vec<(u32, Vec<u8>)>) {
        self.port.write_all(format!("{c}\n").as_bytes()).unwrap();
        let t0 = Instant::now();
        let (mut texts, mut raws) = (Vec::new(), Vec::new());
        while t0.elapsed() < Duration::from_millis(wait_ms) {
            let Ok(n) = self.port.read(&mut self.buf) else { continue };
            self.parser.feed(&self.buf[..n], &mut self.out);
            for m in self.out.drain(..) {
                match m {
                    Message::Text(t) => texts.push(t),
                    Message::Raw { offset, data } => raws.push((offset, data)),
                    _ => {}
                }
            }
        }
        (texts, raws)
    }
    fn peek(&mut self, addr: u32, n: u32) -> Vec<u8> {
        let (texts, _) = self.cmd(&format!("PEEK {addr:08X} {n}"), 250);
        let t = texts.iter().find(|t| t.starts_with("PEEK")).unwrap_or_else(|| panic!("no PEEK reply for {addr:#x}"));
        t.split(':').nth(1).unwrap().split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(args.get(1).expect("out dir"));
    std::fs::create_dir_all(&dir).unwrap();
    let p = device::find_dso().expect("no DSO-510");
    let mut io = Io { port: Conn::open(&p).unwrap(), parser: Parser::default(), out: Vec::new(), buf: vec![0u8; 65536] };
    std::thread::sleep(Duration::from_millis(300));
    io.cmd("DEEP 0", 300);
    for c in args.iter().skip(2) { io.cmd(c, 400); }
    io.cmd("SET RUN 0", 1500);
    let mut ram = Vec::new();
    for a in (0x2000_0000u32..0x2000_0400).step_by(64) { ram.extend(io.peek(a, 64)); }
    let trace_ptr = u32::from_le_bytes(ram[0x27C..0x280].try_into().unwrap());
    let mut trace = Vec::new();
    for a in (trace_ptr..trace_ptr + 301).step_by(64) { trace.extend(io.peek(a, 64.min(trace_ptr + 301 - a))); }
    let (_, raws) = io.cmd("RAW", 2500);
    let mut raw = vec![0u8; 30100];
    let mut got = 0;
    for (off, d) in raws { let o = off as usize; if o + d.len() <= raw.len() { raw[o..o + d.len()].copy_from_slice(&d); got += d.len(); } }
    std::fs::write(dir.join("ram.bin"), &ram).unwrap();
    std::fs::write(dir.join("trace.bin"), &trace).unwrap();
    std::fs::write(dir.join("raw.bin"), &raw).unwrap();
    println!("saved: ram {} B, trace {} B (osc_trace @ {trace_ptr:#x}), raw {got} of 30100 B; tb {} applied {} tmode {} edge {} hpos {} mode_27a {} slow_reload {:#06x}",
        ram.len(), trace.len(), ram[0xF4], ram[0x292], ram[0xEC], ram[0xED], i16::from_le_bytes([ram[0xEE], ram[0xEF]]), ram[0x27A],
        u16::from_le_bytes([ram[0x28E], ram[0x28F]]));
    io.cmd("SET RUN 1", 300);
}
