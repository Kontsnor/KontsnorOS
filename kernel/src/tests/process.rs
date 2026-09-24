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

//! Process & Scheduler subsystem unit and regression tests.

#[test_case]
fn test_scheduler_priority_queues() {
    let mut sched = crate::process::scheduler::Scheduler::new();

    // Create mock tasks with High, Normal, and Low priorities
    let pid_high = crate::process::pid::Pid::from_raw(100);
    let mut task_high =
        crate::process::task::Task::new(pid_high, alloc::string::String::from("high_prio"), 0);
    task_high.priority = crate::process::task::Priority::High;
    task_high.state = crate::process::task::TaskState::Ready;

    let pid_normal = crate::process::pid::Pid::from_raw(101);
    let mut task_normal =
        crate::process::task::Task::new(pid_normal, alloc::string::String::from("normal_prio"), 0);
    task_normal.priority = crate::process::task::Priority::Normal;
    task_normal.state = crate::process::task::TaskState::Ready;

    let pid_low = crate::process::pid::Pid::from_raw(102);
    let mut task_low =
        crate::process::task::Task::new(pid_low, alloc::string::String::from("low_prio"), 0);
    task_low.priority = crate::process::task::Priority::Low;
    task_low.state = crate::process::task::TaskState::Ready;

    // Add them in mixed order
    sched.add_task(task_low);
    sched.add_task(task_high);
    sched.add_task(task_normal);

    // pick_next should retrieve them in priority order: High (100), Normal (101), Low (102)
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_high));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_normal));
    assert_eq!(sched.pick_next().map(|(p, _)| p), Some(pid_low));
    assert_eq!(sched.pick_next(), None);

    sched.remove_mock_task(pid_high);
    sched.remove_mock_task(pid_normal);
    sched.remove_mock_task(pid_low);
}

#[test_case]
fn test_orphan_reparenting() {
    let mut sched = crate::process::scheduler::Scheduler::new();

    // Create parent task (PID 20)
    let pid_parent = crate::process::pid::Pid::from_raw(20);
    let mut task_parent =
        crate::process::task::Task::new(pid_parent, alloc::string::String::from("parent"), 0);
    task_parent.state = crate::process::task::TaskState::Ready;
    sched.add_task(task_parent);

    // Create child task (PID 21) whose parent is the parent task
    let pid_child = crate::process::pid::Pid::from_raw(21);
    let mut task_child =
        crate::process::task::Task::new(pid_child, alloc::string::String::from("child"), 0);
    task_child.parent_pid = pid_parent;
    task_child.state = crate::process::task::TaskState::Ready;
    sched.add_task(task_child);

    // Exit the parent task
    sched.exit_task(pid_parent, 0);

    // Verify child has been re-parented to PID 1 (INIT)
    let child_arc = crate::process::scheduler::get_task_arc(pid_child).expect("Child task missing");
    let child = child_arc.lock();
    assert_eq!(child.parent_pid, crate::process::pid::Pid::INIT);

    // Verify parent has transitioned to Zombie
    let parent_arc =
        crate::process::scheduler::get_task_arc(pid_parent).expect("Parent task missing");
    let parent = parent_arc.lock();
    assert_eq!(parent.state, crate::process::task::TaskState::Zombie);
    drop(parent);

    sched.remove_mock_task(pid_parent);
    sched.remove_mock_task(pid_child);
}

#[test_case]
fn test_auxiliary_vectors() {
    let phys = crate::memory::physical::allocate_frame().expect("Failed to allocate frame");

    let argv = [alloc::string::String::from("test_arg")];
    let envp = [alloc::string::String::from("TEST_ENV=1")];
    let entry_point = 0x10002000;
    let phdr = 0x30004000;
    let phnum = 4;
    let phent = 56;
    let interpreter_base = 0x0000_7FFF_F7F0_0000;

    let init_stack = crate::process::elf::construct_user_stack(
        &argv,
        &envp,
        entry_point,
        phdr,
        phnum,
        phent,
        interpreter_base,
    )
    .expect("Failed to construct user stack");

    let user_sp = init_stack.user_sp;

    // The stack pointer returned is at some offset in the stack top.
    assert!(user_sp >= init_stack.base_vaddr);
    assert!(user_sp < crate::process::elf::USER_STACK_TOP);
    let offset_in_buf = (user_sp - init_stack.base_vaddr) as usize;

    let stack_ptr = unsafe { init_stack.stack_buf.as_ptr().add(offset_in_buf) } as *const u64;

    let argc = unsafe { stack_ptr.read() };
    assert_eq!(argc, 1);

    let arg0_ptr = unsafe { stack_ptr.add(1).read() };
    assert!(arg0_ptr > 0);

    let argv_null = unsafe { stack_ptr.add(2).read() };
    assert_eq!(argv_null, 0);

    let env0_ptr = unsafe { stack_ptr.add(3).read() };
    assert!(env0_ptr > 0);

    let envp_null = unsafe { stack_ptr.add(4).read() };
    assert_eq!(envp_null, 0);

    // The auxiliary vectors start at index 5.
    let mut aux_idx = 5;
    let mut found_phdr = false;
    let mut found_base = false;
    let mut found_entry = false;
    let mut found_phent = false;
    let mut found_phnum = false;
    let mut found_pagesz = false;
    let mut found_random = false;

    loop {
        let type_ = unsafe { stack_ptr.add(aux_idx).read() };
        let val = unsafe { stack_ptr.add(aux_idx + 1).read() };
        if type_ == 0 {
            break;
        }
        match type_ {
            3 => {
                // AT_PHDR
                assert_eq!(val, phdr);
                found_phdr = true;
            }
            4 => {
                // AT_PHENT
                assert_eq!(val, phent);
                found_phent = true;
            }
            5 => {
                // AT_PHNUM
                assert_eq!(val, phnum);
                found_phnum = true;
            }
            6 => {
                // AT_PAGESZ
                assert_eq!(val, 4096);
                found_pagesz = true;
            }
            7 => {
                // AT_BASE
                assert_eq!(val, interpreter_base);
                found_base = true;
            }
            9 => {
                // AT_ENTRY
                assert_eq!(val, entry_point);
                found_entry = true;
            }
            25 => {
                // AT_RANDOM
                assert!(val > 0);
                found_random = true;
            }
            _ => {}
        }
        aux_idx += 2;
    }

    assert!(found_phdr, "AT_PHDR not found or incorrect");
    assert!(found_phent, "AT_PHENT not found or incorrect");
    assert!(found_phnum, "AT_PHNUM not found or incorrect");
    assert!(found_pagesz, "AT_PAGESZ not found or incorrect");
    assert!(found_base, "AT_BASE not found or incorrect");
    assert!(found_entry, "AT_ENTRY not found or incorrect");
    assert!(found_random, "AT_RANDOM not found or incorrect");

    crate::memory::physical::deallocate_frame(phys);
}

#[test_case]
fn test_userspace_wrfsbase() {
    let test_val = 0x0000_1234_5678_9ABCu64;

    // Save current FS_BASE
    let orig_fs = x86_64::registers::model_specific::FsBase::read().as_u64();

    // Write new FS_BASE using wrfsbase instruction
    unsafe {
        core::arch::asm!(
            "wrfsbase {}",
            in(reg) test_val,
        );
    }

    // Read and verify
    let new_fs = x86_64::registers::model_specific::FsBase::read().as_u64();
    assert_eq!(new_fs, test_val);

    // Restore original FS_BASE
    unsafe {
        core::arch::asm!(
            "wrfsbase {}",
            in(reg) orig_fs,
        );
    }
}
