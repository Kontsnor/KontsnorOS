//! Synchronization primitives for KontsnorOS.
//!
//! Provides kernel-level synchronization mechanisms:
//! - Spinlock — busy-waiting lock for short critical sections
//! - Mutex — sleeping lock for longer critical sections
//! - RwLock — reader-writer lock for shared data

pub mod mutex;
pub mod rwlock;
pub mod spinlock;
