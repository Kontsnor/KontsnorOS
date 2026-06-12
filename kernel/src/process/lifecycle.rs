//! Process and thread lifecycle logic: creation, termination, and state wrappers.

use crate::kprintln;
use alloc::sync::Arc;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use super::context::{self, CpuContext};
use super::elf;
use super::pid::{self, Pid};
use super::scheduler::{self, SCHEDULER, TASKS};
use super::task::{Priority, Task, TaskState};

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
    unsafe {
        core::arch::naked_asm!(
            "sti",          // Enable interrupts since context switch disabled them
            "call r12",     // Call the entry_point (stored in r12)
            "mov rdi, 0",   // Set first argument (exit status) to 0
            "call {}",      // Call exit_current_thread
            sym exit_current_thread,
        );
    }
}

/// Spawn a new Ring 3 user process from ELF data.
pub fn spawn_user_process(name: alloc::string::String, elf_data: &[u8]) -> Pid {
    spawn_user_process_with_pid(name, elf_data, pid::allocate())
}

/// Spawn a new Ring 3 user process from ELF data with a specific PID.
pub fn spawn_user_process_with_pid(name: alloc::string::String, elf_data: &[u8], pid: Pid) -> Pid {
    let elf_info = elf::parse_elf(elf_data).expect("Failed to parse user process ELF");

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

    // Allocate and map user stack (64KB below USER_STACK_TOP)
    let stack_size = 64 * 1024;
    let stack_bottom = elf::USER_STACK_TOP - stack_size;
    let stack_start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(stack_bottom));
    let stack_end_page =
        Page::<Size4KiB>::containing_address(VirtAddr::new(elf::USER_STACK_TOP - 1));

    let mut highest_stack_phys = 0;
    for page in Page::range_inclusive(stack_start_page, stack_end_page) {
        let phys_addr =
            crate::memory::physical::allocate_frame().expect("OOM allocating user stack frame");
        if page.start_address().as_u64() == elf::USER_STACK_TOP - 4096 {
            highest_stack_phys = phys_addr;
        }
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
        }
    }

    // Construct System V ABI stack
    let default_argv = [name.clone()];
    let default_envp: [alloc::string::String; 0] = [];
    let user_sp = elf::construct_user_stack(
        &default_argv,
        &default_envp,
        highest_stack_phys,
        elf_info.entry_point,
        elf_info.phdr,
        elf_info.phnum,
        elf_info.phent,
        0, // interpreter_base is 0 for statically linked spawned user processes
    )
    .expect("Failed to construct user stack");

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
    task.brk = initial_brk;

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
    unsafe {
        core::arch::naked_asm!(
            "mov rdi, r12", // Set entry_point as 1st argument (rdi)
            "mov rsi, r13", // Set user_stack as 2nd argument (rsi)
            "mov rdx, r14", // Set page_table as 3rd argument (rdx)
            "mov rcx, r15", // Set user_code_selector as 4th argument (rcx)
            "mov r8, rbx",  // Set user_data_selector as 5th argument (r8)
            "jmp {}",       // Jump to enter_user_mode (never returns)
            sym context::enter_user_mode,
        );
    }
}

/// Returns the virtual address of `user_process_trampoline`.
///
/// Used by `sys_fork` to set up the child task's initial RIP.
pub fn user_process_trampoline_addr() -> u64 {
    user_process_trampoline as *const () as u64
}

/// Exits the currently running task.
pub fn exit_current_thread(exit_code: i32) -> ! {
    x86_64::instructions::interrupts::disable();
    if let Some(current_pid) = scheduler::current_pid() {
        if let Some(ref mut scheduler) = *SCHEDULER.lock() {
            scheduler.exit_task(current_pid, exit_code);
        }
    }

    scheduler::schedule();

    // If there is absolutely no other task left (should not happen due to idle task)
    loop {
        x86_64::instructions::hlt();
    }
}

/// Block a task.
pub fn block_task(pid: Pid) {
    let idx = pid.as_u64() as usize;
    let tasks = TASKS.read();
    if let Some(Some(task_arc)) = tasks.get(idx) {
        task_arc.lock().state = TaskState::Blocked;
    }
}

/// Wake up a blocked task.
pub fn wake_task(pid: Pid) {
    let idx = pid.as_u64() as usize;
    let tasks = TASKS.read();
    if let Some(Some(task_arc)) = tasks.get(idx) {
        let mut task = task_arc.lock();
        if task.state == TaskState::Blocked {
            task.state = TaskState::Ready;
            if !task.in_queue {
                if let Some(ref mut scheduler) = *SCHEDULER.lock() {
                    let priority = task.priority as usize;
                    scheduler.queues[priority].push_back(pid);
                    task.in_queue = true;
                }
            }
        }
    }
}
