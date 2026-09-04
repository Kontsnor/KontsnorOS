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

//! Process and thread lifecycle logic: creation, termination, and state wrappers.

use alloc::sync::Arc;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use super::context::{self, CpuContext};
use super::elf;
use super::pid::{self, Pid};
use super::scheduler::{self, SCHEDULER};
use super::task::Task;

/// Spawn a new kernel thread.
pub fn spawn_kernel_thread(name: alloc::string::String, entry_point: fn()) -> Pid {
    let pid = pid::allocate();

    // Allocate kernel stack (32 KiB)
    let layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
    // SAFETY: Layout is valid (non-zero size, power of 2 alignment) and memory is checked for allocation.
    let stack_base = unsafe { alloc::alloc::alloc(layout) } as u64;

    // Create new page table root (use kernel page table root, which is current CR3)
    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    let cr3_val = cr3_frame.start_address().as_u64();

    let mut task = Task::new(pid, name, cr3_val);
    task.kernel_stack_base = stack_base;
    task.kernel_stack_size = 32768;

    // Prepare the initial stack: R12 will hold the entry_point, and RIP will point to thread_trampoline!
    let stack_top = stack_base + 32768;
    let stack_top_aligned = stack_top & !0xF;

    let mut context = CpuContext::new(
        thread_trampoline as *const () as u64,
        stack_top_aligned,
        cr3_val,
    );
    context.r12 = entry_point as *const () as u64;
    task.context = context;

    // Add to scheduler
    scheduler::add_task(task);

    pid
}

#[unsafe(naked)]
extern "C" fn thread_trampoline() -> ! {
    // SAFETY: Naked assembly stub serving as a trampoline entry point for kernel threads.
    core::arch::naked_asm!(
        "call {}",      // Release scheduler lock and enable interrupts
        "call r12",     // Call the entry_point (stored in r12)
        "mov rdi, 0",   // Set first argument (exit status) to 0
        "call {}",      // Call exit_current_thread
        sym crate::process::scheduler::scheduler_unlock_after_switch,
        sym exit_current_thread,
    );
}

/// Spawn a new Ring 3 user process from ELF data.
pub fn spawn_user_process(name: alloc::string::String, elf_data: &[u8]) -> Pid {
    spawn_user_process_with_pid(name, elf_data, pid::allocate())
}

/// Spawn a new Ring 3 user process from ELF data with a specific PID.
pub fn spawn_user_process_with_pid(name: alloc::string::String, elf_data: &[u8], pid: Pid) -> Pid {
    let elf_info =
        elf::parse_elf(elf_data, elf_data.len()).expect("Failed to parse user process ELF");

    // Create new user PML4 page table (clones kernel mappings)
    let page_table_root = crate::memory::r#virtual::create_user_page_table()
        .expect("Failed to create user page table");

    // Map loadable segments
    for segment in &elf_info.segments {
        let vaddr = segment.vaddr;
        let mem_size = segment.mem_size;
        let file_offset = segment.file_offset;
        let file_size = segment.file_size;

        if mem_size == 0 {
            continue;
        }

        let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr));
        let end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(vaddr + mem_size - 1));

        // Map pages
        for page in Page::range_inclusive(start_page, end_page) {
            let phys_addr =
                crate::memory::physical::allocate_frame().expect("OOM allocating segment frame");
            let frame = PhysFrame::containing_address(PhysAddr::new(phys_addr));

            // Build flags: PRESENT, USER_ACCESSIBLE
            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if segment.flags.write {
                flags |= PageTableFlags::WRITABLE;
            }
            if !segment.flags.execute {
                flags |= PageTableFlags::NO_EXECUTE;
            }

            // Map page in user page table
            // SAFETY: page_table_root points to a valid page directory table, the page and frame are properly aligned.
            unsafe {
                crate::memory::r#virtual::map_user_page_no_shootdown(
                    page_table_root,
                    page,
                    frame,
                    flags,
                )
                .expect("Failed to map user segment page");
            }

            // Copy data from ELF buffer to physical frame via kernel offset
            let dest_ptr = (phys_addr + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            // SAFETY: dest_ptr is a valid direct physical-to-virtual mapped address inside the kernel heap.
            let dest_slice = unsafe { core::slice::from_raw_parts_mut(dest_ptr, 4096) };
            dest_slice.fill(0);

            let page_start = page.start_address().as_u64();

            let segment_start_in_page = if page_start < vaddr {
                vaddr - page_start
            } else {
                0
            };

            let page_offset_in_segment = if page_start > vaddr {
                page_start - vaddr
            } else {
                0
            };

            if page_offset_in_segment < file_size {
                let copy_len = core::cmp::min(
                    4096 - segment_start_in_page,
                    file_size - page_offset_in_segment,
                );
                let src_start = (file_offset + page_offset_in_segment) as usize;
                let src_end = src_start + copy_len as usize;
                let dest_start = segment_start_in_page as usize;
                let dest_end = dest_start + copy_len as usize;

                dest_slice[dest_start..dest_end].copy_from_slice(&elf_data[src_start..src_end]);
            }
        }
    }

    // Allocate and map user stack (8MB below USER_STACK_TOP)
    let stack_size: u64 = 8 * 1024 * 1024;
    let stack_bottom = elf::USER_STACK_TOP - stack_size;

    // Construct System V ABI stack
    let default_argv = [name.clone()];
    let default_envp: [alloc::string::String; 0] = [];
    let init_stack = elf::construct_user_stack(
        &default_argv,
        &default_envp,
        elf_info.entry_point,
        elf_info.phdr,
        elf_info.phnum,
        elf_info.phent,
        0, // interpreter_base is 0 for statically linked spawned user processes
    )
    .expect("Failed to construct user stack");

    // Allocate and map physical frames for the initial stack data
    let num_init_pages = init_stack.stack_buf.len() / 4096;
    for i in 0..num_init_pages {
        let page_vaddr = init_stack.base_vaddr + (i as u64 * 4096);
        let phys_addr =
            crate::memory::physical::allocate_frame().expect("OOM allocating user stack frame");
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_vaddr));
        let frame = PhysFrame::containing_address(PhysAddr::new(phys_addr));

        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;

        // SAFETY: page_table_root points to a valid PML4 page table structure and frame/page parameters are valid.
        unsafe {
            crate::memory::r#virtual::map_user_page_no_shootdown(
                page_table_root,
                page,
                frame,
                flags,
            )
            .expect("Failed to map user stack page");

            let dest = (phys_addr + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
            core::ptr::copy_nonoverlapping(
                init_stack.stack_buf[i * 4096..(i + 1) * 4096].as_ptr(),
                dest,
                4096,
            );
        }
    }

    let user_sp = init_stack.user_sp;

    // Calculate initial program break (brk) dynamically from loaded ELF segment boundaries
    let mut max_vaddr = 0;
    for segment in &elf_info.segments {
        let end = segment.vaddr + segment.mem_size;
        if end > max_vaddr {
            max_vaddr = end;
        }
    }
    let initial_brk = (max_vaddr + 4095) & !4095;

    // Create new process TCB
    let mut task = Task::new(pid, name, page_table_root);
    task.address_space.lock().brk = initial_brk;

    // Allocate kernel stack (32 KiB)
    let kernel_stack_layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
    // SAFETY: Layout parameters are non-zero size and power-of-two alignment.
    let kernel_stack_base = unsafe { alloc::alloc::alloc(kernel_stack_layout) } as u64;
    task.kernel_stack_base = kernel_stack_base;
    task.kernel_stack_size = 32768;

    // Set up CpuContext to start at user_process_trampoline
    let kernel_stack_top = kernel_stack_base + 32768;
    let kernel_stack_top_aligned = kernel_stack_top & !0xF;

    let mut context = CpuContext::new(
        user_process_trampoline as *const () as u64,
        kernel_stack_top_aligned,
        page_table_root,
    );

    // Store user program parameters in callee-saved registers for the trampoline
    context.r12 = elf_info.entry_point;
    context.r13 = user_sp; // User stack pointer (16-byte aligned)
    context.r14 = page_table_root;
    context.r14 = page_table_root;
    context.r15 = (crate::arch::x86_64::gdt::user_code_selector().0 | 3) as u64;
    context.rbx = (crate::arch::x86_64::gdt::user_data_selector().0 | 3) as u64;

    task.context = context;

    // Add to scheduler
    scheduler::add_task(task);

    pid
}

#[unsafe(naked)]
extern "C" fn user_process_trampoline() -> ! {
    // SAFETY: Naked assembly stub serving as a trampoline entry point for Ring 3 user processes.
    core::arch::naked_asm!(
        "call {}",      // Release scheduler lock and enable interrupts
        "mov rdi, r12", // Set entry_point as 1st argument (rdi)
        "mov rsi, r13", // Set user_stack as 2nd argument (rsi)
        "mov rdx, r14", // Set page_table as 3rd argument (rdx)
        "mov rcx, r15", // Set user_code_selector as 4th argument (rcx)
        "mov r8, rbx",  // Set user_data_selector as 5th argument (r8)
        "jmp {}",       // Jump to enter_user_mode (never returns)
        sym crate::process::scheduler::scheduler_unlock_after_switch,
        sym context::enter_user_mode,
    );
}

/// Returns the virtual address of `user_process_trampoline`.
///
/// Used by `sys_fork` to set up the child task's initial RIP.
pub fn user_process_trampoline_addr() -> u64 {
    user_process_trampoline as *const () as u64
}

/// Safely clean up the current task's address space while interrupts are enabled to prevent deadlocks in shootdown_tlb.
pub fn cleanup_address_space() {
    let current_pid = match scheduler::current_pid() {
        Some(pid) => pid,
        None => return,
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return,
    };

    // 1. Switch CR3 to kernel PML4 first to prevent page table use-after-free
    let kernel_pml4 = crate::memory::r#virtual::kernel_pml4_phys();
    unsafe {
        use x86_64::registers::control::{Cr3, Cr3Flags};
        use x86_64::structures::paging::PhysFrame;
        use x86_64::PhysAddr;
        let (current_cr3_frame, _) = Cr3::read();
        let current_cr3 = current_cr3_frame.start_address().as_u64();
        if current_cr3 != kernel_pml4 {
            Cr3::write(
                PhysFrame::containing_address(PhysAddr::new(kernel_pml4)),
                Cr3Flags::empty(),
            );
        }
    }

    // 2. Replace task's address space with kernel-only address space, freeing the old one.
    // Since interrupts are enabled and we do not hold the SCHEDULER lock, TLB shootdown will not deadlock!
    let old_address_space = {
        let mut task = task_arc.lock();
        let old = task.address_space.clone();
        task.address_space = Arc::new(spin::Mutex::new(crate::process::task::AddressSpace {
            page_table_root: kernel_pml4,
            brk: 0,
            mmap_bump: 0,
            mmap_regions: alloc::vec::Vec::new(),
        }));
        // Update context CR3 to 0 (kernel task) or kernel PML4
        task.context.cr3 = 0;
        old
    };
    drop(old_address_space);
}

/// Exits the currently running task.
pub fn exit_current_thread(exit_code: i32) -> ! {
    let mut clear_ctid = None;
    let mut current_tgid = 0;
    let mut robust_head = 0;
    let current_pid_opt = scheduler::current_pid();
    if let Some(current_pid) = current_pid_opt {
        if let Some(task_arc) = scheduler::get_task_arc(current_pid) {
            let mut task = task_arc.lock();
            clear_ctid = task.clear_child_tid;
            current_tgid = task.tgid.as_u64();
            robust_head = task.robust_list_head;
            let fd_table = task.fd_table.clone();
            if Arc::strong_count(&fd_table) == 1 {
                fd_table.lock().entries.clear();
            }
        }
    }

    // Process robust futex list before exit to unblock waiters on robust mutexes
    if robust_head != 0 {
        if crate::syscall::validation::validate_user_ptr(robust_head as *const u8, 24) {
            // SAFETY: Memory is validated and readable.
            let next_ptr = unsafe { (robust_head as *const u64).read_volatile() };
            let futex_offset = unsafe { ((robust_head + 8) as *const i64).read_volatile() };
            let pending_ptr = unsafe { ((robust_head + 16) as *const u64).read_volatile() };

            let tid_val = current_pid_opt.map(|p| p.as_u64() as u32).unwrap_or(0);

            run_with_scheduler_lock(|sched| {
                let handle_entry =
                    |entry: u64, sched: &mut crate::process::scheduler::Scheduler| {
                        if entry == 0 || entry == robust_head {
                            return;
                        }
                        let futex_addr = (entry as i64).wrapping_add(futex_offset) as u64;
                        if crate::syscall::validation::validate_user_ptr_write(
                            futex_addr as *mut u8,
                            4,
                        )
                        .is_ok()
                        {
                            let futex_ptr = futex_addr as *mut u32;
                            // SAFETY: Validated writable user pointer.
                            let val = unsafe { futex_ptr.read_volatile() };
                            if (val & 0x3fff_ffff) == tid_val {
                                let new_val = (val & 0x8000_0000) | 0x4000_0000; // FUTEX_OWNER_DIED
                                                                                 // SAFETY: Validated user memory pointer.
                                unsafe {
                                    futex_ptr.write_volatile(new_val);
                                }
                                crate::syscall::process::futex::futex_wake_locked(
                                    current_tgid,
                                    futex_addr,
                                    1,
                                    0xffff_ffff,
                                    sched,
                                );
                            }
                        }
                    };

                if pending_ptr != 0 {
                    handle_entry(pending_ptr, sched);
                }

                let mut curr = next_ptr;
                let mut count = 0;
                while curr != 0 && curr != robust_head && count < 1000 {
                    if crate::syscall::validation::validate_user_ptr(curr as *const u8, 8) {
                        handle_entry(curr, sched);
                        // SAFETY: Validated user memory pointer.
                        curr = unsafe { (curr as *const u64).read_volatile() };
                        count += 1;
                    } else {
                        break;
                    }
                }
            });
        }
    }

    if let Some(ctid) = clear_ctid {
        if crate::syscall::validation::validate_user_ptr_write(ctid as *mut u8, 4).is_ok() {
            // SAFETY: validate_user_ptr_write verifies pointer lies in user memory and is writable
            unsafe {
                (ctid as *mut u32).write_volatile(0);
            }
        }
        run_with_scheduler_lock(|sched| {
            crate::syscall::process::futex::futex_wake_locked(
                current_tgid,
                ctid,
                i32::MAX,
                0xffffffff,
                sched,
            );
        });
    }

    // Drain stale futex registrations for this task before cleaning up address space
    if let Some(current_pid) = scheduler::current_pid() {
        run_with_scheduler_lock(|sched| {
            crate::syscall::process::futex::futex_drain_pid_locked(current_pid, sched);
        });
    }

    // Safely free the address space and run TLB shootdown before disabling interrupts
    cleanup_address_space();

    x86_64::instructions::interrupts::disable();
    let fds = if let Some(current_pid) = scheduler::current_pid() {
        if let Some(ref mut scheduler) = *SCHEDULER.lock() {
            scheduler.exit_task(current_pid, exit_code)
        } else {
            alloc::vec::Vec::new()
        }
    } else {
        alloc::vec::Vec::new()
    };
    drop(fds);

    scheduler::schedule();

    // If there is absolutely no other task left (should not happen due to idle task)
    loop {
        x86_64::instructions::hlt();
    }
}

/// Block a task.
pub fn block_task(pid: Pid) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(ref mut scheduler) = *SCHEDULER.lock() {
            scheduler.block_task(pid);
        }
    });
}

/// Wake up a blocked task.
pub fn wake_task(pid: Pid) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(mut sched_lock) = SCHEDULER.try_lock() {
            if let Some(ref mut scheduler) = *sched_lock {
                scheduler.wake_task(pid)
            } else {
                false
            }
        } else {
            let apic_id = crate::arch::x86_64::smp::current_lapic_id() as u32;
            if SCHEDULER.holding_cpu_id() == apic_id {
                // SAFETY: The current CPU already holds SCHEDULER exclusively
                unsafe {
                    if let Some(ref mut scheduler) = *SCHEDULER.get_mut_unchecked() {
                        scheduler.wake_task(pid)
                    } else {
                        false
                    }
                }
            } else if let Some(ref mut scheduler) = *SCHEDULER.lock() {
                scheduler.wake_task(pid)
            } else {
                false
            }
        }
    })
}

/// Run a closure with the scheduler lock held.
pub fn run_with_scheduler_lock<R, F>(f: F) -> R
where
    F: FnOnce(&mut crate::process::scheduler::Scheduler) -> R,
{
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(mut sched_lock) = SCHEDULER.try_lock() {
            if let Some(ref mut scheduler) = *sched_lock {
                f(scheduler)
            } else {
                panic!("run_with_scheduler_lock: Scheduler not initialized");
            }
        } else {
            let apic_id = crate::arch::x86_64::smp::current_lapic_id() as u32;
            if SCHEDULER.holding_cpu_id() == apic_id {
                // SAFETY: The current CPU already holds SCHEDULER exclusively
                unsafe {
                    if let Some(ref mut scheduler) = *SCHEDULER.get_mut_unchecked() {
                        f(scheduler)
                    } else {
                        panic!("run_with_scheduler_lock: Scheduler not initialized");
                    }
                }
            } else if let Some(ref mut scheduler) = *SCHEDULER.lock() {
                f(scheduler)
            } else {
                panic!("run_with_scheduler_lock: Scheduler not initialized");
            }
        }
    })
}
