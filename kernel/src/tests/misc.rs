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

//! Miscellaneous / Trivial test cases.

#[test_case]
fn test_trivial() {
    let two = 2;
    assert_eq!(1 + 1, two);
}

#[test_case]
fn test_pty_active_master_optimization() {
    crate::kprintln!("[test] Starting PTY active master optimization test...");

    // 1. Verify initial active master setting
    let pty1 = crate::fs::pty::allocate_new_pty().expect("Failed to allocate PTY 1");
    let version_before =
        crate::fs::pty::ACTIVE_PTY_VERSION.load(core::sync::atomic::Ordering::Acquire);

    crate::fs::pty::set_active_pty_master(Some(pty1.clone()));
    let version_after_pty1 =
        crate::fs::pty::ACTIVE_PTY_VERSION.load(core::sync::atomic::Ordering::Acquire);
    assert_eq!(version_after_pty1, version_before + 1);

    // Verify ACTIVE_PTY_MASTER is pty1
    let active_master = crate::fs::pty::ACTIVE_PTY_MASTER.lock().clone();
    assert!(active_master.is_some());

    // 2. Test dynamic switching to a new PTY master
    let pty2 = crate::fs::pty::allocate_new_pty().expect("Failed to allocate PTY 2");
    crate::fs::pty::set_active_pty_master(Some(pty2.clone()));
    let version_after_pty2 =
        crate::fs::pty::ACTIVE_PTY_VERSION.load(core::sync::atomic::Ordering::Acquire);
    assert_eq!(version_after_pty2, version_after_pty1 + 1);

    // 3. Test clearing active master
    crate::fs::pty::set_active_pty_master(None);
    let version_after_none =
        crate::fs::pty::ACTIVE_PTY_VERSION.load(core::sync::atomic::Ordering::Acquire);
    assert_eq!(version_after_none, version_after_pty2 + 1);
    assert!(crate::fs::pty::ACTIVE_PTY_MASTER.lock().is_none());

    // Restore pty1 as active
    crate::fs::pty::set_active_pty_master(Some(pty1.clone()));

    // 4. Benchmark performance difference over 100,000 iterations
    const ITERATIONS: usize = 100_000;

    // Baseline: locking and cloning Arc on every iteration
    let start_tsc_baseline = unsafe { core::arch::x86_64::_rdtsc() };
    for _ in 0..ITERATIONS {
        let master_opt = crate::fs::pty::ACTIVE_PTY_MASTER.lock().clone();
        core::hint::black_box(&master_opt);
    }
    let end_tsc_baseline = unsafe { core::arch::x86_64::_rdtsc() };
    let baseline_cycles = end_tsc_baseline.saturating_sub(start_tsc_baseline);

    // Optimized: Atomic version load and cached reference reuse
    let start_tsc_opt = unsafe { core::arch::x86_64::_rdtsc() };
    let mut cached_master: Option<alloc::sync::Arc<dyn crate::fs::inode::InodeOps>> = None;
    let mut cached_version: u64 = 0;
    for _ in 0..ITERATIONS {
        let current_version =
            crate::fs::pty::ACTIVE_PTY_VERSION.load(core::sync::atomic::Ordering::Acquire);
        if current_version != cached_version || (cached_master.is_none() && current_version == 0) {
            cached_master = crate::fs::pty::ACTIVE_PTY_MASTER.lock().clone();
            cached_version = current_version;
        }
        if let Some(ref master) = cached_master {
            core::hint::black_box(master);
        }
    }
    let end_tsc_opt = unsafe { core::arch::x86_64::_rdtsc() };
    let opt_cycles = end_tsc_opt.saturating_sub(start_tsc_opt);

    crate::kprintln!(
        "[test] PTY Master Access Benchmark ({} iterations):",
        ITERATIONS
    );
    crate::kprintln!(
        "  Unoptimized (lock + Arc clone): {} cycles ({:.2} cycles/op)",
        baseline_cycles,
        baseline_cycles as f64 / ITERATIONS as f64
    );
    crate::kprintln!(
        "  Optimized (version check + cached Arc): {} cycles ({:.2} cycles/op)",
        opt_cycles,
        opt_cycles as f64 / ITERATIONS as f64
    );
    if baseline_cycles > opt_cycles {
        let speedup = (baseline_cycles - opt_cycles) as f64 / baseline_cycles as f64 * 100.0;
        crate::kprintln!(
            "  Speedup: {:.1}% reduction in CPU cycles ({:.1}x faster)",
            speedup,
            baseline_cycles as f64 / opt_cycles.max(1) as f64
        );
    }

    // Clean up active master
    crate::fs::pty::set_active_pty_master(None);

    crate::kprintln!("[test] PTY active master optimization test PASSED!");
}
