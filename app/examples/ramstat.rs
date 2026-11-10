//! RAM usage of the running firmware: FreeRTOS heap_4 (8 KB) and the ALIENTEK mymalloc pool (184 KB).
//!   ramstat [seconds]   samples the pool map repeatedly and reports the peak (switch screens meanwhile)
//!   ramstat scan        switches screens / time bases / modes itself and reports the peak of each step
use dso510_link::{device, protocol::{Message, Parser}, transport::Conn};
use std::time::{Duration, Instant};

struct Io { port: Conn, parser: Parser, out: Vec<Message>, buf: Vec<u8> }
impl Io {
    fn peek(&mut self, addr: u32, n: u32) -> Vec<u8> {
        self.port.write_all(format!("PEEK {addr:08X} {n}\n").as_bytes()).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(500) {
            let Ok(k) = self.port.read(&mut self.buf) else { continue };
            self.parser.feed(&self.buf[..k], &mut self.out);
            for m in self.out.drain(..) {
                if let Message::Text(t) = m {
                    if t.starts_with("PEEK") {
                        return t.split(':').nth(1).unwrap().split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
                    }
                }
            }
        }
        panic!("no PEEK reply for {addr:#x}");
    }
    fn cmd(&mut self, c: &str) {
        self.port.write_all(format!("{c}
").as_bytes()).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(150) {
            let Ok(k) = self.port.read(&mut self.buf) else { continue };
            self.parser.feed(&self.buf[..k], &mut self.out);
            self.out.clear();
        }
    }
    /// used blocks of the pool right now
    fn pool(&mut self) -> Vec<bool> {
        let mut map = Vec::new();
        for a in (MAP..MAP + 2 * BLOCKS).step_by(64) { map.extend(self.peek(a, 64.min(MAP + 2 * BLOCKS - a))); }
        (0..BLOCKS as usize).map(|i| map[2 * i] != 0 || map[2 * i + 1] != 0).collect()
    }
    fn u32(&mut self, a: u32) -> u32 { let b = self.peek(a, 4); u32::from_le_bytes(b[..4].try_into().unwrap()) }
}

const POOL: u32 = 0x2000_74C0;
const MAP: u32 = 0x2003_44C0;
const BLOCKS: u32 = 5760;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    let secs: u64 = arg.parse().unwrap_or(0);
    let p = device::find_dso().expect("no DSO-510");
    let mut io = Io { port: Conn::open(&p).unwrap(), parser: Parser::default(), out: Vec::new(), buf: vec![0u8; 65536] };
    std::thread::sleep(Duration::from_millis(300));
    if arg == "scan" { scan(&mut io); return; }
    let (free, min_free) = (io.u32(0x2000_7444), io.u32(0x2000_7440));
    println!("FreeRTOS heap: 8184 B, free now {free} B, lowest ever {min_free} B -> peak use {} B", 8184 - min_free);
    let t0 = Instant::now();
    let mut peak = vec![false; BLOCKS as usize];
    let mut rounds = 0;
    loop {
        let mut map = Vec::new();
        for a in (MAP..MAP + 2 * BLOCKS).step_by(64) { map.extend(io.peek(a, 64.min(MAP + 2 * BLOCKS - a))); }
        let used: Vec<bool> = (0..BLOCKS as usize).map(|i| map[2 * i] != 0 || map[2 * i + 1] != 0).collect();
        let n = used.iter().filter(|u| **u).count();
        for (p, u) in peak.iter_mut().zip(&used) { *p |= *u; }
        // allocations from the top: lowest used block = how deep the pool is used
        let lowest = used.iter().position(|u| *u).map(|i| i as u32).unwrap_or(BLOCKS);
        println!("pool: {} of {} KB used now, lowest used address {:#x} ({} KB from the top)",
            n * 32 / 1024, BLOCKS * 32 / 1024, POOL + lowest * 32, (BLOCKS - lowest) * 32 / 1024);
        rounds += 1;
        if t0.elapsed() >= Duration::from_secs(secs) { break; }
    }
    let lowest = peak.iter().position(|u| *u).map(|i| i as u32).unwrap_or(BLOCKS);
    println!("over {rounds} samples: blocks ever used {} KB, deepest {} KB from the top -> never touched: {} KB at {:#x}..{:#x}",
        peak.iter().filter(|u| **u).count() * 32 / 1024, (BLOCKS - lowest) * 32 / 1024, lowest * 32 / 1024, POOL, POOL + lowest * 32);
}

/// every reachable state for a while, peak of the pool per state
fn scan(io: &mut Io) {
    let mut steps: Vec<(String, Vec<String>)> = vec![
        ("main menu".into(), vec!["SET SCREEN 0".into()]),
        ("generator".into(), vec!["SET SCREEN 3".into(), "SET GOUT 1".into()]),
        ("settings".into(), vec!["SET SCREEN 4".into()]),
        ("scope".into(), vec!["SET SCREEN 2".into(), "SET RUN 1".into(), "SET TMODE 1".into()]),
    ];
    for tb in 0..=26 { steps.push((format!("scope tb {tb}"), vec![format!("SET TB {tb}")])); }
    for (name, c) in [("tb 2 Normal", "SET TMODE 0"), ("tb 2 Single", "SET TMODE 2"), ("tb 2 Auto", "SET TMODE 1")] {
        steps.push((name.into(), vec!["SET TB 2".into(), c.into()]));
    }
    steps.push(("tb 8 stopped".into(), vec!["SET TB 8".into(), "SET RUN 0".into()]));
    steps.push(("tb 8 running".into(), vec!["SET RUN 1".into()]));
    steps.push(("tb 8 deep memory".into(), vec!["DEEP 1".into()]));
    steps.push(("tb 20 deep memory (roll)".into(), vec!["SET TB 20".into()]));
    steps.push(("back: tb 4, deep off".into(), vec!["DEEP 0".into(), "SET TB 4".into()]));
    let mut ever = vec![false; BLOCKS as usize];
    let mut heap_min = u32::MAX;
    println!("{:<28} {:>9} {:>9}", "state", "used KB", "depth KB");
    for (name, cmds) in steps {
        for c in &cmds { io.cmd(c); }
        std::thread::sleep(Duration::from_millis(700));
        let (mut used_max, mut lowest) = (0usize, BLOCKS);
        for _ in 0..3 {
            let u = io.pool();
            used_max = used_max.max(u.iter().filter(|x| **x).count());
            lowest = lowest.min(u.iter().position(|x| *x).map(|i| i as u32).unwrap_or(BLOCKS));
            for (e, x) in ever.iter_mut().zip(&u) { *e |= *x; }
        }
        heap_min = heap_min.min(io.u32(0x2000_7440));
        println!("{:<28} {:>9} {:>9}", name, used_max * 32 / 1024, (BLOCKS - lowest) * 32 / 1024);
    }
    let lowest = ever.iter().position(|u| *u).map(|i| i as u32).unwrap_or(BLOCKS);
    println!("pool: deepest use {} KB of {} KB -> {} KB never touched; FreeRTOS heap lowest free {} B of 8184",
        (BLOCKS - lowest) * 32 / 1024, BLOCKS * 32 / 1024, lowest * 32 / 1024, heap_min);
}
