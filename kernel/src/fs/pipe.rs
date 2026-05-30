//! POSIX-compatible unidirectional pipes.
//!
//! A pipe is a unidirectional data channel that can be used for
//! interprocess communication. It is backed by a ring-buffer in memory.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::fs::inode::{DirEntry, FileType, Inode, InodeOps};

const PIPE_BUF_SIZE: usize = 8192;

/// Circular queue for pipe data.
struct PipeBuffer {
    data: [u8; PIPE_BUF_SIZE],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl PipeBuffer {
    fn new() -> Self {
        Self {
            data: [0u8; PIPE_BUF_SIZE],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.len == PIPE_BUF_SIZE
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, byte: u8) -> bool {
        if self.len < PIPE_BUF_SIZE {
            self.data[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % PIPE_BUF_SIZE;
            self.len += 1;
            true
        } else {
            false
        }
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.data[self.read_pos];
        self.read_pos = (self.read_pos + 1) % PIPE_BUF_SIZE;
        self.len -= 1;
        Some(byte)
    }
}

/// Shared state of a unidirectional pipe.
pub struct PipeState {
    buffer: Mutex<PipeBuffer>,
    readers: AtomicUsize,
    writers: AtomicUsize,
}

impl PipeState {
    fn new() -> Self {
        Self {
            buffer: Mutex::new(PipeBuffer::new()),
            readers: AtomicUsize::new(1),
            writers: AtomicUsize::new(1),
        }
    }
}

/// Read end of the pipe.
pub struct PipeReader {
    inode: Inode,
    state: Arc<PipeState>,
}

impl InodeOps for PipeReader {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            {
                let mut guard = self.state.buffer.lock();
                if !guard.is_empty() {
                    let mut count = 0;
                    while count < buf.len() {
                        if let Some(byte) = guard.pop() {
                            buf[count] = byte;
                            count += 1;
                        } else {
                            break;
                        }
                    }
                    return Ok(count);
                }
            }

            // Buffer is empty. Check if any writers are left.
            if self.state.writers.load(Ordering::SeqCst) == 0 {
                return Ok(0); // EOF
            }

            // Yield and block cooperatively
            crate::process::scheduler::yield_now();
        }
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.state.readers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Write end of the pipe.
pub struct PipeWriter {
    inode: Inode,
    state: Arc<PipeState>,
}

impl InodeOps for PipeWriter {
    fn inode(&self) -> &Inode {
        &self.inode
    }

    fn write(&self, _offset: u64, data: &[u8]) -> Result<usize, i32> {
        if data.is_empty() {
            return Ok(0);
        }

        let mut written = 0;
        while written < data.len() {
            // Check if readers are closed
            if self.state.readers.load(Ordering::SeqCst) == 0 {
                return Err(-32); // EPIPE
            }

            let space_available = {
                let mut guard = self.state.buffer.lock();
                if !guard.is_full() {
                    while written < data.len() && !guard.is_full() {
                        if guard.push(data[written]) {
                            written += 1;
                        } else {
                            break;
                        }
                    }
                    true
                } else {
                    false
                }
            };

            if !space_available {
                // Yield and block cooperatively until space is freed
                crate::process::scheduler::yield_now();
            }
        }

        Ok(written)
    }

    fn readdir(&self) -> Vec<DirEntry> {
        Vec::new()
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.state.writers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Construct a new unidirectional VFS pipe, returning (Reader, Writer) tuple.
pub fn make_pipe() -> (Arc<dyn InodeOps>, Arc<dyn InodeOps>) {
    let state = Arc::new(PipeState::new());
    
    let reader = Arc::new(PipeReader {
        inode: Inode::new(0, FileType::Pipe),
        state: state.clone(),
    });

    let writer = Arc::new(PipeWriter {
        inode: Inode::new(0, FileType::Pipe),
        state,
    });

    (reader, writer)
}
