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

//! System V IPC and POSIX Message Queue system calls.

use crate::process::scheduler;
use crate::sync::spinlock::TicketLock;
use crate::sync::wait_queue::WaitQueue;
use crate::syscall::validation::{
    copy_string_from_user, validate_user_ptr, validate_user_ptr_write,
};
use crate::syscall::{Errno, SyscallResult};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use x86_64::structures::paging::{Page, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub const IPC_CREAT: i32 = 0o1000;
pub const IPC_EXCL: i32 = 0o2000;
pub const IPC_NOWAIT: i32 = 0o4000;

pub const IPC_RMID: i32 = 0;
pub const IPC_SET: i32 = 1;
pub const IPC_STAT: i32 = 2;
pub const IPC_INFO: i32 = 3;

pub const SHM_RDONLY: i32 = 0o10000;
pub const SHM_RND: i32 = 0o20000;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IpcPerm {
    pub key: i32,
    pub uid: u32,
    pub gid: u32,
    pub cuid: u32,
    pub cgid: u32,
    pub mode: u16,
    pub __pad1: u16,
    pub seq: u16,
    pub __pad2: u16,
    pub __unused1: u64,
    pub __unused2: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ShmidDs {
    pub shm_perm: IpcPerm,
    pub shm_segsz: usize,
    pub shm_atime: i64,
    pub shm_dtime: i64,
    pub shm_ctime: i64,
    pub shm_cpid: i32,
    pub shm_lpid: i32,
    pub shm_nattch: u64,
    pub __unused4: u64,
    pub __unused5: u64,
}

pub struct ShmSegment {
    pub shmid: i32,
    pub key: i32,
    pub size: usize,
    pub ds: ShmidDs,
    pub pages: Vec<u64>,
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        for phys in &self.pages {
            crate::memory::physical::deallocate_frame(*phys);
        }
    }
}

static SHM_NEXT_ID: TicketLock<i32> = TicketLock::new(1);
static SHM_REGISTRY: TicketLock<BTreeMap<i32, Arc<TicketLock<ShmSegment>>>> =
    TicketLock::new(BTreeMap::new());

/// `shmget(key, size, shmflg)` — allocates a System V shared memory segment.
pub fn sys_shmget(key: i32, size: usize, shmflg: i32) -> SyscallResult {
    if size == 0 && (shmflg & IPC_CREAT) != 0 {
        return Errno::EINVAL.into();
    }

    let mut reg = SHM_REGISTRY.lock();

    // Check if key already exists (unless key == 0 / IPC_PRIVATE)
    if key != 0 {
        for (&id, seg_lock) in reg.iter() {
            let seg = seg_lock.lock();
            if seg.key == key {
                if (shmflg & IPC_CREAT) != 0 && (shmflg & IPC_EXCL) != 0 {
                    return Errno::EEXIST.into();
                }
                if size > seg.size {
                    return Errno::EINVAL.into();
                }
                return id as SyscallResult;
            }
        }
    }

    if (shmflg & IPC_CREAT) == 0 {
        return Errno::ENOENT.into();
    }

    let aligned_size = match size.checked_add(4095) {
        Some(s) => s & !4095,
        None => return Errno::EINVAL.into(),
    };
    let num_pages = aligned_size / 4096;
    let mut pages = Vec::with_capacity(num_pages);
    for _ in 0..num_pages {
        let frame = match crate::memory::physical::allocate_frame() {
            Some(f) => f,
            None => {
                for p in pages {
                    crate::memory::physical::deallocate_frame(p);
                }
                return Errno::ENOMEM.into();
            }
        };
        // Zero the frame
        let virt = frame + crate::memory::r#virtual::phys_mem_offset();
        // SAFETY: Direct-mapped physical frame
        unsafe {
            core::ptr::write_bytes(virt as *mut u8, 0, 4096);
        }
        pages.push(frame);
    }

    let id = {
        let mut nid = SHM_NEXT_ID.lock();
        let cur = *nid;
        *nid = cur.wrapping_add(1);
        cur
    };

    let ds = ShmidDs {
        shm_perm: IpcPerm {
            key,
            uid: 0,
            gid: 0,
            cuid: 0,
            cgid: 0,
            mode: (shmflg & 0o777) as u16,
            __pad1: 0,
            seq: 0,
            __pad2: 0,
            __unused1: 0,
            __unused2: 0,
        },
        shm_segsz: size,
        shm_atime: 0,
        shm_dtime: 0,
        shm_ctime: 0,
        shm_cpid: 0,
        shm_lpid: 0,
        shm_nattch: 0,
        __unused4: 0,
        __unused5: 0,
    };

    let seg = Arc::new(TicketLock::new(ShmSegment {
        shmid: id,
        key,
        size,
        ds,
        pages,
    }));

    reg.insert(id, seg);
    id as SyscallResult
}

/// `shmat(shmid, shmaddr, shmflg)` — attaches the System V shared memory segment.
pub fn sys_shmat(shmid: i32, shmaddr: *const u8, shmflg: i32) -> SyscallResult {
    let seg_arc = {
        let reg = SHM_REGISTRY.lock();
        match reg.get(&shmid).cloned() {
            Some(s) => s,
            None => return Errno::EINVAL.into(),
        }
    };

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    let mut seg = seg_arc.lock();
    let num_pages = seg.pages.len();
    let map_len = num_pages * 4096;

    let task = task_arc.lock();
    let mut addr_space = task.address_space.lock();

    let resolved_addr = if !shmaddr.is_null() {
        let addr = shmaddr as u64;
        if (shmflg & SHM_RND) != 0 {
            addr & !4095
        } else if (addr & 4095) != 0 {
            return Errno::EINVAL.into();
        } else {
            addr
        }
    } else {
        let base = addr_space.mmap_bump;
        let next = match base.checked_add(map_len as u64) {
            Some(n) => n,
            None => return Errno::ENOMEM.into(),
        };
        addr_space.mmap_bump = next;
        base
    };

    use x86_64::structures::paging::PageTableFlags;
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (shmflg & SHM_RDONLY) == 0 {
        flags |= PageTableFlags::WRITABLE;
    }

    let pt_root = addr_space.page_table_root;
    for (i, &phys) in seg.pages.iter().enumerate() {
        let vaddr = match resolved_addr.checked_add((i * 4096) as u64) {
            Some(va) => va,
            None => return Errno::EINVAL.into(),
        };
        let page = Page::containing_address(VirtAddr::new(vaddr));
        let frame = PhysFrame::containing_address(PhysAddr::new(phys));
        // SAFETY: Mapping shared memory page into user space
        if unsafe { crate::memory::r#virtual::map_user_page(pt_root, page, frame, flags) }.is_err()
        {
            return Errno::ENOMEM.into();
        }
    }

    addr_space
        .mmap_regions
        .push(crate::process::task::MappedRegion {
            start: resolved_addr,
            len: map_len,
            inode: None,
            offset: 0,
            is_shared: true,
            prot: if (shmflg & SHM_RDONLY) != 0 { 1 } else { 3 },
            pathname: Some(alloc::format!("shm:{}", shmid)),
            is_stack: false,
        });

    seg.ds.shm_nattch = seg.ds.shm_nattch.saturating_add(1);
    resolved_addr as SyscallResult
}

/// `shmdt(shmaddr)` — detaches the System V shared memory segment.
pub fn sys_shmdt(shmaddr: *const u8) -> SyscallResult {
    if (shmaddr as usize & 4095) != 0 || shmaddr.is_null() {
        return Errno::EINVAL.into();
    }
    let target_addr = shmaddr as u64;

    let current_pid = match scheduler::current_pid() {
        Some(p) => p,
        None => return Errno::ESRCH.into(),
    };
    let task_arc = match scheduler::get_task_arc(current_pid) {
        Some(t) => t,
        None => return Errno::ESRCH.into(),
    };

    let task = task_arc.lock();
    let mut addr_space = task.address_space.lock();

    let mut found_idx = None;
    for (i, r) in addr_space.mmap_regions.iter().enumerate() {
        if r.start == target_addr && r.pathname.as_ref().map_or(false, |p| p.starts_with("shm:")) {
            found_idx = Some(i);
            break;
        }
    }

    let idx = match found_idx {
        Some(i) => i,
        None => return Errno::EINVAL.into(),
    };

    let region = addr_space.mmap_regions.remove(idx);
    let num_pages = region.len / 4096;
    let pt_root = addr_space.page_table_root;

    for i in 0..num_pages {
        let vaddr = region.start + (i * 4096) as u64;
        let page = Page::containing_address(VirtAddr::new(vaddr));
        // SAFETY: Unmapping shared memory page
        let _ = unsafe { crate::memory::r#virtual::unmap_user_page(pt_root, page) };
    }
    crate::arch::x86_64::smp::shootdown_tlb();

    if let Some(ref name) = region.pathname {
        if let Some(id_str) = name.strip_prefix("shm:") {
            if let Ok(id) = id_str.parse::<i32>() {
                let reg = SHM_REGISTRY.lock();
                if let Some(seg_lock) = reg.get(&id) {
                    let mut seg = seg_lock.lock();
                    seg.ds.shm_nattch = seg.ds.shm_nattch.saturating_sub(1);
                }
            }
        }
    }

    0
}

/// `shmctl(shmid, cmd, buf)` — System V shared memory control.
pub fn sys_shmctl(shmid: i32, cmd: i32, buf: *mut ShmidDs) -> SyscallResult {
    let mut reg = SHM_REGISTRY.lock();
    let seg_lock = match reg.get(&shmid).cloned() {
        Some(s) => s,
        None => return Errno::EINVAL.into(),
    };

    match cmd {
        IPC_RMID => {
            reg.remove(&shmid);
            0
        }
        IPC_STAT => {
            if buf.is_null() {
                return Errno::EFAULT.into();
            }
            if validate_user_ptr_write(buf as *mut u8, core::mem::size_of::<ShmidDs>()).is_err() {
                return Errno::EFAULT.into();
            }
            let seg = seg_lock.lock();
            // SAFETY: Validated buffer for write
            unsafe {
                core::ptr::write(buf, seg.ds);
            }
            0
        }
        IPC_SET => {
            if buf.is_null() {
                return Errno::EFAULT.into();
            }
            if !validate_user_ptr(buf as *const u8, core::mem::size_of::<ShmidDs>()) {
                return Errno::EFAULT.into();
            }
            // SAFETY: Validated buffer
            let ds = unsafe { core::ptr::read(buf) };
            let mut seg = seg_lock.lock();
            seg.ds.shm_perm.uid = ds.shm_perm.uid;
            seg.ds.shm_perm.gid = ds.shm_perm.gid;
            seg.ds.shm_perm.mode = ds.shm_perm.mode;
            0
        }
        _ => Errno::EINVAL.into(),
    }
}

// ---------------- Semaphores ----------------

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SemBuf {
    pub sem_num: u16,
    pub sem_op: i16,
    pub sem_flg: i16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SemidDs {
    pub sem_perm: IpcPerm,
    pub sem_otime: i64,
    pub sem_ctime: i64,
    pub sem_nsems: u64,
    pub __unused3: u64,
    pub __unused4: u64,
}

pub struct SemArray {
    pub semid: i32,
    pub key: i32,
    pub values: Vec<i16>,
    pub ds: SemidDs,
    pub wait_queue: Arc<WaitQueue>,
}

static SEM_NEXT_ID: TicketLock<i32> = TicketLock::new(1);
static SEM_REGISTRY: TicketLock<BTreeMap<i32, Arc<TicketLock<SemArray>>>> =
    TicketLock::new(BTreeMap::new());

/// `semget(key, nsems, semflg)` — get a System V semaphore set identifier.
pub fn sys_semget(key: i32, nsems: i32, semflg: i32) -> SyscallResult {
    if nsems < 0 || nsems > 1024 {
        return Errno::EINVAL.into();
    }

    let mut reg = SEM_REGISTRY.lock();
    if key != 0 {
        for (&id, sem_lock) in reg.iter() {
            let s = sem_lock.lock();
            if s.key == key {
                if (semflg & IPC_CREAT) != 0 && (semflg & IPC_EXCL) != 0 {
                    return Errno::EEXIST.into();
                }
                if (nsems as usize) > s.values.len() {
                    return Errno::EINVAL.into();
                }
                return id as SyscallResult;
            }
        }
    }

    if (semflg & IPC_CREAT) == 0 {
        return Errno::ENOENT.into();
    }

    let id = {
        let mut nid = SEM_NEXT_ID.lock();
        let cur = *nid;
        *nid = cur.wrapping_add(1);
        cur
    };

    let ds = SemidDs {
        sem_perm: IpcPerm {
            key,
            uid: 0,
            gid: 0,
            cuid: 0,
            cgid: 0,
            mode: (semflg & 0o777) as u16,
            __pad1: 0,
            seq: 0,
            __pad2: 0,
            __unused1: 0,
            __unused2: 0,
        },
        sem_otime: 0,
        sem_ctime: 0,
        sem_nsems: nsems as u64,
        __unused3: 0,
        __unused4: 0,
    };

    let array = Arc::new(TicketLock::new(SemArray {
        semid: id,
        key,
        values: alloc::vec![0; nsems as usize],
        ds,
        wait_queue: Arc::new(WaitQueue::new()),
    }));

    reg.insert(id, array);
    id as SyscallResult
}

/// `semop(semid, sops, nsops)` — System V semaphore operations.
pub fn sys_semop(semid: i32, sops: *const SemBuf, nsops: usize) -> SyscallResult {
    sys_semtimedop(semid, sops, nsops, core::ptr::null())
}

/// `semtimedop(semid, sops, nsops, timeout)` — System V semaphore operations with timeout.
pub fn sys_semtimedop(
    semid: i32,
    sops: *const SemBuf,
    nsops: usize,
    _timeout: *const u8,
) -> SyscallResult {
    if nsops == 0 || nsops > 256 || sops.is_null() {
        return Errno::EINVAL.into();
    }
    let total_size = match nsops.checked_mul(core::mem::size_of::<SemBuf>()) {
        Some(s) => s,
        None => return Errno::EINVAL.into(),
    };
    if !validate_user_ptr(sops as *const u8, total_size) {
        return Errno::EFAULT.into();
    }

    let ops = unsafe { core::slice::from_raw_parts(sops, nsops) };

    let sem_arc = {
        let reg = SEM_REGISTRY.lock();
        match reg.get(&semid).cloned() {
            Some(s) => s,
            None => return Errno::EINVAL.into(),
        }
    };

    let mut sem = sem_arc.lock();
    for op in ops {
        if (op.sem_num as usize) >= sem.values.len() {
            return Errno::EFBIG.into();
        }
    }

    // Attempt all operations
    for op in ops {
        let idx = op.sem_num as usize;
        let val = sem.values[idx];
        if op.sem_op < 0 {
            let dec = -op.sem_op;
            if val < dec {
                if (op.sem_flg & (IPC_NOWAIT as i16)) != 0 {
                    return -(Errno::EAGAIN as i64);
                }
                // Decrement what we can or wait
                sem.values[idx] = 0;
            } else {
                sem.values[idx] -= dec;
            }
        } else if op.sem_op > 0 {
            sem.values[idx] = sem.values[idx].saturating_add(op.sem_op);
        }
    }

    sem.wait_queue.wake_all();
    0
}

/// `semctl(semid, semnum, cmd, arg)` — System V semaphore control.
pub fn sys_semctl(semid: i32, semnum: i32, cmd: i32, arg: u64) -> SyscallResult {
    let mut reg = SEM_REGISTRY.lock();
    let sem_lock = match reg.get(&semid).cloned() {
        Some(s) => s,
        None => return Errno::EINVAL.into(),
    };

    match cmd {
        IPC_RMID => {
            reg.remove(&semid);
            0
        }
        IPC_STAT => {
            let buf = arg as *mut SemidDs;
            if buf.is_null()
                || validate_user_ptr_write(buf as *mut u8, core::mem::size_of::<SemidDs>()).is_err()
            {
                return Errno::EFAULT.into();
            }
            let sem = sem_lock.lock();
            // SAFETY: Validated user ptr
            unsafe { core::ptr::write(buf, sem.ds) };
            0
        }
        12 => {
            // GETVAL
            let sem = sem_lock.lock();
            if semnum < 0 || (semnum as usize) >= sem.values.len() {
                return Errno::EINVAL.into();
            }
            sem.values[semnum as usize] as SyscallResult
        }
        16 => {
            // SETVAL
            let mut sem = sem_lock.lock();
            if semnum < 0 || (semnum as usize) >= sem.values.len() {
                return Errno::EINVAL.into();
            }
            sem.values[semnum as usize] = arg as i16;
            sem.wait_queue.wake_all();
            0
        }
        _ => 0,
    }
}

// ---------------- Message Queues ----------------

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MsqidDs {
    pub msg_perm: IpcPerm,
    pub msg_stime: i64,
    pub msg_rtime: i64,
    pub msg_ctime: i64,
    pub msg_cbytes: u64,
    pub msg_qnum: u64,
    pub msg_qbytes: u64,
    pub msg_lspid: i32,
    pub msg_lrpid: i32,
    pub __unused4: u64,
    pub __unused5: u64,
}

pub struct MsgQueue {
    pub msqid: i32,
    pub key: i32,
    pub messages: VecDeque<(i64, Vec<u8>)>,
    pub ds: MsqidDs,
    pub wait_queue: Arc<WaitQueue>,
}

static MSG_NEXT_ID: TicketLock<i32> = TicketLock::new(1);
static MSG_REGISTRY: TicketLock<BTreeMap<i32, Arc<TicketLock<MsgQueue>>>> =
    TicketLock::new(BTreeMap::new());

/// `msgget(key, msgflg)` — get a System V message queue identifier.
pub fn sys_msgget(key: i32, msgflg: i32) -> SyscallResult {
    let mut reg = MSG_REGISTRY.lock();
    if key != 0 {
        for (&id, mq_lock) in reg.iter() {
            let q = mq_lock.lock();
            if q.key == key {
                if (msgflg & IPC_CREAT) != 0 && (msgflg & IPC_EXCL) != 0 {
                    return Errno::EEXIST.into();
                }
                return id as SyscallResult;
            }
        }
    }

    if (msgflg & IPC_CREAT) == 0 {
        return Errno::ENOENT.into();
    }

    let id = {
        let mut nid = MSG_NEXT_ID.lock();
        let cur = *nid;
        *nid = cur.wrapping_add(1);
        cur
    };

    let ds = MsqidDs {
        msg_perm: IpcPerm {
            key,
            uid: 0,
            gid: 0,
            cuid: 0,
            cgid: 0,
            mode: (msgflg & 0o777) as u16,
            __pad1: 0,
            seq: 0,
            __pad2: 0,
            __unused1: 0,
            __unused2: 0,
        },
        msg_stime: 0,
        msg_rtime: 0,
        msg_ctime: 0,
        msg_cbytes: 0,
        msg_qnum: 0,
        msg_qbytes: 16384,
        msg_lspid: 0,
        msg_lrpid: 0,
        __unused4: 0,
        __unused5: 0,
    };

    let mq = Arc::new(TicketLock::new(MsgQueue {
        msqid: id,
        key,
        messages: VecDeque::new(),
        ds,
        wait_queue: Arc::new(WaitQueue::new()),
    }));

    reg.insert(id, mq);
    id as SyscallResult
}

/// `msgsnd(msqid, msgp, msgsz, msgflg)` — send a message to a System V message queue.
pub fn sys_msgsnd(msqid: i32, msgp: *const u8, msgsz: usize, _msgflg: i32) -> SyscallResult {
    if msgp.is_null() || msgsz > 65536 {
        return Errno::EINVAL.into();
    }
    let total_size = match msgsz.checked_add(core::mem::size_of::<i64>()) {
        Some(s) => s,
        None => return Errno::EINVAL.into(),
    };
    if !validate_user_ptr(msgp, total_size) {
        return Errno::EFAULT.into();
    }

    let mtype = unsafe { core::ptr::read_unaligned(msgp as *const i64) };
    if mtype <= 0 {
        return Errno::EINVAL.into();
    }

    let payload =
        unsafe { core::slice::from_raw_parts(msgp.add(core::mem::size_of::<i64>()), msgsz) };

    let mq_arc = {
        let reg = MSG_REGISTRY.lock();
        match reg.get(&msqid).cloned() {
            Some(q) => q,
            None => return Errno::EINVAL.into(),
        }
    };

    let mut mq = mq_arc.lock();
    mq.messages.push_back((mtype, payload.to_vec()));
    mq.ds.msg_qnum = mq.messages.len() as u64;
    mq.ds.msg_cbytes = mq.ds.msg_cbytes.saturating_add(msgsz as u64);
    mq.wait_queue.wake_all();
    0
}

/// `msgrcv(msqid, msgp, msgsz, msgtyp, msgflg)` — receive a message from a System V message queue.
pub fn sys_msgrcv(
    msqid: i32,
    msgp: *mut u8,
    msgsz: usize,
    msgtyp: i64,
    _msgflg: i32,
) -> SyscallResult {
    if msgp.is_null() {
        return Errno::EINVAL.into();
    }
    let total_size = match msgsz.checked_add(core::mem::size_of::<i64>()) {
        Some(s) => s,
        None => return Errno::EINVAL.into(),
    };
    if validate_user_ptr_write(msgp, total_size).is_err() {
        return Errno::EFAULT.into();
    }

    let mq_arc = {
        let reg = MSG_REGISTRY.lock();
        match reg.get(&msqid).cloned() {
            Some(q) => q,
            None => return Errno::EINVAL.into(),
        }
    };

    let mut mq = mq_arc.lock();
    let mut found_idx = None;
    for (i, (mtype, _)) in mq.messages.iter().enumerate() {
        if msgtyp == 0 || *mtype == msgtyp || (msgtyp < 0 && *mtype <= -msgtyp) {
            found_idx = Some(i);
            break;
        }
    }

    let idx = match found_idx {
        Some(i) => i,
        None => return -(Errno::ENOMSG as i64),
    };

    let (mtype, data) = mq.messages.remove(idx).unwrap();
    let copy_len = data.len().min(msgsz);

    // SAFETY: Validated user write buffer
    unsafe {
        core::ptr::write_unaligned(msgp as *mut i64, mtype);
        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            msgp.add(core::mem::size_of::<i64>()),
            copy_len,
        );
    }

    mq.ds.msg_qnum = mq.messages.len() as u64;
    mq.ds.msg_cbytes = mq.ds.msg_cbytes.saturating_sub(data.len() as u64);
    copy_len as SyscallResult
}

/// `msgctl(msqid, cmd, buf)` — System V message control operations.
pub fn sys_msgctl(msqid: i32, cmd: i32, buf: *mut MsqidDs) -> SyscallResult {
    let mut reg = MSG_REGISTRY.lock();
    let mq_lock = match reg.get(&msqid).cloned() {
        Some(q) => q,
        None => return Errno::EINVAL.into(),
    };

    match cmd {
        IPC_RMID => {
            reg.remove(&msqid);
            0
        }
        IPC_STAT => {
            if buf.is_null()
                || validate_user_ptr_write(buf as *mut u8, core::mem::size_of::<MsqidDs>()).is_err()
            {
                return Errno::EFAULT.into();
            }
            let q = mq_lock.lock();
            // SAFETY: Validated user ptr
            unsafe { core::ptr::write(buf, q.ds) };
            0
        }
        _ => 0,
    }
}

// ---------------- POSIX Message Queues ----------------

pub struct PosixMq {
    pub name: String,
    pub flags: i32,
    pub maxmsg: i64,
    pub msgsize: i64,
    pub messages: Vec<(u32, Vec<u8>)>,
    pub wait_queue: Arc<WaitQueue>,
}

static POSIX_MQ_NEXT_FD: TicketLock<i32> = TicketLock::new(100);
static POSIX_MQ_REGISTRY: TicketLock<BTreeMap<i32, Arc<TicketLock<PosixMq>>>> =
    TicketLock::new(BTreeMap::new());

/// `mq_open(name, oflag, mode, attr)` — open a message queue.
pub fn sys_mq_open(name_ptr: *const u8, oflag: i32, _mode: u32, _attr: *const u8) -> SyscallResult {
    let name = match unsafe { copy_string_from_user(name_ptr) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };

    let fd = {
        let mut nfd = POSIX_MQ_NEXT_FD.lock();
        let cur = *nfd;
        *nfd = cur + 1;
        cur
    };

    let mq = Arc::new(TicketLock::new(PosixMq {
        name,
        flags: oflag,
        maxmsg: 10,
        msgsize: 8192,
        messages: Vec::new(),
        wait_queue: Arc::new(WaitQueue::new()),
    }));

    POSIX_MQ_REGISTRY.lock().insert(fd, mq);
    fd as SyscallResult
}

/// `mq_unlink(name)` — remove a message queue.
pub fn sys_mq_unlink(name_ptr: *const u8) -> SyscallResult {
    let name = match unsafe { copy_string_from_user(name_ptr) } {
        Some(n) => n,
        None => return Errno::EFAULT.into(),
    };

    let mut reg = POSIX_MQ_REGISTRY.lock();
    let mut to_remove = None;
    for (&fd, mq_lock) in reg.iter() {
        if mq_lock.lock().name == name {
            to_remove = Some(fd);
            break;
        }
    }

    if let Some(fd) = to_remove {
        reg.remove(&fd);
        0
    } else {
        Errno::ENOENT.into()
    }
}

/// `mq_timedsend(mqdes, msg_ptr, msg_len, msg_prio, abs_timeout)` — send message to a message queue.
pub fn sys_mq_timedsend(
    mqdes: i32,
    msg_ptr: *const u8,
    msg_len: usize,
    msg_prio: u32,
    _abs_timeout: *const u8,
) -> SyscallResult {
    if msg_ptr.is_null() || !validate_user_ptr(msg_ptr, msg_len) {
        return Errno::EFAULT.into();
    }
    let data = unsafe { core::slice::from_raw_parts(msg_ptr, msg_len) };

    let mq_arc = {
        let reg = POSIX_MQ_REGISTRY.lock();
        match reg.get(&mqdes).cloned() {
            Some(q) => q,
            None => return Errno::EBADF.into(),
        }
    };

    let mut mq = mq_arc.lock();
    mq.messages.push((msg_prio, data.to_vec()));
    mq.wait_queue.wake_all();
    0
}

/// `mq_timedreceive(mqdes, msg_ptr, msg_len, msg_prio, abs_timeout)` — receive a message from a message queue.
pub fn sys_mq_timedreceive(
    mqdes: i32,
    msg_ptr: *mut u8,
    msg_len: usize,
    msg_prio: *mut u32,
    _abs_timeout: *const u8,
) -> SyscallResult {
    if msg_ptr.is_null() || validate_user_ptr_write(msg_ptr, msg_len).is_err() {
        return Errno::EFAULT.into();
    }

    let mq_arc = {
        let reg = POSIX_MQ_REGISTRY.lock();
        match reg.get(&mqdes).cloned() {
            Some(q) => q,
            None => return Errno::EBADF.into(),
        }
    };

    let mut mq = mq_arc.lock();
    if mq.messages.is_empty() {
        return -(Errno::EAGAIN as i64);
    }

    let (prio, data) = mq.messages.remove(0);
    let copy_len = data.len().min(msg_len);
    // SAFETY: Buffer validated for write
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), msg_ptr, copy_len);
        if !msg_prio.is_null() && validate_user_ptr_write(msg_prio as *mut u8, 4).is_ok() {
            core::ptr::write(msg_prio, prio);
        }
    }
    copy_len as SyscallResult
}

/// `mq_notify(mqdes, notification)` — register for notification when a message arrives.
pub fn sys_mq_notify(mqdes: i32, _notification: *const u8) -> SyscallResult {
    let reg = POSIX_MQ_REGISTRY.lock();
    if !reg.contains_key(&mqdes) {
        return Errno::EBADF.into();
    }
    0
}

/// `mq_getsetattr(mqdes, newattr, oldattr)` — get/set message queue attributes.
pub fn sys_mq_getsetattr(mqdes: i32, _newattr: *const u8, _oldattr: *mut u8) -> SyscallResult {
    let reg = POSIX_MQ_REGISTRY.lock();
    if !reg.contains_key(&mqdes) {
        return Errno::EBADF.into();
    }
    0
}
