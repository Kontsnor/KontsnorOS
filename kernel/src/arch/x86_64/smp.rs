//! Symmetric Multiprocessing (SMP) support and CPU core manager.

use spin::Mutex;
use crate::kprintln;

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

    kprintln!("[smp] CPU Manager initialized with {} logical cores.", count);
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
pub fn shootdown_tlb() {
    if get_cpu_count() > 1 {
        super::apic::broadcast_ipi_all_excluding_self(36);
    }
}

