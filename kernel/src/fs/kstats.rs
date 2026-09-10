// Copyright (C) 2026 KontsnorOS Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Kernel performance statistics (kstats).
//!
//! Provides a set of global atomic counters that track key kernel performance
//! metrics. Exposed to user space through `/proc/kstats`.
//!
//! ## Counter List
//!
//! | Name                     | Description                                     |
//! |--------------------------|-------------------------------------------------|
//! | `context_switches`       | Total context switches performed                |
//! | `syscall_total`          | Total system calls dispatched                   |
//! | `page_faults_minor`      | Minor page faults (page present in cache)       |
//! | `page_faults_major`      | Major page faults (required disk I/O)           |
//! | `frame_alloc_lock_spins` | Times frame allocator lock was contended        |
//! | `sched_picks_rt`         | Scheduler picks from RealTime queue             |
//! | `sched_picks_high`       | Scheduler picks from High queue                 |
//! | `sched_picks_normal`     | Scheduler picks from Normal queue               |
//! | `sched_picks_low`        | Scheduler picks from Low queue                  |
//! | `sched_picks_idle`       | Scheduler picks from Idle queue                 |
//! | `dcache_hits`            | VFS dentry cache hits                           |
//! | `dcache_misses`          | VFS dentry cache misses                         |
//! | `page_cache_hits`        | Page cache hits (file I/O skipped disk)         |
//! | `page_cache_misses`      | Page cache misses (disk I/O required)           |
//!
//! ## Usage
//!
//! ```rust
//! // Increment from hot paths:
//! crate::fs::kstats::KSTATS.context_switches.fetch_add(1, Ordering::Relaxed);
//! ```
//!
//! ## Design Note
//!
//! All counters use `Ordering::Relaxed` — they are updated from interrupt
//! context and hot scheduler paths where sequential consistency is too
//! expensive. The values are informational estimates, not exact counts.

use core::sync::atomic::{AtomicU64, Ordering};

/// Global kernel performance statistics.
pub struct KStats {
    // ── Scheduler ────────────────────────────────────────────────────────
    /// Total context switches (incremented in `schedule()`).
    pub context_switches: AtomicU64,
    /// Scheduler picks from RealTime (priority 0) queue.
    pub sched_picks_rt: AtomicU64,
    /// Scheduler picks from High (priority 1) queue.
    pub sched_picks_high: AtomicU64,
    /// Scheduler picks from Normal (priority 2) queue.
    pub sched_picks_normal: AtomicU64,
    /// Scheduler picks from Low (priority 3) queue.
    pub sched_picks_low: AtomicU64,
    /// Scheduler picks from Idle (priority 4) queue.
    pub sched_picks_idle: AtomicU64,

    // ── Syscalls ─────────────────────────────────────────────────────────
    /// Total system calls dispatched.
    pub syscall_total: AtomicU64,

    // ── Memory ───────────────────────────────────────────────────────────
    /// Minor page faults handled (page was in the page cache).
    pub page_faults_minor: AtomicU64,
    /// Major page faults handled (required disk I/O).
    pub page_faults_major: AtomicU64,
    /// Number of times frame allocator global lock was spin-waited.
    pub frame_alloc_lock_spins: AtomicU64,

    // ── VFS Dentry Cache ─────────────────────────────────────────────────
    /// VFS dentry cache hits.
    pub dcache_hits: AtomicU64,
    /// VFS dentry cache misses.
    pub dcache_misses: AtomicU64,

    // ── Page Cache ───────────────────────────────────────────────────────
    /// Page cache hits (block I/O avoided).
    pub page_cache_hits: AtomicU64,
    /// Page cache misses (block I/O required).
    pub page_cache_misses: AtomicU64,

    // ── IPC ─────────────────────────────────────────────────────────────
    /// Futex wake operations performed.
    pub futex_wakes: AtomicU64,
    /// Futex wait operations performed.
    pub futex_waits: AtomicU64,
}

impl KStats {
    const fn new() -> Self {
        Self {
            context_switches: AtomicU64::new(0),
            sched_picks_rt: AtomicU64::new(0),
            sched_picks_high: AtomicU64::new(0),
            sched_picks_normal: AtomicU64::new(0),
            sched_picks_low: AtomicU64::new(0),
            sched_picks_idle: AtomicU64::new(0),
            syscall_total: AtomicU64::new(0),
            page_faults_minor: AtomicU64::new(0),
            page_faults_major: AtomicU64::new(0),
            frame_alloc_lock_spins: AtomicU64::new(0),
            dcache_hits: AtomicU64::new(0),
            dcache_misses: AtomicU64::new(0),
            page_cache_hits: AtomicU64::new(0),
            page_cache_misses: AtomicU64::new(0),
            futex_wakes: AtomicU64::new(0),
            futex_waits: AtomicU64::new(0),
        }
    }
}

/// The global kernel stats instance.
pub static KSTATS: KStats = KStats::new();

/// Render the kstats as a human-readable UTF-8 string.
///
/// Called by `/proc/kstats` to produce the file contents.
pub fn render() -> alloc::string::String {
    use alloc::format;

    let (dcache_hits, dcache_misses) = crate::fs::dcache::dcache_stats();

    // Merge live dcache stats into the KSTATS atomics for consistency.
    // We read them directly here since dcache maintains its own counters.
    let _ = KSTATS.dcache_hits.fetch_max(dcache_hits, Ordering::Relaxed);
    let _ = KSTATS
        .dcache_misses
        .fetch_max(dcache_misses, Ordering::Relaxed);

    format!(
        "context_switches        {}\n\
         sched_picks_rt          {}\n\
         sched_picks_high        {}\n\
         sched_picks_normal      {}\n\
         sched_picks_low         {}\n\
         sched_picks_idle        {}\n\
         syscall_total           {}\n\
         page_faults_minor       {}\n\
         page_faults_major       {}\n\
         frame_alloc_lock_spins  {}\n\
         dcache_hits             {}\n\
         dcache_misses           {}\n\
         page_cache_hits         {}\n\
         page_cache_misses       {}\n\
         futex_wakes             {}\n\
         futex_waits             {}\n",
        KSTATS.context_switches.load(Ordering::Relaxed),
        KSTATS.sched_picks_rt.load(Ordering::Relaxed),
        KSTATS.sched_picks_high.load(Ordering::Relaxed),
        KSTATS.sched_picks_normal.load(Ordering::Relaxed),
        KSTATS.sched_picks_low.load(Ordering::Relaxed),
        KSTATS.sched_picks_idle.load(Ordering::Relaxed),
        KSTATS.syscall_total.load(Ordering::Relaxed),
        KSTATS.page_faults_minor.load(Ordering::Relaxed),
        KSTATS.page_faults_major.load(Ordering::Relaxed),
        KSTATS.frame_alloc_lock_spins.load(Ordering::Relaxed),
        dcache_hits,
        dcache_misses,
        KSTATS.page_cache_hits.load(Ordering::Relaxed),
        KSTATS.page_cache_misses.load(Ordering::Relaxed),
        KSTATS.futex_wakes.load(Ordering::Relaxed),
        KSTATS.futex_waits.load(Ordering::Relaxed),
    )
}
