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

//! Task Control Block (TCB) definition.
//!
//! A Task represents a thread of execution in the kernel. Each process
//! has at least one task (the main thread), and may have additional tasks
//! for multi-threading.
//!
//! ## Task States
//!
//! ```text
//! ┌──────────┐    schedule()    ┌─────────┐
//! │  Ready   │ ──────────────→ │ Running │
//! └──────────┘                 └─────────┘
//!      ↑                            │
//!      │                            │ block() / yield()
//!      │                            ↓
//!      │    wake_up()         ┌──────────┐
//!      └───────────────────── │ Blocked  │
//!                             └──────────┘
//!                                   │
//!                                   │ exit()
//!                                   ↓
//!                             ┌──────────┐
//!                             │  Zombie  │
//!                             └──────────┘
//! ```

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::context::CpuContext;
use super::pid::Pid;
use crate::fs::file::{FileDescription, OpenFlags};
use crate::fs::namespace::{FsContext, UtsNamespace, INITIAL_PID_NS_ID};

/// The state of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Task is ready to run and waiting in the scheduler queue.
    Ready,
    /// Task is currently executing on a CPU.
    Running,
    /// Task is blocked waiting for an event (I/O, sleep, lock, etc.).
    Blocked,
    /// Task has exited but hasn't been waited on by its parent yet.
    Zombie,
}

/// Priority levels for the multi-level feedback queue scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Priority {
    /// Highest priority — real-time / interrupt processing.
    RealTime = 0,
    /// High priority — interactive tasks.
    High = 1,
    /// Normal priority — most user processes.
    Normal = 2,
    /// Low priority — batch / background tasks.
    Low = 3,
    /// Lowest priority — idle tasks.
    Idle = 4,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Normal
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct SigAction {
    pub sa_handler: u64,
    pub sa_flags: u64,
    pub sa_restorer: u64,
    pub sa_mask: u64,
}

#[derive(Clone)]
pub struct MappedRegion {
    pub start: u64,
    pub len: usize,
    pub inode: Option<Arc<dyn crate::fs::inode::InodeOps>>,
    pub offset: u64,
    pub is_shared: bool,
    pub prot: i32,
    pub pathname: Option<alloc::string::String>,
    /// True for the main user-space stack region — enables auto-growth in the page fault handler.
    pub is_stack: bool,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StackT {
    pub ss_sp: u64,
    pub ss_flags: i32,
    pub _pad: i32,
    pub ss_size: u64,
}

impl core::fmt::Debug for MappedRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MappedRegion")
            .field("start", &self.start)
            .field("len", &self.len)
            .field("inode_ino", &self.inode.as_ref().map(|i| i.inode().ino))
            .field("offset", &self.offset)
            .field("is_shared", &self.is_shared)
            .field("prot", &self.prot)
            .field("pathname", &self.pathname)
            .field("is_stack", &self.is_stack)
            .finish()
    }
}

pub struct AddressSpace {
    pub page_table_root: u64,
    pub start_brk: u64,
    pub brk: u64,
    pub mmap_bump: u64,
    pub mmap_regions: Vec<MappedRegion>,
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        if self.page_table_root != 0
            && self.page_table_root != crate::memory::r#virtual::kernel_pml4_phys()
        {
            for r in &self.mmap_regions {
                if r.is_shared && r.inode.is_some() {
                    crate::memory::page_cache::sync_mapped_region(
                        self.page_table_root,
                        r,
                        r.start,
                        r.start.saturating_add(r.len as u64),
                    );
                }
            }
            crate::memory::page_cache::unregister_shared_mmaps_for_page_table(self.page_table_root);
            let _ = crate::memory::r#virtual::free_user_page_table(self.page_table_root);
        }
    }
}

pub struct FdTable {
    pub entries: Vec<Option<Arc<FileDescription>>>,
    pub cloexec: Vec<bool>,
    /// Optimization hint: index of the lowest unallocated descriptor slot.
    pub next_free_fd: usize,
}

/// A Task Control Block (TCB).
///
/// Contains all the information the kernel needs to manage a task:
/// - Identity (PID, name)
/// - Scheduling state and priority
/// - CPU register context (for context switching)
/// - Memory management info (page table root, kernel stack)
/// - File descriptor table (up to 64 open files)
#[repr(C)]
pub struct Task {
    /// Unique process identifier.
    pub pid: Pid,

    /// Human-readable task name (for debugging).
    pub name: String,

    /// Absolute path of the executed binary for `/proc/self/exe`.
    pub executable_path: String,

    /// Current task state.
    pub state: TaskState,

    /// Scheduling priority.
    pub priority: Priority,

    /// Saved CPU context for context switching.
    pub context: CpuContext,

    /// Physical address of this task's page table root (CR3 value) and mapping info.
    pub address_space: Arc<spin::Mutex<AddressSpace>>,

    /// Base address of the kernel stack for this task.
    pub kernel_stack_base: u64,

    /// Size of the kernel stack in bytes.
    pub kernel_stack_size: usize,

    /// Exit code (set when task transitions to Zombie state).
    pub exit_code: Option<i32>,

    /// Parent PID (0 for the init process).
    pub parent_pid: Pid,

    /// Process group ID (POSIX job control).
    pub pgid: u64,

    /// Thread group ID (Process ID).
    pub tgid: Pid,

    /// CPU time consumed (in timer ticks).
    pub cpu_ticks: u64,

    /// Open file descriptor table.
    pub fd_table: Arc<spin::Mutex<FdTable>>,

    /// Current working directory (always an absolute normalized path).
    pub cwd: String,

    /// Pending signals mask.
    pub pending_signals: u64,

    /// Blocked signals mask.
    pub blocked_signals: u64,

    /// Registered signal actions.
    pub sigactions: Arc<spin::Mutex<[SigAction; 64]>>,

    /// Wait queue for child process state changes (e.g. wait4).
    pub child_wait_queue: Arc<crate::sync::wait_queue::WaitQueue>,

    /// Dedicated pre-allocated wait queue for poll/select/epoll multiplexing.
    pub poll_wait_queue: Arc<crate::sync::wait_queue::WaitQueue>,

    /// Tracks whether this task is currently queued in the scheduler priority queues.
    pub in_queue: bool,

    /// Tracks whether this task is a CPU core's idle task.
    pub is_idle: bool,

    /// Real User ID
    pub uid: u32,
    /// Real Group ID
    pub gid: u32,
    /// Effective User ID
    pub euid: u32,
    /// Effective Group ID
    pub egid: u32,
    /// Saved set-user-ID
    pub suid: u32,
    /// Saved set-group-ID
    pub sgid: u32,
    /// Supplementary group IDs
    pub groups: Vec<u32>,
    /// Session ID
    pub sid: u64,
    /// Registered user-space address to be cleared when thread exits (CLONE_CHILD_CLEARTID)
    pub clear_child_tid: Option<u64>,
    /// Alternate signal stack.
    pub sigaltstack: Option<StackT>,
    /// Soft limit for open files (RLIMIT_NOFILE)
    pub rlimit_nofile_cur: u64,
    /// Hard limit for open files (RLIMIT_NOFILE)
    pub rlimit_nofile_max: u64,
    /// Process command line arguments
    pub cmdline: Vec<String>,
    /// File mode creation mask (umask)
    pub umask: u32,
    /// Robust futex list head address
    pub robust_list_head: u64,
    /// Robust futex list length
    pub robust_list_len: usize,
    /// Restartable sequence registration address
    pub rseq: Option<u64>,
    /// Restartable sequence size
    pub rseq_len: u32,
    /// Restartable sequence signature
    pub rseq_sig: u32,
    /// Per-task filesystem context (jail root + mount namespace).
    ///
    /// Shared with siblings until `unshare(CLONE_NEWNS)` is called.
    /// Protected by `RwLock` so readers of `root` pay minimal contention.
    pub fs_ctx: Arc<spin::RwLock<FsContext>>,
    /// Per-task UTS namespace (hostname / domainname).
    ///
    /// Cloned on `fork`; independently mutable after `unshare(CLONE_NEWUTS)`.
    pub uts_ns: UtsNamespace,
    /// PID namespace identifier.
    ///
    /// Processes share a PID namespace with their siblings unless
    /// `clone(CLONE_NEWPID)` or `unshare(CLONE_NEWPID)` + fork is used.
    /// Signals, `wait4`, and `/proc` enumeration are restricted to tasks
    /// within the same `pid_ns_id`.
    pub pid_ns_id: u64,
    /// Pending PID namespace ID for subsequent children created via fork/clone.
    /// Set when the task calls unshare(CLONE_NEWPID).
    pub child_pid_ns_id: Option<u64>,
    /// Tracks if this task is the init process (PID 1) of its PID namespace.
    pub is_pid_ns_init: bool,
}

impl Task {
    /// Create a new task with the given PID and name.
    pub fn new(pid: Pid, name: String, page_table_root: u64) -> Self {
        // Pre-populate the standard I/O file descriptors for every task.
        // Kernel threads won't use these but they don't hurt.
        let mut entries: Vec<Option<Arc<FileDescription>>> = Vec::new();
        entries.push(Some(Arc::new(FileDescription::new(
            crate::fs::tty::make_stdin(),
            OpenFlags(OpenFlags::O_RDONLY),
            Some(alloc::string::String::from("/dev/stdin")),
        )))); // fd 0: stdin
        entries.push(Some(Arc::new(FileDescription::new(
            crate::fs::tty::make_stdout(),
            OpenFlags(OpenFlags::O_WRONLY),
            Some(alloc::string::String::from("/dev/stdout")),
        )))); // fd 1: stdout
        entries.push(Some(Arc::new(FileDescription::new(
            crate::fs::tty::make_stderr(),
            OpenFlags(OpenFlags::O_WRONLY),
            Some(alloc::string::String::from("/dev/stderr")),
        )))); // fd 2: stderr

        Self {
            pid,
            name: name.clone(),
            executable_path: String::from("/bin/sh"),
            state: TaskState::Ready,
            priority: Priority::default(),
            context: CpuContext {
                cr3: page_table_root,
                ..CpuContext::default()
            },
            address_space: Arc::new(spin::Mutex::new(AddressSpace {
                page_table_root,
                start_brk: 0,
                brk: 0,
                mmap_bump: 0x0000_5000_0000_0000u64,
                mmap_regions: Vec::new(),
            })),
            kernel_stack_base: 0,
            kernel_stack_size: 0,
            exit_code: None,
            parent_pid: Pid::IDLE,
            cpu_ticks: 0,
            fd_table: Arc::new(spin::Mutex::new(FdTable {
                entries,
                cloexec: alloc::vec![false, false, false],
                next_free_fd: 3,
            })),
            cwd: String::from("/"),
            pending_signals: 0,
            blocked_signals: 0,
            sigactions: Arc::new(spin::Mutex::new(
                [SigAction {
                    sa_handler: 0,
                    sa_flags: 0,
                    sa_restorer: 0,
                    sa_mask: 0,
                }; 64],
            )),
            child_wait_queue: Arc::new(crate::sync::wait_queue::WaitQueue::new()),
            poll_wait_queue: Arc::new(crate::sync::wait_queue::WaitQueue::new()),
            pgid: pid.as_u64(),
            tgid: pid,
            in_queue: false,
            is_idle: false,
            uid: 0,
            gid: 0,
            euid: 0,
            egid: 0,
            suid: 0,
            sgid: 0,
            groups: Vec::new(),
            sid: pid.as_u64(),
            clear_child_tid: None,
            sigaltstack: None,
            rlimit_nofile_cur: 1024,
            rlimit_nofile_max: 4096,
            cmdline: Vec::new(),
            umask: 0o022,
            robust_list_head: 0,
            robust_list_len: 0,
            rseq: None,
            rseq_len: 0,
            rseq_sig: 0,
            // Share the global mount namespace; per-task isolation happens via
            // unshare(CLONE_NEWNS) / chroot which fork the namespace/root.
            fs_ctx: Arc::new(spin::RwLock::new(FsContext::new_initial(
                crate::fs::namespace::INITIAL_MOUNT_NS.clone(),
            ))),
            uts_ns: UtsNamespace::default_ns(),
            pid_ns_id: INITIAL_PID_NS_ID,
            child_pid_ns_id: None,
            is_pid_ns_init: pid.as_u64() == 1,
        }
    }

    /// Check if this task is a user-space task (not kernel thread or idle).
    pub fn is_user(&self) -> bool {
        !self.is_idle && self.context.cr3 != crate::memory::r#virtual::kernel_pml4_phys()
    }

    /// Create the kernel idle task (PID 0).
    pub fn idle() -> Self {
        let kernel_pml4 = crate::memory::r#virtual::kernel_pml4_phys();
        let mut task = Self::new(Pid::IDLE, String::from("idle"), kernel_pml4);
        task.priority = Priority::Idle;
        task.is_idle = true;
        task
    }

    /// Check if this task is runnable.
    pub fn is_runnable(&self) -> bool {
        matches!(self.state, TaskState::Ready | TaskState::Running)
    }
}

impl core::fmt::Debug for Task {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Task")
            .field("pid", &self.pid)
            .field("name", &self.name)
            .field("state", &self.state)
            .field("priority", &self.priority)
            .finish()
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        // Free the kernel stack if allocated
        if self.kernel_stack_base != 0 && self.kernel_stack_size != 0 {
            let layout = alloc::alloc::Layout::from_size_align(self.kernel_stack_size, 16).unwrap();
            // SAFETY: kernel_stack_base and kernel_stack_size were allocated using exactly the same layout in clone/fork.
            unsafe {
                alloc::alloc::dealloc(self.kernel_stack_base as *mut u8, layout);
            }
        }
        // Release advisory fcntl locks
        let pid_val = self.pid.as_u64();
        crate::syscall::fs::io::release_fcntl_locks(pid_val);
    }
}
