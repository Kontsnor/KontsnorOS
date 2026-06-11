//! Process management subsystem for KontsnorOS.
//!
//! This module provides Unix-compatible process management including:
//! - Task/Thread Control Blocks
//! - PID allocation
//! - Process scheduling (multi-level feedback queue)
//! - Context switching

use crate::kprintln;
use x86_64::structures::paging::{Page, PhysFrame, PageTableFlags, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};
pub mod context;
pub mod elf;
pub mod fd;
pub mod pid;
pub mod scheduler;
pub mod task;
pub mod shell_elf;
pub mod hello_elf;
pub mod net_test_elf;

/// Spawn a new kernel thread.
pub fn spawn_kernel_thread(name: alloc::string::String, entry_point: fn()) -> pid::Pid {
    let pid = pid::allocate();
    
    // Allocate kernel stack (32 KiB)
    let layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
    let stack_base = unsafe { alloc::alloc::alloc(layout) } as u64;
    
    // Create new page table root (use kernel page table root, which is current CR3)
    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    let cr3_val = cr3_frame.start_address().as_u64();
    
    let mut task = task::Task::new(pid, name, cr3_val);
    task.kernel_stack_base = stack_base;
    task.kernel_stack_size = 32768;
    
    // Prepare the initial stack: R12 will hold the entry_point, and RIP will point to thread_trampoline!
    let stack_top = stack_base + 32768;
    let stack_top_aligned = stack_top & !0xF;
    
    let mut context = context::CpuContext::new(thread_trampoline as *const () as u64, stack_top_aligned, cr3_val);
    context.r12 = entry_point as *const () as u64;
    task.context = context;
    
    // Add to scheduler
    scheduler::add_task(task);
    
    pid
}

#[unsafe(naked)]
extern "C" fn thread_trampoline() -> ! {
    core::arch::naked_asm!(
        "sti",          // Enable interrupts since context switch disabled them
        "call r12",     // Call the entry_point (stored in r12)
        "mov rdi, 0",   // Set first argument (exit status) to 0
        "call {}",      // Call exit_current_thread
        sym scheduler::exit_current_thread,
    );
}

/// Initialize the process management subsystem.
pub fn init() {
    pid::init();
    scheduler::init();
    
    // Register the early boot thread as a real running task (PID 1, "main")
    let pid = pid::allocate(); // Should allocate 1
    let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
    let cr3_val = cr3_frame.start_address().as_u64();
    
    let main_task = task::Task::new(pid, alloc::string::String::from("main"), cr3_val);
    
    // Set the boot thread as active in the scheduler
    scheduler::set_bootstrap_thread(main_task);
    
    kprintln!("[process] Process subsystem ready. Bootstrap thread registered as PID {}.", pid);
}

/// Spawn a new Ring 3 user process from ELF data.
pub fn spawn_user_process(name: alloc::string::String, elf_data: &[u8]) -> pid::Pid {
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
            let phys_addr = crate::memory::physical::allocate_frame()
                .expect("OOM allocating segment frame");
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
            unsafe {
                crate::memory::r#virtual::map_user_page_no_shootdown(page_table_root, page, frame, flags)
                    .expect("Failed to map user segment page");
            }
            
            // Copy data from ELF buffer to physical frame via kernel offset
            let dest_ptr = (phys_addr + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
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
                let copy_len = core::cmp::min(4096 - segment_start_in_page, file_size - page_offset_in_segment);
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
    let stack_end_page = Page::<Size4KiB>::containing_address(VirtAddr::new(elf::USER_STACK_TOP - 1));
    
    let mut highest_stack_phys = 0;
    for page in Page::range_inclusive(stack_start_page, stack_end_page) {
        let phys_addr = crate::memory::physical::allocate_frame()
            .expect("OOM allocating user stack frame");
        if page.start_address().as_u64() == elf::USER_STACK_TOP - 4096 {
            highest_stack_phys = phys_addr;
        }
        let frame = PhysFrame::containing_address(PhysAddr::new(phys_addr));
        
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        
        unsafe {
            crate::memory::r#virtual::map_user_page_no_shootdown(page_table_root, page, frame, flags)
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
    let pid = pid::allocate();
    let mut task = task::Task::new(pid, name, page_table_root);
    task.brk = initial_brk;
    
    // Allocate kernel stack (32 KiB)
    let kernel_stack_layout = alloc::alloc::Layout::from_size_align(32768, 16).unwrap();
    let kernel_stack_base = unsafe { alloc::alloc::alloc(kernel_stack_layout) } as u64;
    task.kernel_stack_base = kernel_stack_base;
    task.kernel_stack_size = 32768;
    
    // Set up CpuContext to start at user_process_trampoline
    let kernel_stack_top = kernel_stack_base + 32768;
    let kernel_stack_top_aligned = kernel_stack_top & !0xF;
    
    let mut context = context::CpuContext::new(
        user_process_trampoline as *const () as u64,
        kernel_stack_top_aligned,
        page_table_root
    );
    
    // Store user program parameters in callee-saved registers for the trampoline
    context.r12 = elf_info.entry_point;
    context.r13 = user_sp; // User stack pointer (16-byte aligned)
    context.r14 = page_table_root;
    
    task.context = context;
    
    // Add to scheduler
    scheduler::add_task(task);
    
    pid
}

#[unsafe(naked)]
extern "C" fn user_process_trampoline() -> ! {
    core::arch::naked_asm!(
        "mov rdi, r12", // Set entry_point as 1st argument (rdi)
        "mov rsi, r13", // Set user_stack as 2nd argument (rsi)
        "mov rdx, r14", // Set page_table as 3rd argument (rdx)
        "jmp {}",       // Jump to enter_user_mode (never returns)
        sym context::enter_user_mode,
    );
}

/// Returns the virtual address of `user_process_trampoline`.
///
/// Used by `sys_fork` to set up the child task's initial RIP.
pub fn user_process_trampoline_addr() -> u64 {
    user_process_trampoline as *const () as u64
}

/// Create a statically embedded minimal x86_64 ELF binary that runs in user space.
///
/// This program executes:
/// 1. getpid() system call (vector 39)
/// 2. exit(pid) system call (vector 60) with the pid as exit status
pub fn create_demo_user_elf() -> &'static [u8] {
    &[
        // ── ELF64 Header ─────────────────────────────────────────────
        0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00, // e_ident[0..8]
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_ident[8..16]
        0x02, 0x00,                                     // e_type = ET_EXEC
        0x3e, 0x00,                                     // e_machine = EM_X86_64
        0x01, 0x00, 0x00, 0x00,                         // e_version = 1
        0x78, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // e_entry = 0x400078
        0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_phoff = 64
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_shoff = 0
        0x00, 0x00, 0x00, 0x00,                         // e_flags = 0
        0x40, 0x00,                                     // e_ehsize = 64
        0x38, 0x00,                                     // e_phentsize = 56
        0x01, 0x00,                                     // e_phnum = 1
        0x00, 0x00,                                     // e_shentsize = 0
        0x00, 0x00,                                     // e_shnum = 0
        0x00, 0x00,                                     // e_shstrndx = 0

        // ── Program Header ───────────────────────────────────────────
        0x01, 0x00, 0x00, 0x00,                         // p_type = PT_LOAD
        0x05, 0x00, 0x00, 0x00,                         // p_flags = PF_R | PF_X
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_offset = 0
        0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // p_vaddr = 0x400000
        0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // p_paddr = 0x400000
        0x89, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_filesz = 137 bytes
        0x89, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_memsz = 137 bytes
        0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_align = 0x1000

        // ── Code Segment ─────────────────────────────────────────────
        0xb8, 0x27, 0x00, 0x00, 0x00,                   // mov eax, 39 (sys_getpid)
        0x0f, 0x05,                                     // syscall
        0x48, 0x89, 0xc7,                               // mov rdi, rax (status = pid)
        0xb8, 0x3c, 0x00, 0x00, 0x00,                   // mov eax, 60 (sys_exit)
        0x0f, 0x05,                                     // syscall
    ]
}

/// Create the statically embedded minimal kontsnorsh shell ELF binary.
pub fn create_shell_elf() -> &'static [u8] {
    shell_elf::SHELL_ELF
}

/// Create the statically embedded freestanding C test binary.
pub fn create_hello_elf() -> &'static [u8] {
    hello_elf::HELLO_ELF
}

/// Create the statically embedded freestanding network test binary.
pub fn create_net_test_elf() -> &'static [u8] {
    net_test_elf::NET_TEST_ELF
}

