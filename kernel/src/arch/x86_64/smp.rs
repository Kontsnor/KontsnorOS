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

//! Symmetric Multiprocessing (SMP) support and CPU core manager.

use crate::kprintln;
use spin::Mutex;

use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, Ordering};

global_asm!(
    r#"
    .section .ap_trampoline, "ax"
    .global ap_trampoline_start
    .global ap_trampoline_end
    
    ap_trampoline_start:
    .code16
    entry:
    cli
    
    # Set segment registers to 0
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    
    # Checkpoint 0x1111: Real Mode entered (write to offset 294 / 0x124)
    mov ax, 0x1111
    mov [0x8124], ax
    
    # Load temporary GDT (using DS = 0, address 0x8180)
    lgdt [0x8180]
    
    # Enable protected mode
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    
    # Far jump to 32-bit protected mode at 0x8050
    # 0x66 0xea [offset] [selector]
    .byte 0x66, 0xea, 0x50, 0x80, 0x00, 0x00, 0x08, 0x00
    
    # Protected mode entry at offset 0x50 (80)
    .org 0x50
    .code32
    protected_mode:
    # Set data selectors
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    
    # Checkpoint 0x22222222: Protected Mode entered (write to offset 294 / 0x124)
    mov eax, 0x22222222
    mov [0x8124], eax
    
    # Load CR3 (page table root) from offset 0x8120
    mov eax, [0x8120]
    mov cr3, eax
    
    # Enable PAE
    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax
    
    # Enable Long Mode and NXE in EFER MSR
    mov ecx, 0xC0000080
    rdmsr
    or eax, 0x900  # Bit 8 = LME, Bit 11 = NXE
    wrmsr
    
    # Enable Paging
    mov eax, cr0
    or eax, 1 << 31
    mov cr0, eax
    
    # Far jump to 64-bit long mode at 0x80a0
    # 0xea [offset] [selector]
    .byte 0xea, 0xa0, 0x80, 0x00, 0x00, 0x18, 0x00
    
    # Long mode entry at offset 0xa0 (160)
    .org 0xa0
    .code64
    long_mode:
    # Set segment registers to 0 in 64-bit mode
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    
    # Checkpoint 0x33333333: Long Mode entered
    mov ebx, 0x8124
    mov eax, 0x33333333
    mov [rbx], eax
    
    # Load stack pointer from offset 0x8128 using absolute address load via ebx
    mov ebx, 0x8128
    mov rsp, [rbx]
    
    # Jump to ap_entry from offset 0x8130 using absolute address load via ebx
    mov ebx, 0x8130
    mov rax, [rbx]
    jmp rax
    
    # Communication variables block at offset 0x120 (288)
    .org 0x120
    ap_pml4:       .long 0
    ap_ready:      .long 0
    ap_stack_top:  .quad 0
    ap_entry_ptr:  .quad 0
    
    # GDT start at offset 0x150 (336)
    .org 0x150
    gdt_start:
        .quad 0x0000000000000000          # Null descriptor
        .quad 0x00cf9a000000ffff          # 32-bit Code descriptor (0x08)
        .quad 0x00cf92000000ffff          # 32-bit Data descriptor (0x10)
        .quad 0x00209a0000000000          # 64-bit Code descriptor (0x18)
    gdt_end:
    
    # GDT descriptor at offset 0x180 (384)
    .org 0x180
    gdt_desc:
        .word gdt_end - gdt_start - 1
        .long 0x8150
        
    ap_trampoline_end:
    "#
);

extern "C" {
    fn ap_trampoline_start();
    fn ap_trampoline_end();
}

/// Representation of a single CPU core.
#[derive(Debug, Clone)]
pub struct Cpu {
    /// Local APIC ID of this processor.
    pub apic_id: u8,
    /// Whether this core has started up.
    pub started: bool,
    /// Whether this core is the Bootstrap Processor (BSP).
    pub is_bsp: bool,
}

/// Global CPU list manager.
pub struct CpuManager {
    cpus: [Option<Cpu>; 32],
    count: usize,
}

impl CpuManager {
    const fn new() -> Self {
        const INIT_CPU: Option<Cpu> = None;
        Self {
            cpus: [INIT_CPU; 32],
            count: 0,
        }
    }
}

static CPU_MANAGER: Mutex<CpuManager> = Mutex::new(CpuManager::new());

/// Global lock for serializing TLB shootdowns across all cores.
static TLB_SHOOTDOWN_LOCK: Mutex<()> = Mutex::new(());

/// Global atomic counter for tracking TLB shootdown acknowledgements.
static TLB_SHOOTDOWN_ACKS: AtomicU32 = AtomicU32::new(0);

/// Initialize the CPU manager using core enumeration from the MADT.
pub fn init() {
    let mut manager = CPU_MANAGER.lock();
    let bsp_apic_id = super::apic::get_lapic_id();

    let madt_info = match crate::acpi::get_madt_info() {
        Some(info) => info,
        None => {
            // ACPI not available; assume single-core BSP system
            manager.cpus[0] = Some(Cpu {
                apic_id: bsp_apic_id,
                started: true,
                is_bsp: true,
            });
            manager.count = 1;
            kprintln!("[smp] ACPI MADT not available. Single-core fallback BSP initialized.");
            return;
        }
    };

    let mut count = 0;
    for cpu_info in madt_info.cpus.iter() {
        if cpu_info.enabled && count < 32 {
            let is_bsp = cpu_info.apic_id == bsp_apic_id;
            manager.cpus[count] = Some(Cpu {
                apic_id: cpu_info.apic_id,
                started: is_bsp, // BSP is already started, APs are not
                is_bsp,
            });
            kprintln!(
                "[smp] Core {}: APIC ID {}, is_bsp={}",
                count,
                cpu_info.apic_id,
                is_bsp
            );
            count += 1;
        }
    }
    manager.count = count;

    kprintln!(
        "[smp] CPU Manager initialized with {} logical cores.",
        count
    );
}

/// Retrieve the number of logical CPU cores.
pub fn get_cpu_count() -> usize {
    CPU_MANAGER.lock().count
}

/// Get the Local APIC ID of the currently executing processor core.
pub fn current_lapic_id() -> u8 {
    super::apic::get_lapic_id()
}

/// Broadcast a TLB shootdown interrupt to all other logical CPU cores.
///
/// Under SMP, we broadcast the IPI and block until all other active cores
/// have processed the flush, preventing use-after-free conditions.
///
/// # Panics
///
/// This function must not be called from interrupt/exception context as it
/// can produce a deadlock if another core is also waiting for a TLB shootdown ACK.
pub fn shootdown_tlb() {
    let mut target_count = 0;
    {
        let current_id = current_lapic_id();
        let manager = CPU_MANAGER.lock();
        for i in 0..manager.count {
            if let Some(ref cpu) = manager.cpus[i] {
                if cpu.started && cpu.apic_id != current_id {
                    target_count += 1;
                }
            }
        }
    }

    if target_count > 0 {
        // F-08: Ensure we are not in an interrupt context under SMP
        debug_assert!(
            x86_64::instructions::interrupts::are_enabled(),
            "shootdown_tlb called with interrupts disabled (potential deadlock)"
        );

        let _lock = TLB_SHOOTDOWN_LOCK.lock();
        TLB_SHOOTDOWN_ACKS.store(target_count as u32, Ordering::SeqCst);

        super::apic::broadcast_ipi_all_excluding_self(36);

        // Spin-wait with bounded timeout until all other cores have acknowledged the TLB flush.
        // Cores spinning in kernel space with interrupts disabled also poll and acknowledge
        // pending shootdowns during their spin loops.
        let mut spins = 0u32;
        while TLB_SHOOTDOWN_ACKS.load(Ordering::SeqCst) > 0 && spins < 200_000 {
            core::hint::spin_loop();
            spins += 1;
        }
    }
}

/// Query whether there is currently an active TLB shootdown waiting for acknowledgements.
#[inline]
pub fn has_pending_tlb_shootdown() -> bool {
    TLB_SHOOTDOWN_ACKS.load(Ordering::Relaxed) > 0
}

/// Acknowledge a pending TLB shootdown. Called by the IPI handler or spinlock polling.
pub fn tlb_shootdown_ack() {
    let mut current = TLB_SHOOTDOWN_ACKS.load(Ordering::Relaxed);
    while current > 0 {
        match TLB_SHOOTDOWN_ACKS.compare_exchange_weak(
            current,
            current - 1,
            Ordering::SeqCst,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(val) => current = val,
        }
    }
}

/// Start the secondary CPU cores (APs) using the APIC INIT-SIPI-SIPI protocol.
pub fn start_aps() {
    // Map the trampoline physical page 0x8000 to virtual address 0x8000 identity-mapped
    // to allow the AP core to transition from real mode to protected and long mode.
    use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    let page = Page::<Size4KiB>::from_start_address(VirtAddr::new(0x8000)).unwrap();
    let frame = PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(0x8000)).unwrap();
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    kprintln!("[smp] Mapping trampoline page...");
    unsafe {
        crate::memory::r#virtual::map_page(page, frame, flags)
            .expect("Failed to map trampoline page");
    }
    kprintln!("[smp] Trampoline page mapped.");

    let mut manager = CPU_MANAGER.lock();
    let count = manager.count;
    if count <= 1 {
        kprintln!("[smp] No secondary cores to boot.");
        return;
    }

    kprintln!("[smp] Starting secondary cores...");

    // Copy trampoline to physical address 0x8000
    let trampoline_start = ap_trampoline_start as *const u8;
    let trampoline_end = ap_trampoline_end as *const u8;
    let size = trampoline_end as usize - trampoline_start as usize;

    let dest_addr = 0x8000 + crate::memory::r#virtual::phys_mem_offset();

    // SAFETY: Copying the AP trampoline to the physical address 0x8000 is safe
    // because this memory region is identity mapped and reserved for CPU booting.
    unsafe {
        core::ptr::copy_nonoverlapping(trampoline_start, dest_addr as *mut u8, size);
    }

    kprintln!(
        "[smp] Trampoline src: {:p}, dest: {:#x}, size: {}",
        trampoline_start,
        dest_addr,
        size
    );
    let read_back = unsafe { core::slice::from_raw_parts(dest_addr as *const u8, size) };
    kprintln!("[smp] Trampoline read-back (full):");
    for (idx, chunk) in read_back.chunks(32).enumerate() {
        kprintln!("[smp]   {:#04x}: {:?}", idx * 32, chunk);
    }

    let ap_pml4_offset = 288;
    let ap_ready_offset = 292;
    let ap_stack_top_offset = 296;
    let ap_entry_ptr_offset = 304;

    for i in 0..count {
        if let Some(ref mut cpu) = manager.cpus[i] {
            if cpu.is_bsp {
                continue;
            }

            let apic_id = cpu.apic_id;
            kprintln!("[smp] Booting AP core {} (APIC ID {})...", i, apic_id);

            // 1. Allocate a unique PID for the AP's idle task
            let ap_idle_pid = crate::process::pid::allocate();

            // 2. Allocate stack (64 KiB)
            let stack_size = 65536;
            let layout = alloc::alloc::Layout::from_size_align(stack_size, 16).unwrap();

            // SAFETY: Allocating memory for the AP stack using standard layout is safe.
            let stack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
            let stack_top = stack_base + stack_size as u64;

            // 3. Get current CR3
            // SAFETY: Reading CR3 on the BSP is safe.
            let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
            let cr3_val = cr3_frame.start_address().as_u64();

            // 4. Create task
            let mut ap_idle_task = crate::process::task::Task::new(
                ap_idle_pid,
                alloc::format!("idle-{}", apic_id),
                cr3_val,
            );
            ap_idle_task.kernel_stack_base = stack_base;
            ap_idle_task.kernel_stack_size = stack_size;
            ap_idle_task.priority = crate::process::task::Priority::Idle;
            ap_idle_task.state = crate::process::task::TaskState::Running;
            ap_idle_task.is_idle = true;

            // 5. Register in SCHEDULER and TASKS
            {
                let mut sched = crate::process::scheduler::SCHEDULER.lock();
                let task_arc = alloc::sync::Arc::new(spin::Mutex::new(ap_idle_task));
                let idx = ap_idle_pid.as_u64() as usize;
                {
                    let mut tasks = crate::process::scheduler::TASKS.write();
                    while tasks.len() <= idx {
                        tasks.push(None);
                    }
                    tasks[idx] = Some(task_arc);
                }
                if let Some(ref mut s) = *sched {
                    s.current_cpus[apic_id as usize] = Some(ap_idle_pid);
                    s.idle_cpus[apic_id as usize] = ap_idle_pid;
                }
            }

            // 7. Write boot parameters into the communication block
            // SAFETY: Writing to the allocated and identity-mapped trampoline block is safe.
            unsafe {
                core::ptr::write_volatile(
                    (dest_addr + ap_pml4_offset as u64) as *mut u32,
                    cr3_val as u32,
                );
                core::ptr::write_volatile(
                    (dest_addr + ap_stack_top_offset as u64) as *mut u64,
                    stack_top,
                );
                core::ptr::write_volatile(
                    (dest_addr + ap_entry_ptr_offset as u64) as *mut u64,
                    ap_entry as *const () as u64,
                );
                core::ptr::write_volatile((dest_addr + ap_ready_offset as u64) as *mut u32, 0);
            }

            // 8. Send INIT-SIPI-SIPI sequence
            super::apic::send_init_ipi(apic_id);
            super::apic::delay_us(10000); // 10ms delay

            super::apic::send_startup_ipi(apic_id, 0x08); // vector 0x08 for physical 0x8000
            super::apic::delay_us(200); // 200us delay

            super::apic::send_startup_ipi(apic_id, 0x08);
            super::apic::delay_us(200); // 200us delay

            // 9. Wait for ready signal from the AP
            let mut timeout = 20000000;
            let mut last_state = 0;

            while timeout > 0 {
                let state = unsafe {
                    core::ptr::read_volatile((dest_addr + ap_ready_offset as u64) as *const u32)
                };
                if state != last_state {
                    kprintln!("[smp] AP core {} state changed: {:#x}", apic_id, state);
                    last_state = state;
                }
                if state == 1 {
                    break;
                }
                timeout -= 1;
                core::hint::spin_loop();
            }

            if timeout == 0 {
                panic!(
                    "Timed out waiting for AP core {} to boot! Last state: {:#x}",
                    apic_id, last_state
                );
            }

            cpu.started = true;
            kprintln!(
                "[smp] AP core {} (APIC ID {}) started successfully.",
                i,
                apic_id
            );
        }
    }
}

/// 64-bit entry point for secondary Application Processor (AP) cores.
///
/// # Safety
///
/// This function is the raw entry point called by the assembly trampoline.
/// It must initialize the processor state and never return.
#[no_mangle]
pub extern "C" fn ap_entry() -> ! {
    // 1. Load GDT and TSS for this core
    super::gdt::init_heap();

    // 2. Load IDT
    super::interrupts::init_idt();

    // 3. Enable SSE and FSGSBASE
    // SAFETY: Enabling SSE and FSGSBASE is safe on x86_64 CPUs.
    unsafe {
        super::boot::enable_sse();
        super::boot::enable_fsgsbase();
    }

    // 4. Configure syscall MSR registers (STAR, LSTAR, FMASK, GS_BASE, KERNEL_GS_BASE)
    crate::syscall::init();

    // 5. Initialize AP LAPIC and LAPIC timer
    super::apic::init_ap();

    // 6. Signal the BSP that we are ready
    let dest_addr = 0x8000 + crate::memory::r#virtual::phys_mem_offset();
    let ap_ready_offset = 292;

    // SAFETY: Writing the ready flag to the mapped trampoline block is safe.
    unsafe {
        core::ptr::write_volatile((dest_addr + ap_ready_offset as u64) as *mut u32, 1);
    }

    kprintln!(
        "[smp] AP core (APIC ID {}) running. Entering scheduler...",
        current_lapic_id()
    );

    // 7. Enable interrupts
    x86_64::instructions::interrupts::enable();

    // 8. Enter the scheduler loop
    loop {
        crate::process::scheduler::schedule();
        x86_64::instructions::hlt();
    }
}
