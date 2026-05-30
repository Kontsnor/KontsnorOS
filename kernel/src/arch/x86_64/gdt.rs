//! Global Descriptor Table (GDT) for KontsnorOS.
//!
//! The GDT defines memory segments for the CPU. In long mode (64-bit),
//! segmentation is largely disabled, but a GDT is still required for:
//!
//! - Defining kernel and user code/data segments (ring 0 vs ring 3)
//! - Setting up the Task State Segment (TSS) for interrupt stack switching
//!
//! ## Segments
//!
//! | Index | Segment          | DPL | Description                    |
//! |-------|-----------------|-----|--------------------------------|
//! | 0     | Null            | -   | Required null descriptor       |
//! | 1     | Kernel Code     | 0   | Ring 0 code segment            |
//! | 2     | Kernel Data     | 0   | Ring 0 data segment            |
//! | 3     | User Code       | 3   | Ring 3 code segment            |
//! | 4     | User Data       | 3   | Ring 3 data segment            |
//! | 5     | TSS             | 0   | Task State Segment             |

use lazy_static::lazy_static;
use x86_64::instructions::segmentation::{CS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::SegmentSelector;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// Index of the IST entry used for double fault handling.
/// Using a separate stack prevents triple faults when the kernel
/// stack overflows.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Size of the interrupt stack (32 KiB).
const INTERRUPT_STACK_SIZE: usize = 4096 * 8;

/// Stack used for double fault handling.
///
/// This is a separate stack from the kernel stack, ensuring that
/// double faults can be handled even if the kernel stack has overflowed.
static mut DOUBLE_FAULT_STACK: [u8; INTERRUPT_STACK_SIZE] = [0; INTERRUPT_STACK_SIZE];

/// Index of the IST entry used for page fault handling.
pub const PAGE_FAULT_IST_INDEX: u16 = 1;

/// Stack used for page fault handling.
static mut PAGE_FAULT_STACK: [u8; INTERRUPT_STACK_SIZE] = [0; INTERRUPT_STACK_SIZE];

lazy_static! {
    /// The Task State Segment (TSS).
    ///
    /// In 64-bit mode, the TSS is primarily used for:
    /// - Storing Interrupt Stack Table (IST) pointers for stack switching
    ///   during exceptions
    /// - Storing the I/O permission bitmap base address
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();

        // Set up the double fault handler stack (IST index 0)
        // SAFETY: We are the only code accessing DOUBLE_FAULT_STACK during
        // initialization. The stack is statically allocated and lives for
        // the entire duration of the kernel.
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            let stack_start = VirtAddr::from_ptr(core::ptr::addr_of!(DOUBLE_FAULT_STACK));
            stack_start + INTERRUPT_STACK_SIZE as u64
        };

        // Set up the page fault handler stack (IST index 1)
        tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = {
            let stack_start = VirtAddr::from_ptr(core::ptr::addr_of!(PAGE_FAULT_STACK));
            stack_start + INTERRUPT_STACK_SIZE as u64
        };

        tss
    };
}

lazy_static! {
    /// The Global Descriptor Table with segment selectors.
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();

        // Kernel segments (ring 0)
        let kernel_code = gdt.append(Descriptor::kernel_code_segment());
        let kernel_data = gdt.append(Descriptor::kernel_data_segment());

        // User segments (ring 3) — needed for syscall/sysret
        let user_data = gdt.append(Descriptor::user_data_segment());
        let user_code = gdt.append(Descriptor::user_code_segment());

        // Task State Segment
        let tss = gdt.append(Descriptor::tss_segment(&TSS));

        (gdt, Selectors {
            kernel_code,
            kernel_data,
            user_code,
            user_data,
            tss,
        })
    };
}

/// Segment selectors for accessing GDT entries.
#[derive(Debug, Clone, Copy)]
pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub user_code: SegmentSelector,
    pub user_data: SegmentSelector,
    pub tss: SegmentSelector,
}

/// Dynamic heap-allocated per-core GDT/TSS configuration.
pub struct CoreGdt {
    pub gdt: &'static GlobalDescriptorTable,
    pub tss: &'static mut TaskStateSegment,
    pub selectors: Selectors,
}

/// Thread-safe global cell for the active core GDT/TSS configuration.
pub static CORE_GDT: crate::sync::spinlock::TicketLock<Option<CoreGdt>> = crate::sync::spinlock::TicketLock::new(None);

/// Initialize the GDT and load segment registers.
///
/// This must be called early in the boot process, before interrupts
/// are enabled.
pub fn init() {
    GDT.0.load();

    // SAFETY: We just loaded a valid GDT containing these segments.
    unsafe {
        CS::set_reg(GDT.1.kernel_code);
        load_tss(GDT.1.tss);
    }
}

/// Initialize heap-allocated per-core GDT and TSS.
///
/// Once the kernel heap is initialized, we dynamically allocate the
/// GDT and TSS to avoid global static mutations and prepare for SMP.
pub fn init_heap() {
    use alloc::boxed::Box;

    let tss_mut = Box::leak(Box::new(TaskStateSegment::new()));

    // Set up the double fault and page fault handler stacks using statically allocated buffers
    tss_mut.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        let stack_start = VirtAddr::from_ptr(core::ptr::addr_of!(DOUBLE_FAULT_STACK));
        stack_start + INTERRUPT_STACK_SIZE as u64
    };
    tss_mut.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = {
        let stack_start = VirtAddr::from_ptr(core::ptr::addr_of!(PAGE_FAULT_STACK));
        stack_start + INTERRUPT_STACK_SIZE as u64
    };

    let tss_ref = unsafe { &*(tss_mut as *const TaskStateSegment) };

    let gdt_mut = Box::leak(Box::new(GlobalDescriptorTable::new()));

    let kernel_code = gdt_mut.append(Descriptor::kernel_code_segment());
    let kernel_data = gdt_mut.append(Descriptor::kernel_data_segment());
    let user_data = gdt_mut.append(Descriptor::user_data_segment());
    let user_code = gdt_mut.append(Descriptor::user_code_segment());
    let tss_sel = gdt_mut.append(Descriptor::tss_segment(tss_ref));

    let selectors = Selectors {
        kernel_code,
        kernel_data,
        user_code,
        user_data,
        tss: tss_sel,
    };

    let gdt_ref = unsafe { &*(gdt_mut as *const GlobalDescriptorTable) };
    gdt_ref.load();

    unsafe {
        CS::set_reg(selectors.kernel_code);
        load_tss(selectors.tss);
    }

    let mut lock = CORE_GDT.lock();
    *lock = Some(CoreGdt {
        gdt: gdt_ref,
        tss: tss_mut,
        selectors,
    });
}

/// Get the kernel code segment selector.
pub fn kernel_code_selector() -> SegmentSelector {
    let lock = CORE_GDT.lock();
    if let Some(ref core_gdt) = *lock {
        core_gdt.selectors.kernel_code
    } else {
        GDT.1.kernel_code
    }
}

/// Get the kernel data segment selector.
pub fn kernel_data_selector() -> SegmentSelector {
    let lock = CORE_GDT.lock();
    if let Some(ref core_gdt) = *lock {
        core_gdt.selectors.kernel_data
    } else {
        GDT.1.kernel_data
    }
}

/// Get the user code segment selector.
pub fn user_code_selector() -> SegmentSelector {
    let lock = CORE_GDT.lock();
    if let Some(ref core_gdt) = *lock {
        core_gdt.selectors.user_code
    } else {
        GDT.1.user_code
    }
}

/// Get the user data segment selector.
pub fn user_data_selector() -> SegmentSelector {
    let lock = CORE_GDT.lock();
    if let Some(ref core_gdt) = *lock {
        core_gdt.selectors.user_data
    } else {
        GDT.1.user_data
    }
}

/// Set the interrupt stack (RSP0) in the TSS for privilege transitions.
///
/// This is called during context switching to ensure that if an interrupt
/// occurs while executing in user space (Ring 3), the CPU switches to the
/// correct kernel stack for the active task.
pub fn set_interrupt_stack(stack_top: u64) {
    let mut lock = CORE_GDT.lock();
    if let Some(ref mut core_gdt) = *lock {
        core_gdt.tss.privilege_stack_table[0] = VirtAddr::new(stack_top);
    } else {
        // Fallback to static TSS if heap is not yet initialized
        unsafe {
            let tss_ptr = &*TSS as *const TaskStateSegment as *mut TaskStateSegment;
            (*tss_ptr).privilege_stack_table[0] = VirtAddr::new(stack_top);
        }
    }
}

