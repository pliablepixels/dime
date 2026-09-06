//! Live process view: memory, CPU, disk and network per process, plus open files on demand.
use serde::Serialize;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{ProcessesToUpdate, System, Users};

/// Per-pid cumulative network bytes from `nettop`, sampled in the background.
#[derive(Default)]
pub struct Net {
    prev: HashMap<u32, (u64, u64)>,
    cur: HashMap<u32, (u64, u64)>,
    elapsed: f64,
}

pub struct Hog {
    sys: Mutex<(System, Instant)>,
    users: Users,
    net: Arc<Mutex<Net>>,
    /// Last IOKit sample of accumulated GPU time per pid, to turn into a rate.
    gpu_prev: Mutex<(Instant, HashMap<u32, u64>)>,
}

#[derive(Serialize)]
pub struct Proc {
    pid: u32,
    parent: Option<u32>,
    name: String,
    user: String,
    exe: String,
    rss: u64,
    cpu: f32,
    read_rate: f64,
    write_rate: f64,
    net_in: f64,
    net_out: f64,
    run_time: u64,
    /// GPU activity proxy from IOKit: open accelerator clients and active command queues.
    gpu_clients: u32,
    gpu_queues: u32,
    /// GPU busy % for this process from Metal's accumulated GPU time (IOKit, no root needed).
    gpu_pct: Option<f32>,
}

#[derive(Serialize)]
pub struct Snapshot {
    total: u64,
    used: u64,
    available: u64,
    cpu: f32,
    cpus: usize,
    gpu: Option<u32>,
    load: f64,
    procs: Vec<Proc>,
}

impl Hog {
    pub fn new() -> Hog {
        let net = Arc::new(Mutex::new(Net::default()));
        let bg = net.clone();
        std::thread::spawn(move || loop {
            let t0 = Instant::now();
            let out = Command::new("nettop").args(["-P", "-L", "1", "-x", "-J", "bytes_in,bytes_out"]).output();
            let Ok(out) = out else { std::thread::sleep(Duration::from_secs(10)); continue };
            let mut cur = HashMap::new();
            for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
                let cols: Vec<&str> = line.split(',').collect();
                if cols.len() < 3 { continue }
                let Some(pid) = cols[0].rsplit('.').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
                let i = cols[1].parse().unwrap_or(0);
                let o = cols[2].parse().unwrap_or(0);
                cur.insert(pid, (i, o));
            }
            let mut n = bg.lock().unwrap();
            n.prev = std::mem::replace(&mut n.cur, cur);
            n.elapsed = t0.elapsed().as_secs_f64().max(0.5);
            drop(n);
            std::thread::sleep(Duration::from_secs(2));
        });
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_processes(ProcessesToUpdate::All, true); // baseline so the first snapshot has real deltas
        Hog { sys: Mutex::new((sys, Instant::now())), users: Users::new_with_refreshed_list(), net, gpu_prev: Mutex::new((Instant::now(), HashMap::new())) }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut g = self.sys.lock().unwrap();
        let (sys, last) = &mut *g;
        let dt = last.elapsed().as_secs_f64().max(0.2);
        *last = Instant::now();
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let rates: HashMap<u32, (f64, f64)> = {
            let n = self.net.lock().unwrap();
            n.cur.iter().map(|(pid, (i, o))| {
                let (pi, po) = n.prev.get(pid).copied().unwrap_or((*i, *o));
                (*pid, ((i.saturating_sub(pi)) as f64 / n.elapsed, (o.saturating_sub(po)) as f64 / n.elapsed))
            }).collect()
        };
        let gpu = gpu_clients();
        // GPU busy % = growth of accumulated GPU time since the last sample
        let gpu_pct: HashMap<u32, f32> = {
            let mut prev = self.gpu_prev.lock().unwrap();
            let ns = prev.0.elapsed().as_nanos().max(1) as f64;
            let cur: HashMap<u32, u64> = gpu.iter().map(|(pid, g)| (*pid, g.2)).collect();
            let out = cur.iter().map(|(pid, t)| (*pid, prev.1.get(pid).map(|p| ((t.saturating_sub(*p)) as f64 / ns * 100.0) as f32).unwrap_or(0.0))).collect();
            *prev = (Instant::now(), cur);
            out
        };
        let mut procs: Vec<Proc> = sys
            .processes()
            .values()
            .filter(|p| p.memory() > 0)
            .map(|p| {
                let pid = p.pid().as_u32();
                let d = p.disk_usage();
                let (net_in, net_out) = rates.get(&pid).copied().unwrap_or((0.0, 0.0));
                Proc {
                    pid,
                    parent: p.parent().map(|x| x.as_u32()),
                    name: p.name().to_string_lossy().into_owned(),
                    user: p.user_id().and_then(|u| self.users.get_user_by_id(u)).map(|u| u.name().to_string()).unwrap_or_default(),
                    exe: p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(),
                    rss: p.memory(),
                    cpu: p.cpu_usage(),
                    read_rate: d.read_bytes as f64 / dt,
                    write_rate: d.written_bytes as f64 / dt,
                    net_in,
                    net_out,
                    run_time: p.run_time(),
                    gpu_clients: gpu.get(&pid).map(|g| g.0).unwrap_or(0),
                    gpu_queues: gpu.get(&pid).map(|g| g.1).unwrap_or(0),
                    gpu_pct: Some(gpu_pct.get(&pid).copied().unwrap_or(0.0)),
                }
            })
            .collect();
        procs.sort_unstable_by(|a, b| b.rss.cmp(&a.rss));
        procs.truncate(120);
        Snapshot {
            total: sys.total_memory(),
            used: sys.used_memory(),
            available: sys.available_memory(),
            cpu: sys.global_cpu_usage(),
            cpus: sys.cpus().len().max(1),
            gpu: gpu_util(),
            load: System::load_average().one,
            procs,
        }
    }
}

/// Per-pid (accelerator clients, active command queues, accumulated GPU time in ns) from IOKit's
/// Metal user clients. `AppUsage` carries `accumulatedGPUTime` per client, which is real GPU time.
fn gpu_clients() -> HashMap<u32, (u32, u32, u64)> {
    let Ok(o) = Command::new("ioreg").args(["-r", "-c", "AGXDeviceUserClient", "-l", "-w0"]).output() else { return HashMap::new() };
    parse_gpu_clients(&String::from_utf8_lossy(&o.stdout))
}

pub fn parse_gpu_clients(text: &str) -> HashMap<u32, (u32, u32, u64)> {
    let mut out = HashMap::new();
    // properties inside an entry come in any order, so buffer per `{ ... }` block
    let (mut pid, mut queues, mut gpu_ns) = (None, 0u32, 0u64);
    for line in text.lines() {
        let t = line.trim_matches(|c: char| c == ' ' || c == '|' || c == '\t'); // ioreg draws a tree with '|'
        if let Some(rest) = t.strip_prefix("\"IOUserClientCreator\" = \"pid ") {
            pid = rest.split(',').next().and_then(|p| p.trim().parse::<u32>().ok());
        } else if let Some(rest) = t.strip_prefix("\"CommandQueueCount\" = ") {
            queues = rest.trim().parse().unwrap_or(0);
        } else if t.starts_with("\"AppUsage\"") {
            gpu_ns = t.split("\"accumulatedGPUTime\"=").skip(1).filter_map(|s| s.split(|c: char| !c.is_ascii_digit()).next()?.parse::<u64>().ok()).sum();
        } else if t == "}" {
            if let Some(p) = pid {
                let e = out.entry(p).or_insert((0, 0, 0));
                e.0 += 1;
                e.1 += queues;
                e.2 += gpu_ns;
            }
            pid = None; queues = 0; gpu_ns = 0;
        }
    }
    out
}

/// GPU busy percentage from IOKit, if the accelerator reports one.
fn gpu_util() -> Option<u32> {
    let out = Command::new("ioreg").args(["-r", "-d", "1", "-c", "IOAccelerator"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split("\"Device Utilization %\"=").skip(1).filter_map(|t| t.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).max()
}

#[derive(Serialize)]
pub struct OpenFile {
    pub path: String,
}

/// Regular files a process has open, via `lsof`. Own processes only without root.
pub fn open_files(pid: u32) -> Vec<OpenFile> {
    let Ok(out) = Command::new("lsof").args(["-p", &pid.to_string(), "-Ftn", "-w"]).output() else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files = vec![];
    let mut is_reg = false;
    for line in text.lines() {
        match line.as_bytes().first() {
            Some(b't') => is_reg = &line[1..] == "REG",
            Some(b'n') if is_reg => {
                let p = &line[1..];
                let noise = p.starts_with("/System/") || p.starts_with("/usr/") || p.starts_with("/private/var/db/") || p.starts_with("/dev/")
                    || p.contains(".framework/") || p.ends_with(".dylib") || p.contains("/Library/Preferences/Logging/") || p.contains("code_sign_clone");
                if !noise && !files.iter().any(|f: &OpenFile| f.path == p) {
                    files.push(OpenFile { path: p.to_string() });
                }
            }
            _ => {}
        }
    }
    files.truncate(300);
    files
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_gpu_clients() {
        let text = "  | {\n  |   \"AppUsage\" = ({\"API\"=\"Metal\",\"lastSubmittedTime\"=7,\"accumulatedGPUTime\"=1000},{\"API\"=\"Metal\",\"lastSubmittedTime\"=0,\"accumulatedGPUTime\"=500})\n  |   \"IOUserClientCreator\" = \"pid 1463, Google Chrome\"\n  |   \"CommandQueueCount\" = 2\n  | }\n  | {\n  |   \"AppUsage\" = ()\n  |   \"IOUserClientCreator\" = \"pid 1463, Google Chrome\"\n  | }\n";
        let m = super::parse_gpu_clients(text);
        assert_eq!(m[&1463], (2, 2, 1500));
    }
}
