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

//! Crypto & PRNG subsystem unit & regression tests.

#[test_case]
fn test_crypto_prng() {
    // 1. Reset PRNG state to test unseeded / no-entropy state
    crate::crypto::prng::reset_for_test();
    let mut unseeded_buf = [0xAAu8; 32];
    let res_unseeded = crate::crypto::prng::fill_bytes(&mut unseeded_buf);
    assert!(
        !res_unseeded,
        "fill_bytes should return false when PRNG has no entropy"
    );
    assert_eq!(
        unseeded_buf, [0xAAu8; 32],
        "Destination slice must remain unmodified when fill_bytes fails"
    );

    // 2. Seed PRNG with initial entropy key
    let seed_key = [0x42u8; 32];
    crate::crypto::prng::seed(&seed_key);

    // 3. Test small buffer fill and mutation verification
    let mut small_buf = [0u8; 16];
    let res_seeded = crate::crypto::prng::fill_bytes(&mut small_buf);
    assert!(
        res_seeded,
        "fill_bytes should return true after PRNG is seeded"
    );
    assert_ne!(
        small_buf, [0u8; 16],
        "Destination slice must be mutated with random bytes"
    );

    // 4. Test distinct, non-repetitive random output across consecutive calls
    let mut buf_a = [0u8; 32];
    let mut buf_b = [0u8; 32];
    assert!(crate::crypto::prng::fill_bytes(&mut buf_a));
    assert!(crate::crypto::prng::fill_bytes(&mut buf_b));
    assert_ne!(
        buf_a, buf_b,
        "Consecutive PRNG byte fills must produce distinct random output"
    );

    // 5. Test multi-block generation (> 64 bytes) to test ChaCha20 block generation and buffer index wrapping
    let mut large_buf = [0u8; 128];
    assert!(crate::crypto::prng::fill_bytes(&mut large_buf));
    // Verify first block (0..64) and second block (64..128) are non-zero and non-identical
    assert_ne!(&large_buf[0..64], &large_buf[64..128]);

    // 6. Test reseed functionality
    let reseed_entropy = [0x99u8; 32];
    crate::crypto::prng::reseed(&reseed_entropy);
    let mut reseeded_buf = [0u8; 32];
    assert!(crate::crypto::prng::fill_bytes(&mut reseeded_buf));
    assert_ne!(reseeded_buf, [0u8; 32]);
}

#[test_case]
fn test_prng_seed_initialization() {
    // 1. Seed the PRNG with initial 32-byte entropy key
    let seed1: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    crate::crypto::prng::seed(&seed1);

    // 2. Verify fill_bytes returns true after seeding
    let mut buf1 = [0u8; 32];
    let ok = crate::crypto::prng::fill_bytes(&mut buf1);
    assert!(ok, "fill_bytes should return true after PRNG is seeded");

    // Ensure the generated random bytes are not all zeros
    assert_ne!(buf1, [0u8; 32], "PRNG output should non-zero");

    // 3. Verify deterministic generation for identical seed initialization
    crate::crypto::prng::seed(&seed1);
    let mut buf2 = [0u8; 32];
    let ok2 = crate::crypto::prng::fill_bytes(&mut buf2);
    assert!(ok2);
    assert_eq!(
        buf1, buf2,
        "Identical seeds must produce identical initial output blocks"
    );

    // 4. Verify re-seeding / different seed initialization changes output sequence
    let seed2: [u8; 32] = [0xff; 32];
    crate::crypto::prng::seed(&seed2);
    let mut buf3 = [0u8; 32];
    let ok3 = crate::crypto::prng::fill_bytes(&mut buf3);
    assert!(ok3);
    assert_ne!(
        buf1, buf3,
        "Different seeds must produce different output blocks"
    );
}
