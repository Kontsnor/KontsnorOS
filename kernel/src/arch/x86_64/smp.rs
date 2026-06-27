//! Symmetric Multiprocessing (SMP) support and CPU core manager.

use crate::kprintln;
use spin::Mutex;

use core::sync::atomic::{AtomicU32, Ordering};

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
static TLB_SHOOTDOWN_LOCK: crate::sync::spinlock::TicketLock<()> =
    crate::sync::spinlock::TicketLock::new(());

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
        let manager = CPU_MANAGER.lock();
        for i in 0..manager.count {
            if let Some(ref cpu) = manager.cpus[i] {
                if cpu.started && !cpu.is_bsp {
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

        // Spin-wait until all other cores have acknowledged the TLB flush
        while TLB_SHOOTDOWN_ACKS.load(Ordering::SeqCst) > 0 {
            core::hint::spin_loop();
        }
    }
}

/// Acknowledge a pending TLB shootdown. Called by the IPI handler.
pub fn tlb_shootdown_ack() {
    TLB_SHOOTDOWN_ACKS.fetch_sub(1, Ordering::SeqCst);
}
