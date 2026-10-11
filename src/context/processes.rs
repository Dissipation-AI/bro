//! Running processes, read through libproc directly (no `ps` spawn).
//!
//! For requests about load ("what's eating my cpu"), CPU is measured over a short window between
//! two samples, so it reflects what's busy now. Otherwise it's each process's average since it
//! started, which needs no waiting. Other users' processes have names and pids but no figures.

use super::Process;
use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::time::{Duration, Instant};

/// Sampling window for CPU usage.
const WINDOW: Duration = Duration::from_millis(12);
const TOP_CPU: usize = 15;
const TOP_MEMORY: usize = 15;
const MATCHING: usize = 10;

unsafe extern "C" {
    fn proc_listallpids(buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_name(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    fn mach_timebase_info(info: *mut TimebaseInfo) -> c_int;
    fn mach_absolute_time() -> u64;
}

#[repr(C)]
#[derive(Default)]
struct TimebaseInfo {
    numer: u32,
    denom: u32,
}

const RUSAGE_INFO_V2: c_int = 2;

#[repr(C)]
#[derive(Default)]
struct RusageInfoV2 {
    uuid: [u8; 16],
    user_time: u64,
    system_time: u64,
    pkg_idle_wkups: u64,
    interrupt_wkups: u64,
    pageins: u64,
    wired_size: u64,
    resident_size: u64,
    phys_footprint: u64,
    proc_start_abstime: u64,
    proc_exit_abstime: u64,
    child_user_time: u64,
    child_system_time: u64,
    child_pkg_idle_wkups: u64,
    child_interrupt_wkups: u64,
    child_pageins: u64,
    child_elapsed_abstime: u64,
    diskio_bytesread: u64,
    diskio_byteswritten: u64,
}

fn all_pids() -> Vec<c_int> {
    let mut pids = vec![0 as c_int; 4096];
    loop {
        let bytes = (pids.len() * size_of::<c_int>()) as c_int;
        let n = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if n < 0 {
            return Vec::new();
        }
        if (n as usize) < pids.len() {
            pids.truncate(n as usize);
            pids.retain(|&p| p > 0);
            return pids;
        }
        pids.resize(pids.len() * 2, 0);
    }
}

fn name(pid: c_int) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe { proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

struct Usage {
    /// User and system time, in mach units.
    cpu: u64,
    /// Physical footprint in bytes.
    footprint: u64,
    /// Start time, in mach units.
    started: u64,
}

/// For processes we may inspect.
fn usage(pid: c_int) -> Option<Usage> {
    let mut info = RusageInfoV2::default();
    let r = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V2, (&mut info as *mut RusageInfoV2).cast()) };
    (r == 0).then_some(Usage {
        cpu: info.user_time + info.system_time,
        footprint: info.phys_footprint,
        started: info.proc_start_abstime,
    })
}

fn nanos_per_unit() -> f64 {
    let mut tb = TimebaseInfo::default();
    unsafe { mach_timebase_info(&mut tb) };
    if tb.denom == 0 { 1.0 } else { tb.numer as f64 / tb.denom as f64 }
}

/// Processes worth showing: those whose name matches a request word, then the busiest by CPU
/// and the largest by memory. bro's own process is left out.
pub fn sample(words: &[String], measure_cpu: bool) -> Vec<Process> {
    let me = std::process::id() as c_int;
    let pids = all_pids();
    let first: HashMap<c_int, u64> =
        if measure_cpu { pids.iter().filter_map(|&p| usage(p).map(|u| (p, u.cpu))).collect() } else { HashMap::new() };
    let t0 = Instant::now();
    if measure_cpu {
        std::thread::sleep(WINDOW);
    }
    let elapsed_ns = t0.elapsed().as_nanos().max(1) as f64;
    let scale = nanos_per_unit();
    let now = unsafe { mach_absolute_time() };

    let mut all: Vec<Process> = Vec::with_capacity(pids.len());
    for &pid in &pids {
        if pid == me {
            continue;
        }
        let Some(name) = name(pid) else { continue };
        let round = |percent: f64| Some((percent * 10.0).round() as f32 / 10.0);
        let (cpu_percent, memory_mb) = match (first.get(&pid), usage(pid)) {
            (Some(&cpu0), Some(u)) => {
                (round(u.cpu.saturating_sub(cpu0) as f64 * scale / elapsed_ns * 100.0), Some(u.footprint / 1_048_576))
            }
            (None, Some(u)) => {
                let lifetime = now.saturating_sub(u.started).max(1);
                (round(u.cpu as f64 / lifetime as f64 * 100.0), Some(u.footprint / 1_048_576))
            }
            _ => (None, None),
        };
        all.push(Process { name, pid, cpu_percent, memory_mb, cpu_average: !measure_cpu });
    }
    select(all, words)
}

fn select(all: Vec<Process>, words: &[String]) -> Vec<Process> {
    let mut chosen: Vec<Process> = Vec::new();
    let push = |p: &Process, chosen: &mut Vec<Process>| {
        if !chosen.iter().any(|c| c.pid == p.pid) {
            chosen.push(p.clone());
        }
    };
    for p in all.iter().filter(|p| super::relevant(&p.name, words)).take(MATCHING) {
        push(p, &mut chosen);
    }
    let mut by_cpu: Vec<&Process> = all.iter().filter(|p| p.cpu_percent.is_some_and(|c| c >= 1.0)).collect();
    by_cpu.sort_by(|a, b| b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap_or(std::cmp::Ordering::Equal));
    for p in by_cpu.into_iter().take(TOP_CPU) {
        push(p, &mut chosen);
    }
    let mut by_memory: Vec<&Process> = all.iter().filter(|p| p.memory_mb.is_some()).collect();
    by_memory.sort_by_key(|p| std::cmp::Reverse(p.memory_mb));
    for p in by_memory.into_iter().take(TOP_MEMORY) {
        push(p, &mut chosen);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_this_test_process_parent() {
        let pids = all_pids();
        assert!(pids.len() > 10);
        assert!(name(std::os::unix::process::parent_id() as c_int).is_some());
    }

    #[test]
    fn matching_processes_come_first() {
        let p = |name: &str, pid, cpu, mem| Process {
            name: name.into(),
            pid,
            cpu_percent: Some(cpu),
            memory_mb: Some(mem),
            cpu_average: false,
        };
        let all = vec![p("WindowServer", 1, 20.0, 900), p("ghostty", 2, 0.0, 400), p("cargo", 3, 90.0, 100)];
        let chosen = select(all, &["kill".into(), "ghostty".into()]);
        assert_eq!(chosen[0].name, "ghostty");
        assert_eq!(chosen[1].name, "cargo");
        assert_eq!(chosen.len(), 3);
    }
}
