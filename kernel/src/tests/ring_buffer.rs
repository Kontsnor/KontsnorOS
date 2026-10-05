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

//! Lock-free ring buffer unit tests.

use crate::kprintln;
use crate::util::ring_buffer::RingBuffer;

#[test_case]
fn test_ring_buffer_comprehensive() {
    kprintln!("[test] Starting RingBuffer unit tests...");

    let rb: RingBuffer<u8, 8> = RingBuffer::new();
    assert!(rb.is_empty());
    assert_eq!(rb.len(), 0);

    // 1. Fill ring buffer to capacity (8 elements)
    for i in 0..8 {
        assert_eq!(rb.push(i as u8), Ok(()));
    }

    assert_eq!(rb.len(), 8);
    assert!(!rb.is_empty());

    // 2. Overfill test -> expect Err(item)
    assert_eq!(rb.push(99), Err(99));
    assert_eq!(rb.len(), 8);

    // 3. Pop elements and verify FIFO order
    for i in 0..8 {
        assert_eq!(rb.pop(), Some(i as u8));
    }

    // 4. Underflow / Zero-byte / Empty pop test -> expect None
    assert_eq!(rb.pop(), None);
    assert!(rb.is_empty());
    assert_eq!(rb.len(), 0);

    // 5. Interleaved push/pop and wrap-around index arithmetic
    for step in 0..256 {
        assert_eq!(rb.push((step & 0xFF) as u8), Ok(()));
        assert_eq!(rb.pop(), Some((step & 0xFF) as u8));
        assert!(rb.is_empty());
    }

    // 6. Partial fill and drain across boundary
    assert_eq!(rb.push(10), Ok(()));
    assert_eq!(rb.push(20), Ok(()));
    assert_eq!(rb.push(30), Ok(()));
    assert_eq!(rb.len(), 3);

    assert_eq!(rb.pop(), Some(10));
    assert_eq!(rb.pop(), Some(20));
    assert_eq!(rb.len(), 1);

    for i in 40..=46 {
        assert_eq!(rb.push(i as u8), Ok(()));
    }
    assert_eq!(rb.len(), 8);
    assert_eq!(rb.push(255), Err(255)); // Full

    assert_eq!(rb.pop(), Some(30));
    for i in 40..=46 {
        assert_eq!(rb.pop(), Some(i as u8));
    }
    assert_eq!(rb.pop(), None);
    assert!(rb.is_empty());

    // 7. Bulk slice operations (`push_slice` and `pop_slice`)
    let src = [10u8, 11, 12, 13, 14];
    assert_eq!(rb.push_slice(&src), 5);
    assert_eq!(rb.len(), 5);

    let mut dst = [0u8; 8];
    assert_eq!(rb.pop_slice(&mut dst[..3]), 3);
    assert_eq!(&dst[..3], &[10, 11, 12]);
    assert_eq!(rb.len(), 2);

    // Overfill push_slice (capacity = 8, 2 remaining -> pushes 6 bytes out of 8)
    let src2 = [20u8, 21, 22, 23, 24, 25, 26, 27];
    assert_eq!(rb.push_slice(&src2), 6);
    assert_eq!(rb.len(), 8);

    // Drain all 8 bytes via pop_slice across circular boundary
    let mut dst2 = [0u8; 8];
    assert_eq!(rb.pop_slice(&mut dst2), 8);
    assert_eq!(&dst2, &[13, 14, 20, 21, 22, 23, 24, 25]);
    assert!(rb.is_empty());

    kprintln!("[test] RingBuffer unit tests PASSED!");
}
