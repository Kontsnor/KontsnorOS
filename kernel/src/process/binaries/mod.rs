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

//! Embedded user-space ELF binaries for early boot and testing.

pub mod hello_elf;
pub mod net_test_elf;
pub mod shell_elf;

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
        0x02, 0x00, // e_type = ET_EXEC
        0x3e, 0x00, // e_machine = EM_X86_64
        0x01, 0x00, 0x00, 0x00, // e_version = 1
        0x78, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // e_entry = 0x400078
        0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_phoff = 64
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_shoff = 0
        0x00, 0x00, 0x00, 0x00, // e_flags = 0
        0x40, 0x00, // e_ehsize = 64
        0x38, 0x00, // e_phentsize = 56
        0x01, 0x00, // e_phnum = 1
        0x00, 0x00, // e_shentsize = 0
        0x00, 0x00, // e_shnum = 0
        0x00, 0x00, // e_shstrndx = 0
        // ── Program Header ───────────────────────────────────────────
        0x01, 0x00, 0x00, 0x00, // p_type = PT_LOAD
        0x05, 0x00, 0x00, 0x00, // p_flags = PF_R | PF_X
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_offset = 0
        0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // p_vaddr = 0x400000
        0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // p_paddr = 0x400000
        0x89, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_filesz = 137 bytes
        0x89, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_memsz = 137 bytes
        0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_align = 0x1000
        // ── Code Segment ─────────────────────────────────────────────
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, 39 (sys_getpid)
        0x0f, 0x05, // syscall
        0x48, 0x89, 0xc7, // mov rdi, rax (status = pid)
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, 60 (sys_exit)
        0x0f, 0x05, // syscall
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
