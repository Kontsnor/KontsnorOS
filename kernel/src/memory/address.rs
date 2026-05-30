//! Type-safe physical and virtual address wrappers.
//!
//! These types prevent accidentally mixing up physical and virtual addresses,
//! which is a common source of bugs in kernel development.

use core::fmt;

/// A physical memory address.
///
/// Physical addresses are used to reference actual locations in RAM
/// and are used by the MMU for page table entries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysAddr(u64);

impl PhysAddr {
    /// Create a new physical address.
    ///
    /// On x86_64, physical addresses are limited to the lower 52 bits.
    pub const fn new(addr: u64) -> Self {
        // x86_64 supports up to 52-bit physical addresses
        debug_assert!(addr & 0xFFF0_0000_0000_0000 == 0, "PhysAddr exceeds 52 bits");
        Self(addr)
    }

    /// Get the raw u64 value of this address.
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Check if this address is page-aligned (4 KiB boundary).
    pub const fn is_aligned(self) -> bool {
        self.0 % super::PAGE_SIZE as u64 == 0
    }

    /// Align this address down to the nearest page boundary.
    pub const fn align_down(self) -> Self {
        Self(self.0 & !(super::PAGE_SIZE as u64 - 1))
    }

    /// Align this address up to the nearest page boundary.
    pub const fn align_up(self) -> Self {
        Self((self.0 + super::PAGE_SIZE as u64 - 1) & !(super::PAGE_SIZE as u64 - 1))
    }
}

impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhysAddr({:#x})", self.0)
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl core::ops::Add<u64> for PhysAddr {
    type Output = Self;
    fn add(self, rhs: u64) -> Self {
        Self::new(self.0 + rhs)
    }
}

impl core::ops::Sub<PhysAddr> for PhysAddr {
    type Output = u64;
    fn sub(self, rhs: PhysAddr) -> u64 {
        self.0 - rhs.0
    }
}

/// A virtual memory address.
///
/// Virtual addresses are used by the CPU and are translated to physical
/// addresses through the page tables.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtAddr(u64);

impl VirtAddr {
    /// Create a new virtual address.
    ///
    /// On x86_64, virtual addresses must be in canonical form:
    /// bits 48–63 must be copies of bit 47.
    pub const fn new(addr: u64) -> Self {
        // Enforce canonical form
        let canonical = ((addr as i64) << 16 >> 16) as u64;
        debug_assert!(
            addr == canonical,
            "VirtAddr is not in canonical form"
        );
        Self(addr)
    }

    /// Create a new virtual address, truncating to canonical form.
    pub const fn new_truncate(addr: u64) -> Self {
        Self(((addr as i64) << 16 >> 16) as u64)
    }

    /// Get the raw u64 value of this address.
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Get this address as a pointer.
    pub const fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    /// Get this address as a mutable pointer.
    pub const fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    /// Check if this address is page-aligned.
    pub const fn is_aligned(self) -> bool {
        self.0 % super::PAGE_SIZE as u64 == 0
    }

    /// Align this address down to the nearest page boundary.
    pub const fn align_down(self) -> Self {
        Self(self.0 & !(super::PAGE_SIZE as u64 - 1))
    }

    /// Align this address up to the nearest page boundary.
    pub const fn align_up(self) -> Self {
        Self::new_truncate((self.0 + super::PAGE_SIZE as u64 - 1) & !(super::PAGE_SIZE as u64 - 1))
    }

    /// Extract the page table indices from this virtual address.
    ///
    /// Returns (L4 index, L3 index, L2 index, L1 index, page offset).
    pub const fn page_table_indices(self) -> (u16, u16, u16, u16, u16) {
        let addr = self.0;
        let l4 = ((addr >> 39) & 0x1FF) as u16;
        let l3 = ((addr >> 30) & 0x1FF) as u16;
        let l2 = ((addr >> 21) & 0x1FF) as u16;
        let l1 = ((addr >> 12) & 0x1FF) as u16;
        let offset = (addr & 0xFFF) as u16;
        (l4, l3, l2, l1, offset)
    }
}

impl fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VirtAddr({:#x})", self.0)
    }
}

impl fmt::Display for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl core::ops::Add<u64> for VirtAddr {
    type Output = Self;
    fn add(self, rhs: u64) -> Self {
        Self::new(self.0 + rhs)
    }
}

impl core::ops::Sub<VirtAddr> for VirtAddr {
    type Output = u64;
    fn sub(self, rhs: VirtAddr) -> u64 {
        self.0 - rhs.0
    }
}
