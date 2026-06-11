//! Cryptographically secure pseudo-random number generator (CSPRNG) for KontsnorOS.

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

// ChaCha20 core state
struct ChaCha20 {
    state: [u32; 16],
}

impl ChaCha20 {
    fn new(key: &[u8; 32], nonce: &[u8; 12]) -> Self {
        let mut state = [0u32; 16];
        state[0] = 0x61707865;
        state[1] = 0x3320646e;
        state[2] = 0x79622d32;
        state[3] = 0x6b206574;
        for i in 0..8 {
            state[4 + i] = u32::from_le_bytes([
                key[i * 4],
                key[i * 4 + 1],
                key[i * 4 + 2],
                key[i * 4 + 3],
            ]);
        }
        state[12] = 0; // counter
        for i in 0..3 {
            state[13 + i] = u32::from_le_bytes([
                nonce[i * 4],
                nonce[i * 4 + 1],
                nonce[i * 4 + 2],
                nonce[i * 4 + 3],
            ]);
        }
        Self { state }
    }

    fn block(&mut self) -> [u32; 16] {
        let mut x = self.state;
        for _ in 0..10 { // 20 rounds
            // Column rounds
            x[0] = x[0].wrapping_add(x[4]); x[12] = (x[12] ^ x[0]).rotate_left(16);
            x[8] = x[8].wrapping_add(x[12]); x[4] = (x[4] ^ x[8]).rotate_left(12);
            x[0] = x[0].wrapping_add(x[4]); x[12] = (x[12] ^ x[0]).rotate_left(8);
            x[8] = x[8].wrapping_add(x[12]); x[4] = (x[4] ^ x[8]).rotate_left(7);

            x[1] = x[1].wrapping_add(x[5]); x[13] = (x[13] ^ x[1]).rotate_left(16);
            x[9] = x[9].wrapping_add(x[13]); x[5] = (x[5] ^ x[9]).rotate_left(12);
            x[1] = x[1].wrapping_add(x[5]); x[13] = (x[13] ^ x[1]).rotate_left(8);
            x[9] = x[9].wrapping_add(x[13]); x[5] = (x[5] ^ x[9]).rotate_left(7);

            x[2] = x[2].wrapping_add(x[6]); x[14] = (x[14] ^ x[2]).rotate_left(16);
            x[10] = x[10].wrapping_add(x[14]); x[6] = (x[6] ^ x[10]).rotate_left(12);
            x[2] = x[2].wrapping_add(x[6]); x[14] = (x[14] ^ x[2]).rotate_left(8);
            x[10] = x[10].wrapping_add(x[14]); x[6] = (x[6] ^ x[10]).rotate_left(7);

            x[3] = x[3].wrapping_add(x[7]); x[15] = (x[15] ^ x[3]).rotate_left(16);
            x[11] = x[11].wrapping_add(x[15]); x[7] = (x[7] ^ x[11]).rotate_left(12);
            x[3] = x[3].wrapping_add(x[7]); x[15] = (x[15] ^ x[3]).rotate_left(8);
            x[11] = x[11].wrapping_add(x[15]); x[7] = (x[7] ^ x[11]).rotate_left(7);

            // Diagonal rounds
            x[0] = x[0].wrapping_add(x[5]); x[15] = (x[15] ^ x[0]).rotate_left(16);
            x[10] = x[10].wrapping_add(x[15]); x[5] = (x[5] ^ x[10]).rotate_left(12);
            x[0] = x[0].wrapping_add(x[5]); x[15] = (x[15] ^ x[0]).rotate_left(8);
            x[10] = x[10].wrapping_add(x[15]); x[5] = (x[5] ^ x[10]).rotate_left(7);

            x[1] = x[1].wrapping_add(x[6]); x[12] = (x[12] ^ x[1]).rotate_left(16);
            x[11] = x[11].wrapping_add(x[12]); x[6] = (x[6] ^ x[11]).rotate_left(12);
            x[1] = x[1].wrapping_add(x[6]); x[12] = (x[12] ^ x[1]).rotate_left(8);
            x[11] = x[11].wrapping_add(x[12]); x[6] = (x[6] ^ x[11]).rotate_left(7);

            x[2] = x[2].wrapping_add(x[7]); x[13] = (x[13] ^ x[2]).rotate_left(16);
            x[8] = x[8].wrapping_add(x[13]); x[7] = (x[7] ^ x[8]).rotate_left(12);
            x[2] = x[2].wrapping_add(x[7]); x[13] = (x[13] ^ x[2]).rotate_left(8);
            x[8] = x[8].wrapping_add(x[13]); x[7] = (x[7] ^ x[8]).rotate_left(7);

            x[3] = x[3].wrapping_add(x[4]); x[14] = (x[14] ^ x[3]).rotate_left(16);
            x[9] = x[9].wrapping_add(x[14]); x[4] = (x[4] ^ x[9]).rotate_left(12);
            x[3] = x[3].wrapping_add(x[4]); x[14] = (x[14] ^ x[3]).rotate_left(8);
            x[9] = x[9].wrapping_add(x[14]); x[4] = (x[4] ^ x[9]).rotate_left(7);
        }
        for i in 0..16 {
            x[i] = x[i].wrapping_add(self.state[i]);
        }
        self.state[12] = self.state[12].wrapping_add(1); // increment counter
        x
    }
}

pub struct PrngState {
    chacha: ChaCha20,
    buffer: [u8; 64],
    buf_idx: usize,
}

static PRNG: Mutex<Option<PrngState>> = Mutex::new(None);
static HAS_ENTROPY: AtomicBool = AtomicBool::new(false);

/// Seed the PRNG with initial entropy.
pub fn seed(initial_entropy: &[u8; 32]) {
    let mut lock = PRNG.lock();
    let nonce = [0u8; 12];
    *lock = Some(PrngState {
        chacha: ChaCha20::new(initial_entropy, &nonce),
        buffer: [0u8; 64],
        buf_idx: 64,
    });
    HAS_ENTROPY.store(true, Ordering::SeqCst);
}

/// Periodic reseed to mix in fresh entropy.
pub fn reseed(entropy: &[u8; 32]) {
    let mut lock = PRNG.lock();
    if let Some(ref mut prng) = *lock {
        // Read 32 bytes from current generator to make new key, mixed with new entropy
        let mut new_key = [0u8; 32];
        for i in 0..32 {
            if prng.buf_idx >= 64 {
                let block = prng.chacha.block();
                for (j, &word) in block.iter().enumerate() {
                    prng.buffer[j * 4..j * 4 + 4].copy_from_slice(&word.to_le_bytes());
                }
                prng.buf_idx = 0;
            }
            new_key[i] = prng.buffer[prng.buf_idx] ^ entropy[i];
            prng.buf_idx += 1;
        }
        let nonce = [0u8; 12];
        prng.chacha = ChaCha20::new(&new_key, &nonce);
    }
}

/// Fill the buffer with cryptographically secure random bytes.
/// Returns false if not enough entropy has been ingested yet.
pub fn fill_bytes(dest: &mut [u8]) -> bool {
    if !HAS_ENTROPY.load(Ordering::SeqCst) {
        return false;
    }
    let mut lock = PRNG.lock();
    if let Some(ref mut prng) = *lock {
        for b in dest.iter_mut() {
            if prng.buf_idx >= 64 {
                let block = prng.chacha.block();
                for (j, &word) in block.iter().enumerate() {
                    prng.buffer[j * 4..j * 4 + 4].copy_from_slice(&word.to_le_bytes());
                }
                prng.buf_idx = 0;
            }
            *b = prng.buffer[prng.buf_idx];
            prng.buf_idx += 1;
        }
        true
    } else {
        false
    }
}

/// Seed the PRNG using available hardware and system configuration entropy sources.
pub fn init_entropy(boot_info: &bootloader_api::BootInfo) {
    let mut entropy = [0u8; 32];
    
    // 1. Timestamp counter (RDTSC)
    let rdtsc = unsafe { core::arch::x86_64::_rdtsc() };
    entropy[0..8].copy_from_slice(&rdtsc.to_le_bytes());
    
    // 2. APIC ID
    let apic_id = crate::arch::x86_64::smp::current_lapic_id();
    entropy[8] = apic_id;
    
    // 3. PIT Channel 0 counter read
    let pit_val = unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x40);
        port.read()
    };
    entropy[9] = pit_val;
    
    // 4. Checksum of bootloader memory regions
    let mut mem_hash = 0u64;
    for region in boot_info.memory_regions.iter() {
        mem_hash = mem_hash.wrapping_add(region.start);
        mem_hash = mem_hash.wrapping_add(region.end);
        let kind_val = match region.kind {
            bootloader_api::info::MemoryRegionKind::Usable => 1u64,
            bootloader_api::info::MemoryRegionKind::Bootloader => 2u64,
            _ => 3u64,
        };
        mem_hash = mem_hash.wrapping_add(kind_val);
    }
    entropy[10..18].copy_from_slice(&mem_hash.to_le_bytes());
    
    // 5. RDRAND if supported
    let mut has_rdrand = false;
    let mut ecx_val: u32 = 0;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "mov {0:e}, ecx",
            "pop rbx",
            out(reg) ecx_val,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
        );
    }
    if (ecx_val & (1 << 30)) != 0 {
        has_rdrand = true;
    }

    let mut rdrand_val = 0u64;
    let mut success = 0u32;
    if has_rdrand {
        unsafe {
            core::arch::asm!(
                "rdrand {0}",
                "mov {1:e}, 1",
                "jc 2f",
                "mov {1:e}, 0",
                "2:",
                out(reg) rdrand_val,
                out(reg) success,
            );
        }
    }
    if success != 0 {
        entropy[18..26].copy_from_slice(&rdrand_val.to_le_bytes());
    } else {
        // Fallback mixing
        let fallback = rdtsc.wrapping_mul(0x5851f42d4c957f2d).wrapping_add(1);
        entropy[18..26].copy_from_slice(&fallback.to_le_bytes());
    }
    
    // 6. Rest of bytes mixed with rdtsc + pit
    let rest = rdtsc.wrapping_add(pit_val as u64).wrapping_add(apic_id as u64);
    entropy[26..32].copy_from_slice(&rest.to_le_bytes()[0..6]);
    
    seed(&entropy);
}
