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

use crate::kprintln;
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

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
    let active_gs = unsafe { x86_64::registers::model_specific::Msr::new(0xC0000101).read() };
    let is_user = stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    let swap_needed = is_user || (active_gs < 0xFFFF800000000000);
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space or if user GS is active
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    kprintln!("[EXCEPTION] General Protection Fault");
    kprintln!("  Error Code: {:#x}", error_code);
    kprintln!("{:#?}", stack_frame);

    if is_user {
        kprintln!(
            "[gpf] Process PID {:?} caused GPF at RIP={:#x} (error_code={:#x}) — terminating",
            crate::process::scheduler::current_pid(),
            stack_frame.instruction_pointer.as_u64(),
            error_code,
        );
        // Terminate the faulting process group with SIGSEGV exit code (139)
        // rather than crashing the entire kernel.
        let _ = crate::syscall::process::sys_exit_group(139);
        // sys_exit_group does not return; but if somehow we continue, halt.
        loop {
            x86_64::instructions::hlt();
        }
    }

    panic!(
        "Unhandled kernel general protection fault (error code: {:#x})",
        error_code
    );
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    let active_gs = unsafe { x86_64::registers::model_specific::Msr::new(0xC0000101).read() };
    let swap_needed = (stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3)
        || (active_gs < 0xFFFF800000000000);
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space or if user GS is active
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    kprintln!("[EXCEPTION] DOUBLE FAULT");
    kprintln!("  Error Code: {}", error_code);
    kprintln!("{:#?}", stack_frame);
    panic!("Double fault — system cannot recover");
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let active_gs = unsafe { x86_64::registers::model_specific::Msr::new(0xC0000101).read() };
    let swap_needed = (stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3)
        || (active_gs < 0xFFFF800000000000);
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space or if user GS is active
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    page_fault_handler_inner(stack_frame, error_code);

    if swap_needed {
        // SAFETY: Swap back to user GS base before returning
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }
}

fn page_fault_handler_inner(stack_frame: InterruptStackFrame, error_code: PageFaultErrorCode) {
    use x86_64::registers::control::{Cr2, Cr3};
    use x86_64::structures::paging::{Page, PageTable, PageTableFlags, PhysFrame, Size4KiB};
    use x86_64::{PhysAddr, VirtAddr};

    macro_rules! kprintln {
        ($fmt:expr $(, $arg:expr)* $(,)?) => {
            if !$fmt.starts_with("[debug pf]") {
                crate::kprintln!($fmt $(, $arg)*);
            }
        };
    }

    let fault_addr = Cr2::read().unwrap();
    let is_user = stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    /*
    crate::kprintln!(
        "[debug pf] RAW Page Fault at vaddr {:#x}, err_code={:#x}, is_user={}",
        fault_addr.as_u64(),
        error_code.bits(),
        is_user
    );
    */

    // Check if the fault was caused by a write operation
    if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
        let (pml4_frame, _) = Cr3::read();
        let pml4_phys = pml4_frame.start_address().as_u64();

        // Upgrade intermediate directory flags to WRITABLE | USER_ACCESSIBLE
        unsafe {
            crate::memory::r#virtual::ensure_directory_permissions(pml4_phys, fault_addr);
        }

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
                                            let idx = (old_phys / 4096) as usize;

                                            use core::sync::atomic::Ordering;
                                            let is_sole_owner = crate::memory::physical::FRAME_REFS
                                                [idx]
                                                .compare_exchange(
                                                    1,
                                                    1,
                                                    Ordering::SeqCst,
                                                    Ordering::SeqCst,
                                                )
                                                .is_ok();

                                            if is_sole_owner {
                                                // Not shared anymore! Mark as writable directly
                                                flags.remove(PageTableFlags::BIT_9);
                                                flags.insert(PageTableFlags::WRITABLE);
                                                pt_entry.set_addr(PhysAddr::new(old_phys), flags);

                                                // Flush local TLB for this virtual address
                                                x86_64::instructions::tlb::flush(fault_addr);
                                                let ref_count = crate::memory::physical::FRAME_REFS
                                                    [idx]
                                                    .load(Ordering::SeqCst);
                                                kprintln!("[debug pf] CoW sole owner resolved at vaddr {:#x}, refcount={}", fault_addr.as_u64(), ref_count);
                                                return; // Fault resolved!
                                            } else {
                                                // Shared page! Allocate a new page frame, copy contents, and map writable
                                                if let Some(new_phys) =
                                                    crate::memory::physical::allocate_frame()
                                                {
                                                    let src_ptr =
                                                        (old_phys + phys_mem_offset) as *const u8;
                                                    let dest_ptr =
                                                        (new_phys + phys_mem_offset) as *mut u8;

                                                    unsafe {
                                                        core::ptr::copy_nonoverlapping(
                                                            src_ptr, dest_ptr, 4096,
                                                        );
                                                    }

                                                    // Decrement old frame's reference count
                                                    crate::memory::physical::decrement_ref(
                                                        old_phys,
                                                    );

                                                    // Re-verify that pt_entry still points to old_phys before writing
                                                    if let Ok(current_frame) = pt_entry.frame() {
                                                        if current_frame.start_address().as_u64()
                                                            == old_phys
                                                        {
                                                            flags.remove(PageTableFlags::BIT_9);
                                                            flags.insert(PageTableFlags::WRITABLE);
                                                            pt_entry.set_addr(
                                                                PhysAddr::new(new_phys),
                                                                flags,
                                                            );

                                                            // Flush local TLB for this virtual address
                                                            x86_64::instructions::tlb::flush(
                                                                fault_addr,
                                                            );
                                                        } else {
                                                            // Another core already handled the page fault, deallocate the new frame
                                                            crate::memory::physical::deallocate_frame(new_phys);
                                                        }
                                                    } else {
                                                        crate::memory::physical::deallocate_frame(
                                                            new_phys,
                                                        );
                                                    }
                                                    let ref_count =
                                                        crate::memory::physical::FRAME_REFS[idx]
                                                            .load(Ordering::SeqCst);
                                                    kprintln!("[debug pf] CoW shared resolved at vaddr {:#x}, refcount of old was {}, now {}", fault_addr.as_u64(), ref_count + 1, ref_count);
                                                    return; // Fault resolved!
                                                }
                                            }
                                        }
                                    } else if flags.contains(PageTableFlags::WRITABLE) {
                                        // Leaf page is already writable, meaning the write fault occurred because
                                        // one of the intermediate directory entries was read-only. We have already
                                        // upgraded them, so the fault is now resolved.
                                        x86_64::instructions::tlb::flush(fault_addr);
                                        // kprintln!("[debug pf] Protection violation already writable resolved at vaddr {:#x}", fault_addr.as_u64());
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if !error_code.contains(x86_64::structures::idt::PageFaultErrorCode::PROTECTION_VIOLATION)
        && fault_addr.as_u64() < 0x0000_8000_0000_0000
    {
        kprintln!("[debug pf] start for vaddr {:#x}", fault_addr.as_u64());
        let resolved = crate::process::scheduler::current_pid()
            .and_then(|pid| {
                kprintln!("[debug pf] pid={}", pid.as_u64());
                crate::process::scheduler::get_task_arc(pid)
            })
            .and_then(|task_arc| {
                let fault_vaddr = fault_addr.as_u64();
                let address_space_arc = {
                    let task = task_arc.lock();
                    task.address_space.clone()
                };
                let (region, page_table_root) = {
                    let addr_space = address_space_arc.lock();

                    // Find if fault_vaddr falls inside any mapped region
                    let region_opt = addr_space
                        .mmap_regions
                        .iter()
                        .find(|region| {
                            fault_vaddr >= region.start
                                && fault_vaddr < region.start + region.len as u64
                        })
                        .cloned();
                    let pt_root = addr_space.page_table_root;
                    region_opt.map(|r| (r, pt_root))
                }?;

                let page_vaddr = fault_vaddr & !4095;
                let page_offset = page_vaddr - region.start;

                let prot = region.prot;
                let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
                if (prot & 2) != 0 {
                    page_flags |= PageTableFlags::WRITABLE;
                }
                if (prot & 5) == 0 {
                    page_flags |= PageTableFlags::NO_EXECUTE;
                }

                let is_shared = region.is_shared;

                kprintln!("[debug pf] checking inode for vaddr={:#x}", page_vaddr);
                let (phys, do_cow) = match region.inode {
                    Some(ref inode) => {
                        kprintln!("[debug pf] file-backed page");
                        let file_offset = region.offset + page_offset;
                        match crate::memory::page_cache::get_or_create_page(inode, file_offset) {
                            Ok(p) => {
                                let cow = !is_shared && (prot & 2) != 0;
                                (p, cow)
                            }
                            Err(_) => {
                                return Some(Err((-5, page_vaddr))); // EIO
                            }
                        }
                    }
                    None => {
                        kprintln!("[debug pf] anon page: calling allocate_frame");
                        match crate::memory::physical::allocate_frame() {
                            Some(p) => {
                                kprintln!("[debug pf] allocated frame {:#x}", p);
                                let dest =
                                    (p + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
                                unsafe {
                                    core::ptr::write_bytes(dest, 0, 4096);
                                }
                                kprintln!("[debug pf] zeroed frame");
                                (p, false)
                            }
                            None => {
                                kprintln!("[debug pf] allocate_frame returned None (ENOMEM)!");
                                return Some(Err((-12, page_vaddr))); // ENOMEM
                            }
                        }
                    }
                };

                let actual_flags = if do_cow {
                    let mut flags = page_flags;
                    flags.remove(PageTableFlags::WRITABLE);
                    flags.insert(PageTableFlags::BIT_9);
                    kprintln!(
                        "[debug pf] phys={:#x}, do_cow = true, flags before={:?}, after={:?}",
                        phys,
                        page_flags,
                        flags
                    );
                    flags
                } else {
                    kprintln!(
                        "[debug pf] phys={:#x}, do_cow = false, flags={:?}",
                        phys,
                        page_flags
                    );
                    page_flags
                };

                let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_vaddr));
                let frame = PhysFrame::containing_address(PhysAddr::new(phys));

                unsafe {
                    if region.inode.is_some() {
                        kprintln!("[debug pf] incrementing ref count");
                        crate::memory::physical::increment_ref(phys);
                    }

                    kprintln!("[debug pf] unmapping page if present");
                    if let Ok(old_phys) = crate::memory::r#virtual::unmap_user_page_no_shootdown(
                        page_table_root,
                        page,
                    ) {
                        kprintln!("[debug pf] old page at {:#x} deallocating", old_phys);
                        crate::memory::physical::deallocate_frame(old_phys);
                    }

                    kprintln!("[debug pf] ensure directory permissions");
                    crate::memory::r#virtual::ensure_directory_permissions(
                        page_table_root,
                        VirtAddr::new(page_vaddr),
                    );

                    kprintln!("[debug pf] mapping user page");
                    if let Err(_e) = crate::memory::r#virtual::map_user_page_no_shootdown(
                        page_table_root,
                        page,
                        frame,
                        actual_flags,
                    ) {
                        kprintln!("[debug pf] map_user_page failed!");
                        if region.inode.is_some() {
                            crate::memory::physical::decrement_ref(phys);
                        } else {
                            crate::memory::physical::deallocate_frame(phys);
                        }
                        return Some(Err((-12, page_vaddr)));
                    }

                    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
                    let active_cr3 = cr3_frame.start_address().as_u64();
                    if active_cr3 != page_table_root && active_cr3 != 0 {
                        let _ = crate::memory::r#virtual::map_user_page_no_shootdown(
                            active_cr3,
                            page,
                            frame,
                            actual_flags,
                        );
                    }
                }

                kprintln!("[debug pf] flushing TLB");
                x86_64::instructions::tlb::flush(VirtAddr::new(page_vaddr));
                kprintln!("[debug pf] done successfully");
                Some(Ok(()))
            });

        if let Some(res) = resolved {
            match res {
                Ok(()) => {
                    // kprintln!("[debug pf] Resolved Page Fault at vaddr {:#x}", fault_addr.as_u64());
                    return; // Page fault resolved successfully!
                }
                Err((errno, page_vaddr)) => {
                    crate::kprintln!(
                        "[demand_page] Error resolving page fault at vaddr {:#x}: errno {}",
                        page_vaddr,
                        errno
                    );
                }
            }
        }
    }

    kprintln!("[EXCEPTION] Unhandled Page Fault");
    kprintln!("  Accessed Address: {:#x}", fault_addr.as_u64());
    kprintln!("  Error Code bits: {:#x}", error_code.bits());
    kprintln!(
        "  RIP: {:#x}, CS: {:#x}, RFLAGS: {:#x}, RSP: {:#x}, SS: {:#x}",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.code_segment.0,
        stack_frame.cpu_flags,
        stack_frame.stack_pointer.as_u64(),
        stack_frame.stack_segment.0
    );
    crate::memory::r#virtual::debug_dump_mapping(fault_addr.as_u64());

    if is_user {
        kprintln!(
            "[page_fault] Process PID {:?} caused unhandled page fault at {:#x} (RIP={:#x}) — terminating task",
            crate::process::scheduler::current_pid(),
            fault_addr.as_u64(),
            stack_frame.instruction_pointer.as_u64()
        );
        let _ = crate::syscall::process::sys_exit_group(139);
    }

    panic!("Unhandled page fault — system cannot recover");
}

// ═══════════════════════════════════════════════════════════════════════
// Hardware Interrupt Handlers
// ═══════════════════════════════════════════════════════════════════════

/// Timer tick counter for basic timekeeping.
static TIMER_TICKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

extern "x86-interrupt" fn timer_interrupt_handler(stack_frame: InterruptStackFrame) {
    let swap_needed = stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    let _ticks = TIMER_TICKS.fetch_add(1, core::sync::atomic::Ordering::Release) + 1;
    /*
    if ticks % 10 == 0 {
        let current_pid = crate::process::scheduler::current_pid()
            .map(|p| p.as_u64())
            .unwrap_or(0);
        crate::kprintln!(
            "[timer] Tick {}, PID={}, RIP={:#x}, RSP={:#x}",
            ticks,
            current_pid,
            stack_frame.instruction_pointer.as_u64(),
            stack_frame.stack_pointer.as_u64()
        );
    }
    */

    // Update scheduler tick counter
    crate::process::scheduler::tick();

    // Check sleep timeouts and active timerfds
    crate::fs::timerfd::check_timers();
    crate::fs::epoll::check_sleep_timeouts();

    // Acknowledge the timer interrupt to the Local APIC
    super::apic::lapic_eoi();

    // Trigger rescheduling to enable preemption
    crate::process::scheduler::schedule();

    if swap_needed {
        // SAFETY: Swap back to user GS base before returning
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(stack_frame: InterruptStackFrame) {
    let swap_needed = stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    use x86_64::instructions::port::Port;

    // Read the scancode from the keyboard data port
    let mut port = Port::new(0x60);
    // SAFETY: Port 0x60 is the standard keyboard data port on x86.
    let scancode: u8 = unsafe { port.read() };

    // Translate and buffer the scancode via the keyboard driver
    crate::drivers::keyboard::push_scancode(scancode);

    // SAFETY: Acknowledge the keyboard interrupt to the Local APIC.
    super::apic::lapic_eoi();

    if swap_needed {
        // SAFETY: Swap back to user GS base before returning
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }
}

extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Spurious interrupts do not require an EOI.
}

extern "x86-interrupt" fn ipi_reschedule_handler(stack_frame: InterruptStackFrame) {
    let swap_needed = stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    super::apic::lapic_eoi();
    crate::process::scheduler::schedule();

    if swap_needed {
        // SAFETY: Swap back to user GS base before returning
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }
}

extern "x86-interrupt" fn ipi_halt_handler(_stack_frame: InterruptStackFrame) {
    super::apic::lapic_eoi();
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn ipi_tlb_shootdown_handler(stack_frame: InterruptStackFrame) {
    let swap_needed = stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    super::apic::lapic_eoi();
    x86_64::instructions::tlb::flush_all();
    crate::arch::x86_64::smp::tlb_shootdown_ack();

    if swap_needed {
        // SAFETY: Swap back to user GS base before returning
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }
}

extern "x86-interrupt" fn network_interrupt_handler(stack_frame: InterruptStackFrame) {
    let swap_needed = stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    if swap_needed {
        // SAFETY: Swap to kernel GS base if entering from user space
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }

    // Call e1000 poll handler if initialized
    crate::drivers::net::e1000::handle_interrupt();

    // Acknowledge interrupt to Local APIC
    super::apic::lapic_eoi();

    if swap_needed {
        // SAFETY: Swap back to user GS base before returning
        unsafe {
            core::arch::asm!("swapgs", options(nostack, preserves_flags));
        }
    }
}

/// Returns the number of timer ticks since boot.
pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(core::sync::atomic::Ordering::Acquire)
}
