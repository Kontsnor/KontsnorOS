//! Interrupt Descriptor Table (IDT) and interrupt handling for x86_64.
//!
//! This module sets up:
//! - CPU exception handlers (divide by zero, page fault, double fault, etc.)
//! - Hardware interrupt handlers (timer, keyboard via PIC 8259)
//! - The Programmable Interrupt Controller (PIC) chain
//!
//! ## Interrupt Layout
//!
//! | Vector | Source              | Description                     |
//! |--------|--------------------|---------------------------------|
//! | 0      | CPU                | Division Error                  |
//! | 3      | CPU                | Breakpoint                      |
//! | 6      | CPU                | Invalid Opcode                  |
//! | 8      | CPU                | Double Fault                    |
//! | 13     | CPU                | General Protection Fault        |
//! | 14     | CPU                | Page Fault                      |
//! | 32     | PIC (IRQ 0)        | Timer                           |
//! | 33     | PIC (IRQ 1)        | Keyboard                        |

use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use crate::kprintln;

use super::gdt;

/// Offset for the primary PIC (master).
/// Hardware interrupts are mapped starting at vector 32 to avoid
/// conflicts with CPU exception vectors (0–31).
const PIC_1_OFFSET: u8 = 32;

/// Offset for the secondary PIC (slave).
const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// Chained PIC 8259 controller.
///
/// The x86 platform uses two cascaded 8259 PICs to handle 15 hardware
/// interrupt lines (IRQs 0–15).
pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

/// Hardware interrupt numbers (after PIC remapping).
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    /// Timer interrupt (IRQ 0, vector 32).
    Timer = PIC_1_OFFSET,
    /// Keyboard interrupt (IRQ 1, vector 33).
    Keyboard,
    /// IPI Reschedule interrupt (vector 34).
    IpiReschedule = 34,
    /// IPI Halt interrupt (vector 35).
    IpiHalt = 35,
    /// IPI TLB Shootdown interrupt (vector 36).
    IpiTlbShootdown = 36,
    /// Network interrupt (IRQ 11, vector 43).
    Network = 43,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }


}

lazy_static! {
    /// The Interrupt Descriptor Table.
    ///
    /// Maps interrupt/exception vectors to their handler functions.
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // ── CPU Exceptions ─────────────────────────────────────────
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.debug.set_handler_fn(debug_handler);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);

        // Double fault uses a separate IST stack to handle kernel stack overflow
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // Page fault uses a separate IST stack
        unsafe {
            idt.page_fault
                .set_handler_fn(page_fault_handler)
                .set_stack_index(gdt::PAGE_FAULT_IST_INDEX);
        }

        // ── Hardware Interrupts (APIC) ─────────────────────────────
        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::IpiReschedule.as_u8()].set_handler_fn(ipi_reschedule_handler);
        idt[InterruptIndex::IpiHalt.as_u8()].set_handler_fn(ipi_halt_handler);
        idt[InterruptIndex::IpiTlbShootdown.as_u8()].set_handler_fn(ipi_tlb_shootdown_handler);
        idt[InterruptIndex::Network.as_u8()].set_handler_fn(network_interrupt_handler);
        idt[255].set_handler_fn(spurious_interrupt_handler);

        idt
    };
}

/// Load the IDT into the CPU.
pub fn init_idt() {
    IDT.load();
}

/// Initialize and enable the PIC interrupt controllers.
pub fn init_pics() {
    // SAFETY: PIC initialization is required for hardware interrupt delivery.
    // The PIC offsets are correctly set to avoid conflicts with CPU exceptions.
    unsafe {
        PICS.lock().initialize();
    }
}

// ═══════════════════════════════════════════════════════════════════════
// CPU Exception Handlers
// ═══════════════════════════════════════════════════════════════════════

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    kprintln!("[EXCEPTION] Division Error");
    kprintln!("{:#?}", stack_frame);
    panic!("Unhandled division error");
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    kprintln!("[EXCEPTION] Debug");
    kprintln!("{:#?}", stack_frame);
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    kprintln!("[EXCEPTION] Breakpoint at {:#?}", stack_frame);
}

extern "x86-interrupt" fn overflow_handler(stack_frame: InterruptStackFrame) {
    kprintln!("[EXCEPTION] Overflow");
    kprintln!("{:#?}", stack_frame);
    panic!("Unhandled overflow");
}

extern "x86-interrupt" fn bound_range_handler(stack_frame: InterruptStackFrame) {
    kprintln!("[EXCEPTION] Bound Range Exceeded");
    kprintln!("{:#?}", stack_frame);
    panic!("Unhandled bound range exceeded");
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    kprintln!("[EXCEPTION] Invalid Opcode");
    kprintln!("{:#?}", stack_frame);
    panic!("Unhandled invalid opcode");
}

extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    kprintln!("[EXCEPTION] Device Not Available");
    kprintln!("{:#?}", stack_frame);
    panic!("Unhandled device not available");
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    kprintln!("[EXCEPTION] General Protection Fault");
    kprintln!("  Error Code: {:#x}", error_code);
    kprintln!("{:#?}", stack_frame);
    panic!("Unhandled general protection fault (error code: {:#x})", error_code);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    kprintln!("[EXCEPTION] DOUBLE FAULT");
    kprintln!("  Error Code: {}", error_code);
    kprintln!("{:#?}", stack_frame);
    panic!("Double fault — system cannot recover");
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::{Cr2, Cr3};
    use x86_64::{VirtAddr, PhysAddr};
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let fault_addr = Cr2::read().unwrap();

    // Check if the fault was caused by a write operation
    if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
        let (pml4_frame, _) = Cr3::read();
        let pml4_phys = pml4_frame.start_address().as_u64();
        let phys_mem_offset = crate::memory::r#virtual::phys_mem_offset();

        let pml4_virt = VirtAddr::new(pml4_phys + phys_mem_offset);
        let pml4: &PageTable = unsafe { &*pml4_virt.as_ptr() };

        let pml4_idx = fault_addr.p4_index();
        let pdpt_idx = fault_addr.p3_index();
        let pd_idx = fault_addr.p2_index();
        let pt_idx = fault_addr.p1_index();

        let pml4_entry = &pml4[pml4_idx];
        if !pml4_entry.is_unused() {
            if let Ok(pdpt_frame) = pml4_entry.frame() {
                let pdpt_phys = pdpt_frame.start_address().as_u64();
                let pdpt_virt = VirtAddr::new(pdpt_phys + phys_mem_offset);
                let pdpt: &PageTable = unsafe { &*pdpt_virt.as_ptr() };

                let pdpt_entry = &pdpt[pdpt_idx];
                if !pdpt_entry.is_unused() {
                    if let Ok(pd_frame) = pdpt_entry.frame() {
                        let pd_phys = pd_frame.start_address().as_u64();
                        let pd_virt = VirtAddr::new(pd_phys + phys_mem_offset);
                        let pd: &PageTable = unsafe { &*pd_virt.as_ptr() };

                        let pd_entry = &pd[pd_idx];
                        if !pd_entry.is_unused() {
                            if let Ok(pt_frame) = pd_entry.frame() {
                                let pt_phys = pt_frame.start_address().as_u64();
                                let pt_virt = VirtAddr::new(pt_phys + phys_mem_offset);
                                let pt: &mut PageTable = unsafe { &mut *pt_virt.as_mut_ptr() };

                                let pt_entry = &mut pt[pt_idx];
                                if !pt_entry.is_unused() {
                                    let mut flags = pt_entry.flags();
                                    if flags.contains(PageTableFlags::BIT_9) {
                                        // This is a Copy-on-Write page!
                                        if let Ok(old_frame) = pt_entry.frame() {
                                            let old_phys = old_frame.start_address().as_u64();

                                            // Safely read reference count from AtomicU8 array
                                            use core::sync::atomic::Ordering;
                                            let refs = crate::memory::physical::FRAME_REFS[(old_phys / 4096) as usize].load(Ordering::SeqCst);

                                            if refs == 1 {
                                                // Not shared anymore! Mark as writable directly
                                                flags.remove(PageTableFlags::BIT_9);
                                                flags.insert(PageTableFlags::WRITABLE);
                                                pt_entry.set_addr(PhysAddr::new(old_phys), flags);

                                                // Flush local TLB for this virtual address
                                                x86_64::instructions::tlb::flush(fault_addr);
                                                // Broadcast TLB shootdown to notify other CPU cores
                                                crate::arch::x86_64::smp::shootdown_tlb();
                                                return; // Fault resolved!
                                            } else if refs > 1 {
                                                // Shared page! Allocate a new page frame, copy contents, and map writable
                                                if let Some(new_phys) = crate::memory::physical::allocate_frame() {
                                                    let src_ptr = (old_phys + phys_mem_offset) as *const u8;
                                                    let dest_ptr = (new_phys + phys_mem_offset) as *mut u8;

                                                    unsafe {
                                                        core::ptr::copy_nonoverlapping(src_ptr, dest_ptr, 4096);
                                                    }

                                                    // Decrement old frame's reference count
                                                    crate::memory::physical::decrement_ref(old_phys);

                                                    flags.remove(PageTableFlags::BIT_9);
                                                    flags.insert(PageTableFlags::WRITABLE);
                                                    pt_entry.set_addr(PhysAddr::new(new_phys), flags);

                                                    // Flush local TLB and broadcast shootdown
                                                    x86_64::instructions::tlb::flush(fault_addr);
                                                    crate::arch::x86_64::smp::shootdown_tlb();
                                                    return; // Fault resolved!
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    kprintln!("[EXCEPTION] Unhandled Page Fault");
    kprintln!("  Accessed Address: {:?}", fault_addr);
    kprintln!("  Error Code: {:?}", error_code);
    kprintln!("{:#?}", stack_frame);
    panic!("Unhandled page fault — system cannot recover");
}

// ═══════════════════════════════════════════════════════════════════════
// Hardware Interrupt Handlers
// ═══════════════════════════════════════════════════════════════════════

/// Timer tick counter for basic timekeeping.
static TIMER_TICKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TIMER_TICKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // Update scheduler tick counter
    crate::process::scheduler::tick();

    // Acknowledge the timer interrupt to the Local APIC
    super::apic::lapic_eoi();

    // Trigger rescheduling to enable preemption
    crate::process::scheduler::schedule();
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Read the scancode from the keyboard data port
    let mut port = Port::new(0x60);
    // SAFETY: Port 0x60 is the standard keyboard data port on x86.
    let scancode: u8 = unsafe { port.read() };

    // Translate and buffer the scancode via the keyboard driver
    crate::drivers::keyboard::push_scancode(scancode);

    // SAFETY: Acknowledge the keyboard interrupt to the Local APIC.
    super::apic::lapic_eoi();
}

extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Spurious interrupts do not require an EOI.
}

extern "x86-interrupt" fn ipi_reschedule_handler(_stack_frame: InterruptStackFrame) {
    super::apic::lapic_eoi();
    crate::process::scheduler::schedule();
}

extern "x86-interrupt" fn ipi_halt_handler(_stack_frame: InterruptStackFrame) {
    super::apic::lapic_eoi();
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn ipi_tlb_shootdown_handler(_stack_frame: InterruptStackFrame) {
    super::apic::lapic_eoi();
    x86_64::instructions::tlb::flush_all();
}

extern "x86-interrupt" fn network_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Call e1000 poll handler if initialized
    crate::drivers::net::e1000::handle_interrupt();

    // Acknowledge interrupt to Local APIC
    super::apic::lapic_eoi();
}

/// Returns the number of timer ticks since boot.
pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed)
}

