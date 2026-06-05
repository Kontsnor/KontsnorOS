//! ELF binary loader for user-space programs.
//!
//! This module parses and loads ELF64 binaries into a process's
//! address space, setting up the initial program state for execution.
//!
//! ## ELF Format Overview
//!
//! ```text
//! ┌─────────────────┐
//! │    ELF Header    │  ← Magic, entry point, program header offset
//! ├─────────────────┤
//! │ Program Headers  │  ← Segments to load (LOAD, DYNAMIC, etc.)
//! ├─────────────────┤
//! │   Section Data   │  ← .text, .data, .bss, .rodata, ...
//! ├─────────────────┤
//! │ Section Headers  │  ← Section metadata (optional for loading)
//! └─────────────────┘
//! ```

/// ELF64 magic number bytes.
pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// ELF class: 64-bit.
pub const ELFCLASS64: u8 = 2;

/// ELF data encoding: little-endian.
pub const ELFDATA2LSB: u8 = 1;

/// ELF OS/ABI: System V.
pub const ELFOSABI_NONE: u8 = 0;

/// ELF type: Executable.
pub const ET_EXEC: u16 = 2;
/// ELF type: Shared object (position-independent executable).
pub const ET_DYN: u16 = 3;

/// Machine type: x86_64.
pub const EM_X86_64: u16 = 0x3E;

/// Program header types.
pub const PT_NULL: u32 = 0;
/// Loadable segment.
pub const PT_LOAD: u32 = 1;
/// Dynamic linking information.
pub const PT_DYNAMIC: u32 = 2;
/// Interpreter path.
pub const PT_INTERP: u32 = 3;
/// Note section.
pub const PT_NOTE: u32 = 4;
/// Program header table.
pub const PT_PHDR: u32 = 6;
/// Thread-local storage.
pub const PT_TLS: u32 = 7;
/// GNU stack permissions.
pub const PT_GNU_STACK: u32 = 0x6474E551;

/// Segment permission flags.
pub const PF_X: u32 = 0x1; // Execute
/// Segment is writable.
pub const PF_W: u32 = 0x2; // Write
/// Segment is readable.
pub const PF_R: u32 = 0x4; // Read

/// ELF64 File Header.
///
/// This is at the very beginning of every ELF file.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Elf64Header {
    /// Magic number and identification.
    pub e_ident: [u8; 16],
    /// Object file type (ET_EXEC, ET_DYN, etc.).
    pub e_type: u16,
    /// Architecture (EM_X86_64).
    pub e_machine: u16,
    /// ELF version (always 1).
    pub e_version: u32,
    /// Entry point virtual address.
    pub e_entry: u64,
    /// Program header table offset.
    pub e_phoff: u64,
    /// Section header table offset.
    pub e_shoff: u64,
    /// Processor-specific flags.
    pub e_flags: u32,
    /// ELF header size.
    pub e_ehsize: u16,
    /// Program header entry size.
    pub e_phentsize: u16,
    /// Number of program headers.
    pub e_phnum: u16,
    /// Section header entry size.
    pub e_shentsize: u16,
    /// Number of section headers.
    pub e_shnum: u16,
    /// Section name string table index.
    pub e_shstrndx: u16,
}

/// ELF64 Program Header — describes a segment to load.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Elf64ProgramHeader {
    /// Segment type (PT_LOAD, PT_DYNAMIC, etc.).
    pub p_type: u32,
    /// Segment-dependent flags (PF_R, PF_W, PF_X).
    pub p_flags: u32,
    /// Offset of the segment in the file.
    pub p_offset: u64,
    /// Virtual address where the segment should be loaded.
    pub p_vaddr: u64,
    /// Physical address (unused on most systems).
    pub p_paddr: u64,
    /// Size of the segment in the file.
    pub p_filesz: u64,
    /// Size of the segment in memory (may be > filesz for .bss).
    pub p_memsz: u64,
    /// Alignment requirement.
    pub p_align: u64,
}

/// ELF64 Section Header.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Elf64SectionHeader {
    /// Section name (index into string table).
    pub sh_name: u32,
    /// Section type.
    pub sh_type: u32,
    /// Section flags.
    pub sh_flags: u64,
    /// Virtual address in memory.
    pub sh_addr: u64,
    /// Offset in file.
    pub sh_offset: u64,
    /// Size of section.
    pub sh_size: u64,
    /// Link to another section.
    pub sh_link: u32,
    /// Additional section information.
    pub sh_info: u32,
    /// Section alignment.
    pub sh_addralign: u64,
    /// Size of entries (for table sections).
    pub sh_entsize: u64,
}

/// Errors that can occur during ELF loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// Not a valid ELF file (bad magic number).
    InvalidMagic,
    /// Not a 64-bit ELF file.
    Not64Bit,
    /// Wrong endianness (we need little-endian).
    WrongEndian,
    /// Not an executable ELF file.
    NotExecutable,
    /// Wrong architecture (not x86_64).
    WrongArchitecture,
    /// File is too small to contain the declared structures.
    FileTooSmall,
    /// A loadable segment has invalid addresses.
    InvalidSegment,
    /// Out of memory while loading.
    OutOfMemory,
}

/// Information about a successfully parsed ELF binary.
#[derive(Debug, Clone)]
pub struct ElfInfo {
    /// Entry point virtual address.
    pub entry_point: u64,
    /// Loadable segments.
    pub segments: alloc::vec::Vec<LoadSegment>,
    /// Whether this is a position-independent executable.
    pub is_pie: bool,
    /// Stack size from PT_GNU_STACK (0 = use default).
    pub stack_size: u64,
    /// Program header table virtual address.
    pub phdr: u64,
    /// Number of program headers.
    pub phnum: u64,
    /// Size of program header entry.
    pub phent: u64,
}

/// A segment that needs to be loaded into memory.
#[derive(Debug, Clone)]
pub struct LoadSegment {
    /// Virtual address to load at.
    pub vaddr: u64,
    /// Offset in the ELF file.
    pub file_offset: u64,
    /// Size of data in the file.
    pub file_size: u64,
    /// Size in memory (file_size + zero-filled BSS area).
    pub mem_size: u64,
    /// Memory protection flags.
    pub flags: SegmentFlags,
}

/// Memory protection flags for a loaded segment.
#[derive(Debug, Clone, Copy)]
pub struct SegmentFlags {
    /// Segment is readable.
    pub read: bool,
    /// Segment is writable.
    pub write: bool,
    /// Segment is executable.
    pub execute: bool,
}

impl From<u32> for SegmentFlags {
    fn from(flags: u32) -> Self {
        Self {
            read: flags & PF_R != 0,
            write: flags & PF_W != 0,
            execute: flags & PF_X != 0,
        }
    }
}

/// Validate and parse an ELF64 binary.
///
/// Returns the entry point and loadable segments on success.
pub fn parse_elf(data: &[u8]) -> Result<ElfInfo, ElfError> {
    // Check minimum size for ELF header
    if data.len() < core::mem::size_of::<Elf64Header>() {
        return Err(ElfError::FileTooSmall);
    }

    // SAFETY: We verified the buffer is large enough for the header.
    let header = unsafe { &*(data.as_ptr() as *const Elf64Header) };

    // Validate ELF magic number
    if header.e_ident[0..4] != ELF_MAGIC {
        return Err(ElfError::InvalidMagic);
    }

    // Must be 64-bit
    if header.e_ident[4] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }

    // Must be little-endian
    if header.e_ident[5] != ELFDATA2LSB {
        return Err(ElfError::WrongEndian);
    }

    // Must be an executable or shared object (PIE)
    let e_type = header.e_type;
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(ElfError::NotExecutable);
    }

    // Must target x86_64
    if header.e_machine != EM_X86_64 {
        return Err(ElfError::WrongArchitecture);
    }

    let is_pie = e_type == ET_DYN;
    let entry_point = header.e_entry;
    let ph_offset = header.e_phoff as usize;
    let ph_entry_size = header.e_phentsize as usize;
    let ph_count = header.e_phnum as usize;

    // Validate program header table fits in the file
    let ph_end = ph_offset + ph_entry_size * ph_count;
    if ph_end > data.len() {
        return Err(ElfError::FileTooSmall);
    }

    let mut segments = alloc::vec::Vec::new();
    let mut stack_size: u64 = 0;

    // Parse program headers
    for i in 0..ph_count {
        let ph_start = ph_offset + i * ph_entry_size;

        // SAFETY: We verified bounds above.
        let phdr = unsafe {
            &*(data[ph_start..].as_ptr() as *const Elf64ProgramHeader)
        };

        match phdr.p_type {
            PT_LOAD => {
                let file_offset = phdr.p_offset;
                let file_size = phdr.p_filesz;
                let vaddr = phdr.p_vaddr;
                let mem_size = phdr.p_memsz;

                // Validate segment fits in the file
                if (file_offset + file_size) as usize > data.len() {
                    return Err(ElfError::InvalidSegment);
                }

                // Memory size must be >= file size
                if mem_size < file_size {
                    return Err(ElfError::InvalidSegment);
                }

                segments.push(LoadSegment {
                    vaddr,
                    file_offset,
                    file_size,
                    mem_size,
                    flags: SegmentFlags::from(phdr.p_flags),
                });
            }
            PT_GNU_STACK => {
                stack_size = phdr.p_memsz;
            }
            _ => {
                // Ignore other segment types for now
            }
        }
    }

    let e_phoff = header.e_phoff;
    let mut phdr_vaddr = 0;
    for segment in &segments {
        if e_phoff >= segment.file_offset && e_phoff < segment.file_offset + segment.file_size {
            phdr_vaddr = segment.vaddr + (e_phoff - segment.file_offset);
            break;
        }
    }
    if phdr_vaddr == 0 {
        phdr_vaddr = e_phoff;
    }

    Ok(ElfInfo {
        entry_point,
        segments,
        is_pie,
        stack_size,
        phdr: phdr_vaddr,
        phnum: ph_count as u64,
        phent: ph_entry_size as u64,
    })
}

/// Default user-space stack size (8 MiB, matching Linux default).
pub const DEFAULT_STACK_SIZE: u64 = 8 * 1024 * 1024;

/// Default user-space stack top address.
pub const USER_STACK_TOP: u64 = 0x0000_7FFF_FFFF_0000;

/// Lowest address for user-space mappings.
pub const USER_SPACE_BASE: u64 = 0x0000_0000_0040_0000;

/// Highest address for user-space (below kernel mapping).
pub const USER_SPACE_TOP: u64 = 0x0000_7FFF_FFFF_FFFF;

/// Safely copies a null-terminated array of null-terminated string pointers from user-space.
///
/// Uses the active page table (caller's context) to read pointers and strings.
pub unsafe fn copy_argv_from_user(mut argv_ptr: *const *const u8) -> Option<alloc::vec::Vec<alloc::string::String>> {
    if argv_ptr.is_null() {
        return Some(alloc::vec::Vec::new());
    }
    let mut args = alloc::vec::Vec::new();
    loop {
        let str_ptr = unsafe { argv_ptr.read_volatile() };
        if str_ptr.is_null() {
            break;
        }
        let s = unsafe { crate::syscall::fs::copy_string_from_user_pub(str_ptr) }?;
        args.push(s);
        argv_ptr = unsafe { argv_ptr.add(1) };
        if args.len() > 512 { // Sanity check to avoid infinite loops or huge allocations
            return None;
        }
    }
    Some(args)
}

/// Constructs a System V AMD64 ABI compliant user stack in the designated physical frame.
///
/// Places argc, argv pointer array, envp pointer array, terminating auxv, and the actual
/// string data onto a 16-byte aligned layout at the top of the stack.
pub fn construct_user_stack(
    argv: &[alloc::string::String],
    envp: &[alloc::string::String],
    phys_frame_addr: u64,
    entry_point: u64,
    phdr: u64,
    phnum: u64,
    phent: u64,
) -> Result<u64, crate::syscall::Errno> {
    let mut page_buf = alloc::vec![0u8; 4096];
    let mut str_pos = 4096;

    // Allocate 16 bytes for AT_RANDOM value at the top of the stack
    if str_pos < 16 {
        return Err(crate::syscall::Errno::E2BIG);
    }
    str_pos -= 16;
    for (i, b) in page_buf[str_pos..str_pos + 16].iter_mut().enumerate() {
        *b = (i as u8 + 42) ^ 0xAA;
    }
    let random_vaddr = (USER_STACK_TOP - 4096) + str_pos as u64;

    // 1. Copy environment strings
    let mut envp_vaddrs = alloc::vec::Vec::new();
    for env in envp.iter().rev() {
        let bytes = env.as_bytes();
        let len = bytes.len() + 1; // plus null terminator
        if len > str_pos {
            return Err(crate::syscall::Errno::E2BIG);
        }
        str_pos -= len;
        page_buf[str_pos..str_pos + bytes.len()].copy_from_slice(bytes);
        page_buf[str_pos + bytes.len()] = 0;
        envp_vaddrs.push((USER_STACK_TOP - 4096) + str_pos as u64);
    }
    envp_vaddrs.reverse();

    // 2. Copy argument strings
    let mut argv_vaddrs = alloc::vec::Vec::new();
    for arg in argv.iter().rev() {
        let bytes = arg.as_bytes();
        let len = bytes.len() + 1; // plus null terminator
        if len > str_pos {
            return Err(crate::syscall::Errno::E2BIG);
        }
        str_pos -= len;
        page_buf[str_pos..str_pos + bytes.len()].copy_from_slice(bytes);
        page_buf[str_pos + bytes.len()] = 0;
        argv_vaddrs.push((USER_STACK_TOP - 4096) + str_pos as u64);
    }
    argv_vaddrs.reverse();

    // Auxiliary vector entries required by musl-libc
    let auxv = [
        (3u64, phdr),          // AT_PHDR
        (4u64, phent),         // AT_PHENT
        (5u64, phnum),         // AT_PHNUM
        (6u64, 4096),          // AT_PAGESZ
        (9u64, entry_point),   // AT_ENTRY
        (11u64, 0),            // AT_UID
        (12u64, 0),            // AT_EUID
        (13u64, 0),            // AT_GID
        (14u64, 0),            // AT_EGID
        (23u64, 0),            // AT_SECURE
        (25u64, random_vaddr), // AT_RANDOM
        (0u64, 0),             // AT_NULL
    ];

    // 3. Calculate space for pointers:
    // pointers size = 8 (argc) + (argv.len() * 8) + 8 (null) + (envp.len() * 8) + 8 (null) + (auxv.len() * 16)
    let ptrs_size = 8 + (argv.len() * 8) + 8 + (envp.len() * 8) + 8 + (auxv.len() * 16);
    if ptrs_size > str_pos {
        return Err(crate::syscall::Errno::E2BIG);
    }

    // Align stack pointer (RSP) down to 16 bytes for System V ABI compliance
    let mut rsp_pos = str_pos - ptrs_size;
    rsp_pos = rsp_pos & !15;

    let mut write_pos = rsp_pos;

    // Helper to write a u64
    let write_u64 = |val: u64, buf: &mut [u8], pos: &mut usize| {
        buf[*pos..*pos + 8].copy_from_slice(&val.to_ne_bytes());
        *pos += 8;
    };

    // Write argc
    write_u64(argv.len() as u64, &mut page_buf, &mut write_pos);

    // Write argv pointers
    for vaddr in argv_vaddrs {
        write_u64(vaddr, &mut page_buf, &mut write_pos);
    }
    write_u64(0, &mut page_buf, &mut write_pos); // argv NULL

    // Write envp pointers
    for vaddr in envp_vaddrs {
        write_u64(vaddr, &mut page_buf, &mut write_pos);
    }
    write_u64(0, &mut page_buf, &mut write_pos); // envp NULL

    // Write auxiliary vector entries
    for (type_, val) in &auxv {
        write_u64(*type_, &mut page_buf, &mut write_pos);
        write_u64(*val, &mut page_buf, &mut write_pos);
    }

    // Copy constructed stack page to physical memory
    let dest = (phys_frame_addr + crate::memory::r#virtual::phys_mem_offset()) as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(page_buf.as_ptr(), dest, 4096);
    }

    let user_sp = (USER_STACK_TOP - 4096) + rsp_pos as u64;
    Ok(user_sp)
}

