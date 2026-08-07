use crate::state::AppState;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// PSS of one process in bytes. RSS overcounts multi-process trees because
/// every browser child reports the full shared footprint (binary, libs, shm),
/// so summing 150 children can exceed the machine's physical RAM. PSS divides
/// each shared page among its users, which is why the kernel exposes it.
/// Linux-only: returns None elsewhere so callers fall back to RSS.
fn pss_bytes(pid: u32) -> Option<u64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Best-effort process memory: PSS when available (Linux), RSS otherwise.
fn proc_mem_bytes(pid: u32, rss: u64) -> u64 {
    pss_bytes(pid).unwrap_or(rss)
}

/// One sampled snapshot of resource usage. Refreshed every 2s by the
/// background sampler and served from cache by /admin/usage/process.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UsageSnapshot {
    // marionette process
    pub pid: u32,
    pub cpu_percent: f32,
    pub mem_bytes: u64,
    // automation (farm python + browsers)
    pub automation_cpu_percent: f32,
    pub automation_mem_bytes: u64,
    pub automation_procs: u32,
    pub automation_running: bool,
    // system context
    pub logical_cores: usize,
    pub total_mem_bytes: u64,
    pub used_mem_bytes: u64,
    pub updated_at: String,
}

/// Shared handle to the latest snapshot. `None` until the first sample lands.
pub type UsageHandle = Arc<StdMutex<Option<UsageSnapshot>>>;

pub fn new_handle() -> UsageHandle {
    Arc::new(StdMutex::new(None))
}

/// Background sampler. Spawns a task that refreshes the process table every
/// `interval` and stores the result in `state.usage`.
pub fn spawn(state: AppState, interval: Duration) {
    let pid_self = sysinfo::get_current_pid().map(|p| p.as_u32()).unwrap_or(0);
    tokio::spawn(async move {
        let mut sys = System::new();
        // Prime the CPU counters so the first delta is meaningful.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_cpu().with_memory(),
        );
        loop {
            tokio::time::sleep(interval).await;

            let automation_roots = state.farm.automation_pids().await;
            let automation_running = !automation_roots.is_empty();

            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::new().with_cpu().with_memory(),
            );
            sys.refresh_memory();

            let snapshot = sample(&mut sys, pid_self, &automation_roots, automation_running);
            if let Ok(mut guard) = state.usage.lock() {
                *guard = Some(snapshot);
            }
        }
    });
}

fn sample(
    sys: &mut System,
    pid_self: u32,
    automation_roots: &[u32],
    automation_running: bool,
) -> UsageSnapshot {
    let procs = sys.processes();

    // Build a parent -> children index so we can walk the automation tree.
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, p) in procs.iter() {
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }

    let self_pid = Pid::from_u32(pid_self);
    let (cpu_percent, mem_bytes) = match procs.get(&self_pid) {
        Some(p) => (p.cpu_usage(), proc_mem_bytes(pid_self, p.memory())),
        None => (0.0, 0),
    };

    // BFS over every automation root, summing CPU + PSS of the whole subtree
    // (python farm process + every browser it spawned). PSS instead of RSS:
    // browser children share text/shm segments, so summing RSS inflates the
    // total far beyond physical RAM.
    let mut auto_cpu = 0.0f32;
    let mut auto_mem = 0u64;
    let mut auto_procs = 0u32;
    if automation_running {
        let mut seen: std::collections::HashSet<Pid> = std::collections::HashSet::new();
        let mut queue: VecDeque<Pid> = automation_roots.iter().map(|p| Pid::from_u32(*p)).collect();
        while let Some(pid) = queue.pop_front() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(p) = procs.get(&pid) {
                auto_cpu += p.cpu_usage();
                auto_mem += proc_mem_bytes(pid.as_u32(), p.memory());
                auto_procs += 1;
            }
            if let Some(kids) = children.get(&pid) {
                for k in kids {
                    queue.push_back(*k);
                }
            }
        }
    }

    let logical_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or_else(|_| sys.physical_core_count().unwrap_or(1));

    UsageSnapshot {
        pid: pid_self,
        cpu_percent,
        mem_bytes,
        automation_cpu_percent: auto_cpu,
        automation_mem_bytes: auto_mem,
        automation_procs: auto_procs,
        automation_running,
        logical_cores,
        total_mem_bytes: sys.total_memory(),
        used_mem_bytes: sys.used_memory(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    }
}
