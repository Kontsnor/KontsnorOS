---
name: kernel-smp
description: Specialized instructions for developing Symmetric Multiprocessing (SMP), interrupt handling, fine-grained locking, and TLB coherency.
---

# 🧱 Symmetric Multiprocessing & Hardened Synchronization Skill

This skill contains specialized system architecture guidelines, implementation plans, and code patterns for executing **Phase A** of the KontsnorOS roadmap. Use this skill when working on APIC configuration, CPU scheduling, spinlocks, Inter-Processor Interrupts (IPIs), or virtual memory coherency (TLB shootdowns).

---

## 🗺️ Architectural Context & Execution Flow

KontsnorOS boots on a single core (the Bootstrap Processor, or BSP). The secondary cores (Application Processors, or APs) are detected via ACPI tables but are currently idle. SMP execution transitions the kernel to run scheduler ticks, page mappings, and threads across all logical cores concurrently.

```
                  ┌──────────────────────────────┐
                  │   ACPI Table Parser (MADT)   │
                  └──────────────┬───────────────┘
                                 ▼
                  ┌──────────────────────────────┐
                  │ Send Startup IPIs to AP Cores│
                  └──────────────┬───────────────┘
                                 ▼
             ┌───────────────────┴───────────────────┐
             ▼                                       ▼
 ┌───────────────────────┐               ┌───────────────────────┐
 │ Bootstrap Proc (BSP)  │               │ Application Proc (AP) │
 ├───────────────────────┤               ├───────────────────────┤
 │ LAPIC Timer Scheduler │               │ LAPIC Timer Scheduler │
 │ Global Task Queue     │ <───[Locks]───> │ Global Task Queue     │
 └───────────┬───────────┘               └───────────┬───────────┘
             │                                       │
             └───────────► [IPI Channel] ◄───────────┘
```

---

## 🛠️ Step-by-Step Technical Requirements

### 1. Local APIC Timer Setup per Core
Each core must operate its own hardware timer to drive the scheduler preemption ticks, avoiding lock contention on a single global timer.

* **Register Offsets**:
  - LAPIC Base Address: Typically found in the ACPI MADT or read from `IA32_APIC_BASE` MSR (`0x1B`).
  - Timer LVT Register Offset: `0x320`
  - Initial Count Register Offset: `0x380`
  - Current Count Register Offset: `0x390`
  - Divide Configuration Register Offset: `0x3E0`
* **Configuration Protocol**:
  1. Map the physical LAPIC address into virtual memory with `NO_CACHE` and `WRITABLE` page flags.
  2. Set the Divide Configuration register to divide-by-16.
  3. Calibrate the timer count against the Pit (Programmable Interval Timer) to match standard HZ rates (e.g. 100Hz = 10ms intervals).
  4. Write the calibrated tick value to the Initial Count register.
  5. Program LVT Timer register with interrupt vector `0x40` (or another free vector) and set mode to **Periodic** (bit 17 = 1).

### 2. Inter-Processor Interrupts (IPIs)
APIC cores communicate asynchronously via the **Interrupt Command Register (ICR)** at offsets `0x300` (low 32-bits) and `0x310` (high 32-bits).

* **ICR Bit Mappings (Low 32-bits)**:
  - Vector: bits 0-7
  - Delivery Mode: bits 8-10 (000 = Fixed, 100 = INIT, 101 = Start-up)
  - Destination Mode: bit 11 (0 = Physical, 1 = Logical)
  - Delivery Status: bit 12 (0 = Idle, 1 = Send Pending)
  - Level: bit 14 (0 = De-assert, 1 = Assert)
  - Trigger Mode: bit 15 (0 = Edge, 1 = Level)
  - Destination Shorthand: bits 18-19 (00 = No Shorthand, 01 = Self, 10 = All Including Self, 11 = All Excluding Self)

* **Code Pattern: Sending an IPI in Rust**:
  ```rust
  // kernel/src/arch/x86_64/apic.rs
  pub unsafe fn send_ipi(target_lapic_id: u8, vector: u8) {
      let lapic_base = get_lapic_base();
      let icr_high = (lapic_base + 0x310) as *mut u32;
      let icr_low = (lapic_base + 0x300) as *mut u32;

      // 1. Wait for delivery status bit to clear
      while core::ptr::read_volatile(icr_low) & (1 << 12) != 0 {}

      // 2. Set target APIC ID in high ICR
      core::ptr::write_volatile(icr_high, (target_lapic_id as u32) << 24);

      // 3. Write vector and delivery flags to low ICR (Fixed, Edge, Physical)
      let icr_val = (vector as u32) | (0 << 8) | (0 << 11) | (1 << 14);
      core::ptr::write_volatile(icr_low, icr_val);
  }
  ```

### 3. Fine-Grained Locking & Spinlocks
To support SMP scheduler loops and physical block drivers, you must transition from global "Big Kernel Locks" to fine-grained mutexes or lock-free queue implementations.

* **Hardened Spinlock Design**:
  ```rust
  // kernel/src/sync/spinlock.rs
  use core::sync::atomic::{AtomicBool, Ordering};
  use core::cell::UnsafeCell;

  pub struct Spinlock<T> {
      lock: AtomicBool,
      data: UnsafeCell<T>,
  }

  unsafe impl<T: Send> Sync for Spinlock<T> {}

  impl<T> Spinlock<T> {
      pub const fn new(data: T) -> Self {
          Self {
              lock: AtomicBool::new(false),
              data: UnsafeCell::new(data),
          }
      }

      pub fn lock(&self) -> SpinlockGuard<'_, T> {
          // Prevent CPU instruction reordering and spin using spin-loop hints
          while self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
              core::hint::spin_loop();
          }
          SpinlockGuard { spinlock: self }
      }
  }

  pub struct SpinlockGuard<'a, T> {
      spinlock: &'a Spinlock<T>,
  }

  impl<'a, T> core::ops::Deref for SpinlockGuard<'a, T> {
      type Target = T;
      fn deref(&self) -> &Self::Target {
          unsafe { &*self.spinlock.data.get() }
      }
  }

  impl<'a, T> core::ops::DerefMut for SpinlockGuard<'a, T> {
      fn deref_mut(&mut self) -> &mut Self::Target {
          unsafe { &mut *self.spinlock.data.get() }
      }
  }

  impl<'a, T> Drop for SpinlockGuard<'a, T> {
      fn drop(&mut self) {
          self.spinlock.lock.store(false, Ordering::Release);
      }
  }
  ```

### 4. TLB Shootdowns & Virtual Memory Coherency
When a core updates its page mappings (e.g., `sys_munmap` or `sys_mprotect`), it must trigger a TLB (Translation Lookaside Buffer) shootdown on all other cores accessing that page table directory.

To prevent IPI storms (where a page-by-page mapping would broadcast individual IPIs for every single 4 KiB page, causing severe performance serialization), KontsnorOS implements a **Batched TLB Invalidation Protocol**:

* **Batched Protocol Flow**:
  1. **Disable Individual Shootdowns**: Range-mapped virtual memory operations (such as `sys_mmap`, `sys_munmap`, or `sys_mprotect`) invoke no-shootdown variants in a loop:
     - `map_user_page_no_shootdown(page_table_root, page, frame, flags)`
     - `unmap_user_page_no_shootdown(page_table_root, page)`
     - `update_user_page_flags_no_shootdown(page_table_root, page, flags)`
  2. **Acquire TLB Lock & Broadcast**: Once the loop completes, a single global `shootdown_tlb()` call is broadcasted at the syscall or operation boundary:
     - The initiator acquires the global `TLB_SHOOTDOWN_LOCK`.
     - An atomic target acknowledgement count is stored in `TLB_SHOOTDOWN_ACKS` (equal to `cpu_count - 1`).
     - A single Inter-Processor Interrupt (vector `36`) is sent to all other cores using `broadcast_ipi_all_excluding_self`.
  3. **Local & Remote Invalidation**: The current core performs its local TLB invalidation, while secondary cores service the IPI (vector `36`), invalidate their local TLB caches, and decrement the acknowledgement counter.
  4. **Synchronize & Release**: The initiator spins on the acknowledgement counter until all target cores have acknowledged, then releases the global lock.

---

## 🚀 Core-Local Architecture & Optimizations

KontsnorOS uses a highly optimized, core-local topology to minimize lock contention and MSR/VM exit overhead in multi-core execution.

### 1. Per-Core CPU-Local Storage
Each CPU core maintains private, fast-access scratch space to store critical registers and thread-tracking values during user/kernel context transitions:
* **`CpuScratch` Struct**:
  ```rust
  #[repr(C, align(16))]
  pub struct CpuScratch {
      pub user_rsp: u64,       // Offset 0x00
      pub kernel_rsp: u64,     // Offset 0x08
      pub current_pid: u64,    // Offset 0x10 (16)
      pub signals_pending: u64, // Offset 0x18 (24)
  }
  ```
* **Initialization & GS Mapping**:
  - The static array `CPU_SCRATCHES: [CpuScratch; 32]` holds the scratch space for up to 32 cores.
  - During core initialization, the active `GS_BASE` Model-Specific Register (MSR `0xC0000101`) is configured to point directly to the element corresponding to the core's Local APIC ID:
    ```rust
    let apic_id = current_lapic_id() as usize;
    let scratch_addr = addr_of!(CPU_SCRATCHES[apic_id]) as u64;
    Msr::new(0xC0000101).write(scratch_addr);
    ```

### 2. Lock-Free Thread Tracking
By pinning the core's `CpuScratch` to the `GS` register base, the currently active thread's PID can be retrieved lock-free via memory-indirect addressing:
* **Assembly Implementation**:
  ```assembly
  mov reg, gs:[16]
  ```
  This retrieves `CPU_SCRATCH.current_pid` directly from offset `16` (`gs:[16]`), bypassing the global scheduler lock and avoiding core contention.

### 3. FS_BASE/GS Context Switch Optimization
MSR access is expensive and causes VM exits under virtualized environments (like QEMU). The context switcher minimizes these overheads:
* **GS Pinning**: `GS_BASE` is permanently pinned to the core's `CpuScratch` block, meaning `GS` MSR writes are completely omitted during context switches.
* **Conditional FS_BASE and KERNEL_GS_BASE Updates**: Writes to `FS_BASE` MSR (`0xC0000100`) and `KERNEL_GS_BASE` MSR (`0xC0000102`) are optimized to only occur if the target base addresses differ from the currently active bases:
  ```assembly
  // Compare new fs_base against cached old fs_base
  mov rax, [rsi + 0x50]
  cmp rax, rbx
  je skip_fs
  wrmsr // Write new FS_BASE
  skip_fs:
  ```

---

## 🛡️ Critical Safety & Quality Checklist

Before finalizing any PR related to SMP or locking, verify the following:

- [ ] **Interrupt Re-entrancy Protection**: Are interrupts disabled (`cli`) before acquiring critical kernel spinlocks? Failing to do so triggers deadlocks if an interrupt fires while holding the lock and attempts to acquire it.
- [ ] **Deadlock Detection**: Have you verified the lock hierarchy? Do not acquire Lock B while holding Lock A if another thread acquires Lock A while holding Lock B.
- [ ] **Volatile Memory Access**: Are memory-mapped I/O registers accessed using `read_volatile` and `write_volatile` to prevent the Rust compiler from optimizing away essential read/write cycles?
- [ ] **Clippy Conformance**: Run `cargo clippy -- -D warnings` on both kernel and driver SDK.
