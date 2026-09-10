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

//! Kernel namespace infrastructure for container isolation.
//!
//! Implements per-task filesystem context (`FsContext`), mount namespace
//! (`MountNamespace`), and UTS namespace (`UtsNamespace`).
//!
//! ## Design
//!
//! Each `Task` owns an `Arc<spin::RwLock<FsContext>>` that tracks its:
//!
//! - **Filesystem root** (`root`): the jailed root visible to the task.
//!   `..` traversal at this boundary is clamped in `fs::path::normalize_jailed`.
//! - **Mount namespace** (`mount_ns`): an `Arc<spin::RwLock<MountNamespace>>`
//!   that holds the task's private or shared mount table.
//!
//! On `fork()` / `clone()` *without* `CLONE_NEWNS`, the child inherits
//! the parent's `Arc<RwLock<FsContext>>` but gets its own `Arc` clone —
//! meaning the mount namespace `Arc` is shared (COW semantics are enforced
//! by `sys_unshare`).
//!
//! On `unshare(CLONE_NEWNS)`, the task atomically replaces its `mount_ns`
//! with a deep clone of the current mount table, receiving a private namespace.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::RwLock;

use super::vfs::FileSystem;

/// Monotonically increasing namespace ID counter.
static NEXT_NS_ID: AtomicU64 = AtomicU64::new(1);

/// Monotonically increasing PID namespace ID counter.
static NEXT_PID_NS_ID: AtomicU64 = AtomicU64::new(1);

fn alloc_ns_id() -> u64 {
    NEXT_NS_ID.fetch_add(1, Ordering::Relaxed)
}

/// Allocate a fresh PID namespace ID.
pub fn alloc_pid_ns_id() -> u64 {
    NEXT_PID_NS_ID.fetch_add(1, Ordering::Relaxed)
}

/// The initial (host) PID namespace ID.
/// All tasks start in this namespace.
pub const INITIAL_PID_NS_ID: u64 = 0;

// ─── Mount Namespace ─────────────────────────────────────────────────────────

/// A per-task mount namespace.
///
/// Contains the complete mount table: an ordered mapping from mountpoint
/// path strings to filesystem instances.  The global VFS shares its mount
/// table through the initial `MountNamespace` created at boot.
pub struct MountNamespace {
    /// Unique namespace identifier (used for debugging and `nsfs`).
    pub id: u64,
    /// Mount table: canonical mount-point path → filesystem.
    pub mounts: BTreeMap<String, Arc<dyn FileSystem>>,
}

impl MountNamespace {
    /// Create a new, empty mount namespace.
    pub fn new() -> Self {
        Self {
            id: alloc_ns_id(),
            mounts: BTreeMap::new(),
        }
    }

    /// Create a deep clone of this namespace (used by `unshare(CLONE_NEWNS)`).
    ///
    /// The new namespace gets a fresh `id`. The filesystem `Arc`s are
    /// *shared* (i.e., the same filesystem driver instance is reused),
    /// but the mount table itself (`BTreeMap`) is independent, so future
    /// `mount`/`umount` operations on the child do not affect the parent.
    pub fn fork(&self) -> Self {
        Self {
            id: alloc_ns_id(),
            mounts: self.mounts.clone(),
        }
    }

    /// Find the filesystem that best handles `path` (longest prefix match).
    ///
    /// Returns `(filesystem, remaining_path_within_fs)` or `None` if no
    /// mount point covers the path.
    pub fn resolve_mount(&self, path: &str) -> Option<(Arc<dyn FileSystem>, String)> {
        let mut best: Option<(&str, &Arc<dyn FileSystem>)> = None;
        for (mount_path, fs) in &self.mounts {
            if path.starts_with(mount_path.as_str()) {
                let dominated = best.map_or(true, |(b, _)| mount_path.len() > b.len());
                if dominated {
                    best = Some((mount_path.as_str(), fs));
                }
            }
        }
        best.map(|(mount_path, fs)| {
            let remaining = &path[mount_path.len()..];
            let remaining = if remaining.is_empty() { "/" } else { remaining };
            (fs.clone(), String::from(remaining))
        })
    }
}

// ─── UTS Namespace ───────────────────────────────────────────────────────────

/// A per-task UTS (UNIX Time-sharing System) namespace.
///
/// Stores the hostname and domain name visible to a task.  On `fork`, the
/// child inherits a clone of the parent's UTS namespace.  On
/// `unshare(CLONE_NEWUTS)`, the task gets its own copy that can be modified
/// independently via `sethostname(2)` / `setdomainname(2)`.
#[derive(Clone)]
pub struct UtsNamespace {
    /// Hostname returned by `uname(2)` and `gethostname(2)`.
    pub hostname: String,
    /// NIS domain name returned by `getdomainname(2)`.
    pub domainname: String,
}

impl UtsNamespace {
    /// Create the default system UTS namespace used at boot.
    pub fn default_ns() -> Self {
        Self {
            hostname: String::from("kontsnoros"),
            domainname: String::from("(none)"),
        }
    }
}

// ─── Filesystem Context ───────────────────────────────────────────────────────

/// Per-task filesystem context.
///
/// Combines the task's jail root (`root`) with the mount namespace it uses.
/// The `cwd` field is kept on the `Task` itself for historical compatibility
/// (many syscalls read `task.cwd` directly); `FsContext` tracks only the
/// isolation boundaries.
pub struct FsContext {
    /// Jailed filesystem root for this task.
    ///
    /// Path resolution is clamped at this prefix: a `..` component that
    /// would pop above `root` is silently clamped to `root` itself.
    /// Defaults to `"/"` (no jail).
    pub root: String,

    /// The mount namespace used by this task.
    ///
    /// Shared with siblings until `unshare(CLONE_NEWNS)` is called, after
    /// which the task receives a private clone.
    pub mount_ns: Arc<RwLock<MountNamespace>>,
}

impl FsContext {
    /// Create the initial `FsContext` used by PID 1 and all tasks that have
    /// not called `unshare(CLONE_NEWNS)` or `chroot`.
    pub fn new_initial(mount_ns: Arc<RwLock<MountNamespace>>) -> Self {
        Self {
            root: String::from("/"),
            mount_ns,
        }
    }

    /// Clone this context for a child task (fork / clone without `CLONE_NEWNS`).
    ///
    /// The child shares the *same* `MountNamespace` `Arc` as the parent.
    /// The `root` string is copied so that a subsequent `chroot` in the child
    /// does not affect the parent.
    pub fn fork(&self) -> Self {
        Self {
            root: self.root.clone(),
            mount_ns: self.mount_ns.clone(),
        }
    }
}

// ─── Global initial mount namespace ──────────────────────────────────────────

/// The initial (system-wide) mount namespace.
///
/// All tasks share this namespace until they call `unshare(CLONE_NEWNS)`.
/// The VFS mount/unmount functions operate on this namespace.
pub static INITIAL_MOUNT_NS: spin::Lazy<Arc<RwLock<MountNamespace>>> =
    spin::Lazy::new(|| Arc::new(RwLock::new(MountNamespace::new())));

/// Register a filesystem mount in the initial (global) mount namespace.
pub fn global_mount(path: String, fs: Arc<dyn FileSystem>) {
    INITIAL_MOUNT_NS.write().mounts.insert(path, fs);
}

/// Unregister a filesystem mount from the initial (global) mount namespace.
/// Returns `true` if the entry was present.
pub fn global_unmount(path: &str) -> bool {
    INITIAL_MOUNT_NS.write().mounts.remove(path).is_some()
}
