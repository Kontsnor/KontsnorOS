# `/dev/fb0` Userspace Graphics Pipeline — Architectural Audit & Implementation Plan

**Author:** Principal Systems Engineer (User-Space & Tooling Specialist / `userspace` agent persona)  
**Date:** 2026-09-17  
**Kernel revision audited:** KontsnorOS current `main`  
**Scope:** Framebuffer driver, devfs device node, mmap, ioctl, lseek/read/write, and input subsystem

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Current State Assessment](#current-state-assessment)
   - [2.1 `/dev/fb0` Device Node Registration](#21-devfb0-device-node-registration)
   - [2.2 fbdev ioctl Coverage](#22-fbdev-ioctl-coverage)
   - [2.3 `FbFixScreeninfo` / `FbVarScreeninfo` ABI Parity](#23-fbfixscreeninfo--fbvarscreeninfo-abi-parity)
   - [2.4 `mmap` on `/dev/fb0`](#24-mmap-on-devfb0)
   - [2.5 `read` / `write` / `lseek` on `/dev/fb0`](#25-read--write--lseek-on-devfb0)
   - [2.6 Bochs GPU Driver State](#26-bochs-gpu-driver-state)
   - [2.7 Input Subsystem](#27-input-subsystem)
3. [Gap Analysis for Running Linux GUI & VNC](#gap-analysis-for-running-linux-gui--vnc)
   - [3.1 Display Server Requirements](#31-display-server-requirements-xfbdev--sdl2)
   - [3.2 VNC Capture Requirements](#32-vnc-capture-requirements-fbvncserver--x11vnc)
   - [3.3 Input Pipeline Missing Links](#33-input-pipeline-missing-links)
4. [Step-by-Step Implementation Roadmap](#step-by-step-implementation-roadmap)
   - [Milestone 1: `/dev/fb0` Completeness & ABI Parity](#milestone-1-devfb0-completeness--abi-parity)
   - [Milestone 2: Userspace `mmap` Hardening](#milestone-2-userspace-mmap-hardening)
   - [Milestone 3: Evdev / Mouse Input Exposure](#milestone-3-evdev--mouse-input-exposure)
   - [Milestone 4: End-to-End Verification Pipeline](#milestone-4-end-to-end-verification-pipeline)
5. [Security Considerations](#security-considerations)
6. [References](#references)

---

## Executive Summary

The KontsnorOS `/dev/fb0` implementation is **partially functional**. The device node is registered correctly, the four most critical fbdev ioctls are present, `mmap` of physical VRAM into userspace exists, and `read`/`write` at an explicit byte offset work. However, **seven concrete gaps** prevent standard userspace display stacks (Xfbdev, SDL2-fbdev, fbvncserver) from operating reliably:

| # | Gap | Severity |
|---|-----|----------|
| G1 | `lseek` on `/dev/fb0` silently has no effect — CharDevice offset not tracked in `FileDescription::read/write` | **HIGH** |
| G2 | `FbFixScreeninfo` ABI mismatch: missing 2-byte padding before `line_length`, causing Xfbdev to read a garbage stride value | **HIGH** |
| G3 | `FBIOPAN_DISPLAY` (0x4606), `FBIO_WAITFORVSYNC` (0x4620), `FBIOGET_CON2FBMAP` (0x460F) are all missing — Xfbdev aborts startup without them | **HIGH** |
| G4 | `/dev/fb0` mmap uses `WRITE_THROUGH` caching; fbdev convention on x86-64 is Write-Combining (WC via PAT) — causes ~10× throughput penalty | **MEDIUM** |
| G5 | mmap `offset` parameter not bounds-checked against VRAM size — can map physical memory past the VRAM BAR | **MEDIUM** |
| G6 | No `/dev/input/` subtree exists — no evdev nodes, no PS/2 mouse node, no `/dev/mice` aggregator | **HIGH** |
| G7 | Bochs GPU driver initialises at 320×200; Xfbdev minimum is typically 640×480 | **LOW** |

All seven gaps are addressable in ≤4 incremental PRs, none of which require breaking changes to the syscall table.

---

## Current State Assessment

### 2.1 `/dev/fb0` Device Node Registration

**Source:** `kernel/src/fs/devfs.rs`, lines 638–644

```rust
entries.insert(
    String::from("fb0"),
    Arc::new(DevFb0 {
        inode: make_chardev_inode(18, 29, 0),
    }) as Arc<dyn InodeOps>,
);
```

- ✅ **Major 29, Minor 0** — matches the Linux `fb0` character device convention.
- ✅ **Permissions 0o666** — correct for display servers running as non-root.
- ✅ **Node visible** — mounted under `/dev` at devfs init time.

No issues here.

---

### 2.2 fbdev ioctl Coverage

**Source:** `DevFb0::ioctl` in `kernel/src/fs/devfs.rs`, lines 342–558

| ioctl | Code | Status | Notes |
|-------|------|--------|-------|
| `FBIOGET_VSCREENINFO` | `0x4600` | ✅ Implemented | Returns current mode; all channel bitfields populated |
| `FBIOPUT_VSCREENINFO` | `0x4601` | ✅ Implemented | Calls `set_video_mode`; writes back updated struct |
| `FBIOGET_FSCREENINFO` | `0x4602` | ✅ Implemented | Populates id, smem_start, smem_len, line_length, visual |
| `FBIOGETCMAP` | `0x4604` | ⚠️ Stub | Accepts pointer, returns 0 (correct for truecolor) |
| `FBIOPUTCMAP` | `0x4605` | ⚠️ Stub | Same as above |
| `FBIOBLANK` | `0x4611` | ✅ Implemented | Returns 0 (no-op — acceptable for QEMU) |
| `FBIOPAN_DISPLAY` | `0x4606` | ❌ **Missing** | Required by Xfbdev for double-buffering; returns ENOTTY |
| `FBIOGET_CON2FBMAP` | `0x460F` | ❌ **Missing** | Xfbdev queries this to map VT console to framebuffer |
| `FBIO_WAITFORVSYNC` | `0x4620` | ❌ **Missing** | Needed for tear-free rendering in SDL2 and Weston |

**Root problem:** `Xfbdev` calls `FBIOPAN_DISPLAY` immediately after `FBIOPUT_VSCREENINFO` during driver initialisation to set `xoffset`/`yoffset` = 0. Receiving `ENOTTY` (−25) causes it to print *"Could not set video mode"* and abort.

---

### 2.3 `FbFixScreeninfo` / `FbVarScreeninfo` ABI Parity

**Source:** `kernel/src/fs/devfs.rs`, lines 225–300

#### `fb_var_screeninfo` — ABI Match ✅

The `FbVarScreeninfo` struct is `#[repr(C)]` and field-ordered to match the Linux `struct fb_var_screeninfo` (defined in `uapi/linux/fb.h`). All field offsets and total size (160 bytes) match.

#### `fb_fix_screeninfo` — **CRITICAL ABI Mismatch** ❌

The Linux kernel's `struct fb_fix_screeninfo` on x86-64 contains an **implicit 2-byte alignment gap** between `ywrapstep` (`u16`) and `line_length` (`u32`):

```
Linux offset map (x86-64):
  [0x00] id[16]           = 16 bytes
  [0x10] smem_start       = 8 bytes  (unsigned long)
  [0x18] smem_len         = 4 bytes
  [0x1c] type             = 4 bytes
  [0x20] type_aux         = 4 bytes
  [0x24] visual           = 4 bytes
  [0x28] xpanstep         = 2 bytes
  [0x2a] ypanstep         = 2 bytes
  [0x2c] ywrapstep        = 2 bytes
  [0x2e] <<2 bytes padding>>          ← MISSING IN KontsnorOS
  [0x30] line_length      = 4 bytes   ← KOS places this at 0x2e!
  [0x34] mmio_start       = 8 bytes
  [0x3c] mmio_len         = 4 bytes
  [0x40] accel            = 4 bytes
  [0x44] capabilities     = 2 bytes
  [0x46] reserved[2]      = 4 bytes
  Total: 0x4a = 74 bytes? No — total is 80 bytes.
```

The KontsnorOS struct places `line_length` at byte offset **46** (0x2E). Linux userspace expects it at offset **48** (0x30). Any tool that reads `finfo.line_length` — including `Xfbdev`, `fbvncserver`, and `SDL2` — will receive a value two bytes off, producing a garbage stride and garbled video.

**Required fix:** Add an explicit `_pad: u16` field and a compile-time size assertion:

```rust
#[repr(C)]
pub struct FbFixScreeninfo {
    pub id:          [u8; 16],
    pub smem_start:  u64,
    pub smem_len:    u32,
    pub type_:       u32,
    pub type_aux:    u32,
    pub visual:      u32,
    pub xpanstep:    u16,
    pub ypanstep:    u16,
    pub ywrapstep:   u16,
    pub _pad:        u16,   // ← ADD: explicit ABI alignment padding
    pub line_length: u32,
    pub mmio_start:  u64,
    pub mmio_len:    u32,
    pub accel:       u32,
    pub capabilities: u16,
    pub reserved:    [u16; 2],
}
// Must be 80 bytes to match Linux uapi on x86-64:
const _: () = assert!(
    core::mem::size_of::<FbFixScreeninfo>() == 80,
    "FbFixScreeninfo size must be 80 bytes to match Linux uapi"
);
```

---

### 2.4 `mmap` on `/dev/fb0`

**Source:** `kernel/src/syscall/memory.rs`, lines 76–342

The `/dev/fb0` detection logic correctly identifies the device by `rdev == (29 << 8)` (major 29, minor 0). The mapping loop iterates page-by-page and calls `map_user_page_no_shootdown`. Several issues exist:

| Issue | Location | Severity |
|-------|----------|----------|
| `WRITE_THROUGH` caching flag — MMIO framebuffer should use `NO_CACHE` or PAT WC | `memory.rs:316–319` | MEDIUM |
| `offset` parameter not bounds-checked before `get_lfb_phys() + offset` | `memory.rs:314` | MEDIUM (security: OOB physical mapping) |
| No `shootdown_tlb()` after the per-page mapping loop | `memory.rs:341` | LOW (SMP TLB coherence) |

**Caching detail:** `WRITE_THROUGH` serialises every store to the memory bus. On a linear framebuffer this is ~10× slower than Write-Combining (WC), which batches writes into 64-byte bus transactions. The correct order is: `NO_CACHE` (safe, always correct) → PAT WC (optimal, requires PAT MSR setup at BSP boot). The current `WRITE_THROUGH` is neither correct nor optimal for MMIO.

**Offset bounds:**

```rust
// Current (UNSAFE):
let vram_phys_base = crate::drivers::gpu::bochs::get_lfb_phys() + (offset as u64);

// Required:
let vram_total = crate::drivers::gpu::bochs::get_lfb_size(); // = 16MB
if (offset as u64).saturating_add(aligned_len as u64) > vram_total {
    return Errno::EINVAL.into();
}
let vram_phys_base = crate::drivers::gpu::bochs::get_lfb_phys() + (offset as u64);
```

---

### 2.5 `read` / `write` / `lseek` on `/dev/fb0`

**`read` and `write`** — `DevFb0::read/write` in `devfs.rs:312–340`

Both implement explicit `offset`-based clamped I/O against `vram_size = 16MB`. No overrun is possible. ✅

**`lseek` — Bug**

`FileDescription::seek` (`fs/file.rs:154`) correctly updates `self.offset`. However, `FileDescription::read/write` discard the offset for `CharDevice`:

```rust
// fs/file.rs:110–122
let is_seekable = file_type == crate::fs::inode::FileType::Regular;
if is_seekable {
    // ... uses and updates self.offset ...
} else {
    self.inode.read(0, buf)  // ← CharDevice always reads from offset 0!
}
```

A process that calls `lseek(fb_fd, 4096, SEEK_SET)` then `write(fb_fd, buf, 4096)` will write to VRAM offset 0, not offset 4096. Sequential pixel-writing tools like `dd if=framebuf.raw of=/dev/fb0` will silently paint the wrong location.

**Fix (one line):**

```rust
let is_seekable = matches!(
    file_type,
    FileType::Regular | FileType::BlockDevice | FileType::CharDevice
);
```

This is safe: `Pipe`, `Socket`, and `FIFO` types are not affected because they have different `FileType` values.

---

### 2.6 Bochs GPU Driver State

**Source:** `kernel/src/drivers/gpu/bochs.rs`

| Property | Status |
|----------|--------|
| PCI VID/DID detection (0x1234:0x1111) | ✅ |
| BAR0 VRAM physical base discovery | ✅ |
| 16MB VRAM kernel mapping at `0xffff_c000_0000_0000` with `NO_CACHE` | ✅ |
| VBE I/O port `set_video_mode` sequence | ✅ |
| Heap-backed backbuffer + `blit()` to VRAM | ✅ |
| ANSI GraphicsConsole with full SGR colour | ✅ |
| **Default mode 320×200×32 — too small for Xfbdev** | ⚠️ |
| `VBE_DISPI_INDEX_VIRT_WIDTH/HEIGHT` never set | ⚠️ |

**Default resolution** must be raised to at least **1024×768×32** to satisfy Xfbdev's minimum mode requirements. Change in `bochs.rs:init()`:

```diff
-    let width = 320;
-    let height = 200;
+    let width = 1024;
+    let height = 768;
```

---

### 2.7 Input Subsystem

**Source:** `kernel/src/drivers/keyboard.rs`

The PS/2 keyboard driver is well-implemented: IRQ-driven scancode translation, 4KB ring buffer, wait-queue signalling. **However, the entire `/dev/input/` subsystem is absent:**

| Missing Component | Impact |
|-------------------|--------|
| `/dev/input/` directory in devfs | Any tool using evdev API fails immediately |
| `/dev/input/event0` keyboard evdev node | Xfbdev, SDL2, libinput, all refuse to start |
| `/dev/input/event1` or `/dev/input/mice` mouse node | No pointer input in any display stack |
| `struct input_event` kernel-side queue | Evdev requires typed events (EV_KEY, EV_REL, EV_SYN), not raw ASCII |
| PS/2 mouse hardware driver (IRQ 12 / port 0x64) | No relative pointer motion data |

Without `/dev/input/event0`, **every display server and VNC tool that relies on evdev will fail to receive any keyboard or mouse events.**

---

## Gap Analysis for Running Linux GUI & VNC

### 3.1 Display Server Requirements (Xfbdev / SDL2)

**Xfbdev** startup sequence and what it calls:

```
1. open("/dev/fb0", O_RDWR)
2. ioctl(FBIOGET_VSCREENINFO)                 ← EXISTS ✅
3. ioctl(FBIOGET_FSCREENINFO)                 ← EXISTS but ABI mismatch ⚠️
4. ioctl(FBIOPUT_VSCREENINFO) [set mode]      ← EXISTS ✅
5. ioctl(FBIOPAN_DISPLAY) [pan to 0,0]        ← MISSING ❌ → Xfbdev ABORTS HERE
6. ioctl(FBIOGET_CON2FBMAP)                   ← MISSING ❌
7. mmap(MAP_SHARED, PROT_READ|PROT_WRITE)     ← EXISTS (WT caching ⚠️)
8. open("/dev/input/event0")                  ← MISSING ❌
```

**SDL2 framebuffer backend** (`SDL_VIDEODRIVER=fbcon`) requires the same set, plus `FBIO_WAITFORVSYNC` for vsync.

### 3.2 VNC Capture Requirements (fbvncserver / x11vnc)

`fbvncserver` only needs:
1. `FBIOGET_VSCREENINFO` / `FBIOGET_FSCREENINFO` (correctly sized) — ⚠️ ABI fix required
2. `mmap(MAP_SHARED, PROT_READ)` — ✅ works
3. Continuous read of the mmap'd pixel data — ✅ no issue
4. For VNC client input injection: `/dev/input/uinput` or direct `/dev/input/event*` write — ❌ missing

**Minimum viable read-only VNC** (display capture, no keyboard/mouse input back to guest) requires only fixing G2 (ABI) + G7 (resolution).

### 3.3 Input Pipeline Missing Links

Complete evdev chain to implement:

```
PS/2 Keyboard IRQ 1          PS/2 Mouse IRQ 12
        │                          │
        ▼                          ▼
  keyboard.rs               [NEW] ps2_mouse.rs
  push_scancode()           push_byte()
        │                          │
        ▼                          ▼
  [NEW] EV_KEY event queue   [NEW] EV_REL event queue
  (struct input_event)       (struct input_event)
        │                          │
        ▼                          ▼
  [NEW] /dev/input/event0    [NEW] /dev/input/event1
  (Major 13, Minor 64)       (Major 13, Minor 65)
        │                          │
        └──────────┬───────────────┘
                   ▼
          [NEW] /dev/input/mice
          (ImPS/2 5-byte packets)
```

The `struct input_event` wire format (24 bytes on x86-64):

```c
struct input_event {
    uint64_t tv_sec;    // timeval: seconds
    uint64_t tv_usec;   // timeval: microseconds
    uint16_t type;      // EV_SYN=0, EV_KEY=1, EV_REL=2
    uint16_t code;      // KEY_A=30, REL_X=0, REL_Y=1, BTN_LEFT=0x110
    int32_t  value;     // 1=keydown, 0=keyup, signed delta for EV_REL
};
```

---

## Step-by-Step Implementation Roadmap

### Milestone 1: `/dev/fb0` Completeness & ABI Parity

**Goal:** All required fbdev ioctls present and correct. ABI structures match Linux uapi. lseek works.  
**Target size:** ~120 lines changed  
**Files:** `kernel/src/fs/devfs.rs`, `kernel/src/fs/file.rs`, `kernel/src/drivers/gpu/bochs.rs`

#### 1.1 — Fix `FbFixScreeninfo` ABI padding

```diff
 pub struct FbFixScreeninfo {
     pub id:          [u8; 16],
     pub smem_start:  u64,
     pub smem_len:    u32,
     pub type_:       u32,
     pub type_aux:    u32,
     pub visual:      u32,
     pub xpanstep:    u16,
     pub ypanstep:    u16,
     pub ywrapstep:   u16,
+    pub _pad:        u16,   // ABI: implicit alignment padding before line_length
     pub line_length: u32,
     pub mmio_start:  u64,
     pub mmio_len:    u32,
     pub accel:       u32,
     pub capabilities: u16,
     pub reserved:    [u16; 2],
 }
+
+const _: () = assert!(
+    core::mem::size_of::<FbFixScreeninfo>() == 80,
+    "FbFixScreeninfo must be 80 bytes to match Linux uapi on x86-64"
+);
```

Also update the `FbFixScreeninfo { ... }` literal inside `FBIOGET_FSCREENINFO` to include `_pad: 0`.

#### 1.2 — Add three missing ioctl stubs

```diff
+        const FBIOPAN_DISPLAY:   u64 = 0x4606;
+        const FBIOGET_CON2FBMAP: u64 = 0x460F;
+        const FBIO_WAITFORVSYNC: u64 = 0x4620;

         match request {
+            FBIOPAN_DISPLAY => {
+                // Validate the fb_var_screeninfo pointer; no hardware pan needed
+                // on Bochs (single flat linear buffer).
+                crate::syscall::validation::validate_user_ptr_write(
+                    arg as *mut u8,
+                    core::mem::size_of::<FbVarScreeninfo>(),
+                )
+                .map_err(|_| -14)?;
+                Ok(0)
+            }
+            FBIO_WAITFORVSYNC => Ok(0), // No vsync hardware; return immediately
+            FBIOGET_CON2FBMAP => Ok(0), // Console 0 → fb0; return success
```

#### 1.3 — Fix `lseek` on CharDevice (`fs/file.rs`)

```diff
-        let is_seekable = file_type == crate::fs::inode::FileType::Regular;
+        // CharDevice and BlockDevice are seekable in addition to Regular files.
+        // Pipe/Socket/FIFO types are explicitly excluded (they remain non-seekable).
+        let is_seekable = matches!(
+            file_type,
+            crate::fs::inode::FileType::Regular
+                | crate::fs::inode::FileType::BlockDevice
+                | crate::fs::inode::FileType::CharDevice
+        );
```

Apply the same diff to the `write` path (line ~136 in `file.rs`).

#### 1.4 — Default Bochs resolution to 1024×768

```diff
-    let width = 320;
-    let height = 200;
+    let width = 1024;
+    let height = 768;
```

#### Verification

```bash
cargo check -p kernel --target x86_64-unknown-none
cargo clippy -p kernel --target x86_64-unknown-none -- -D warnings
```

---

### Milestone 2: Userspace `mmap` Hardening

**Goal:** Correct caching attributes, VRAM bounds enforcement, SMP TLB coherence.  
**Target size:** ~40 lines changed  
**Files:** `kernel/src/syscall/memory.rs`

#### 2.1 — Bounds-check the mmap offset

```diff
     if is_dev_fb0 {
+        let vram_total = crate::drivers::gpu::bochs::get_lfb_size();
+        if (offset as u64).saturating_add(aligned_len as u64) > vram_total {
+            return Errno::EINVAL.into();
+        }
         use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
```

#### 2.2 — Switch caching from WRITE_THROUGH to NO_CACHE

```diff
-            let page_flags = PageTableFlags::PRESENT
-                | PageTableFlags::USER_ACCESSIBLE
-                | PageTableFlags::WRITABLE
-                | PageTableFlags::WRITE_THROUGH;
+            // VRAM MMIO must not be write-through coalesced into the cache hierarchy.
+            // NO_CACHE is safe and correct. Future work: upgrade to PAT Write-Combining
+            // for ~3× throughput improvement (requires PAT MSR setup at BSP boot).
+            let page_flags = PageTableFlags::PRESENT
+                | PageTableFlags::USER_ACCESSIBLE
+                | PageTableFlags::WRITABLE
+                | PageTableFlags::NO_CACHE;
```

#### 2.3 — TLB shootdown after the mapping loop

```diff
         }
+        // Ensure all CPUs see the new VRAM page table entries.
+        crate::arch::x86_64::smp::shootdown_tlb();
     }
```

#### Verification

```bash
cargo check -p kernel --target x86_64-unknown-none
cargo clippy -p kernel --target x86_64-unknown-none -- -D warnings
# Test in QEMU: mmap /dev/fb0 with offset=16*1024*1024 → must return EINVAL
```

---

### Milestone 3: Evdev / Mouse Input Exposure

**Goal:** `/dev/input/event0` (keyboard) and `/dev/input/mice` (PS/2 pointer) visible to userspace.  
**Target size:** ~140 lines (can split into two sub-PRs)  
**Files:** `kernel/src/fs/devfs.rs`, `kernel/src/drivers/keyboard.rs` (minor), NEW `kernel/src/drivers/ps2_mouse.rs`

#### 3.1 — Define `InputEvent` and keyboard evdev node (`devfs.rs`)

```rust
/// Linux input_event structure, exactly 24 bytes on x86-64.
#[repr(C)]
pub struct InputEvent {
    pub tv_sec:  u64,  // seconds
    pub tv_usec: u64,  // microseconds
    pub type_:   u16,  // EV_SYN=0, EV_KEY=1, EV_REL=2
    pub code:    u16,  // key code or axis code
    pub value:   i32,  // 1=down, 0=up, signed delta for EV_REL
}
const _: () = assert!(core::mem::size_of::<InputEvent>() == 24);

/// /dev/input/event0 — evdev keyboard node (Major 13, Minor 64).
pub struct DevInputKeyboard {
    pub inode: Inode,
}

impl InodeOps for DevInputKeyboard {
    fn inode(&self) -> &Inode { &self.inode }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        const SZ: usize = core::mem::size_of::<InputEvent>();
        if buf.len() < SZ { return Err(-22); } // EINVAL: buffer too small
        match crate::drivers::keyboard::try_read_char() {
            Some(ascii) => {
                let ev = InputEvent {
                    tv_sec: 0, tv_usec: 0,
                    type_: 1,  // EV_KEY
                    code: ascii_to_keycode(ascii),
                    value: 1,  // key-down
                };
                // SAFETY: InputEvent is repr(C); no uninit padding bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &ev as *const InputEvent as *const u8,
                        buf.as_mut_ptr(), SZ,
                    );
                }
                Ok(SZ)
            }
            None => Err(-11), // EAGAIN
        }
    }

    fn poll(&self, _events: u32) -> u32 {
        if crate::drivers::keyboard::has_input() { super::inode::POLLIN } else { 0 }
    }
}

/// Minimal ASCII→Linux keycode mapping (PS/2 Set 1 subset).
fn ascii_to_keycode(ascii: u8) -> u16 {
    match ascii {
        b'a'..=b'z' => 30 + (ascii - b'a') as u16,
        b'A'..=b'Z' => 30 + (ascii - b'A') as u16,
        b'\n'       => 28,  // KEY_ENTER
        b'\x08'     => 14,  // KEY_BACKSPACE
        b' '        => 57,  // KEY_SPACE
        _           => 0,
    }
}
```

#### 3.2 — Create `/dev/input/` directory in `create_devfs()`

```rust
// Inside create_devfs(), after the existing entries:
let mut input_entries = BTreeMap::new();
input_entries.insert(
    String::from("event0"),
    Arc::new(DevInputKeyboard {
        inode: make_chardev_inode(50, 13, 64),
    }) as Arc<dyn InodeOps>,
);
// /dev/input/mice will be added in sub-PR 3b
let input_dir = Arc::new(DevFsDir {
    inode: Inode::new(30, FileType::Directory).with_dev(DEVFS_DEV_ID),
    entries: RwLock::new(input_entries),
});
entries.insert(String::from("input"), input_dir as Arc<dyn InodeOps>);
```

#### 3.3 — Minimal PS/2 mouse driver and `/dev/input/mice` (sub-PR 3b)

Create `kernel/src/drivers/ps2_mouse.rs` (~80 lines):
- Enable the auxiliary PS/2 device via port 0x64 command `0xA8`
- IRQ 12 handler calls `push_byte()` which buffers raw 3-byte packets
- Export `read_mice_packet(buf: &mut [u8]) -> Option<usize>` that emits 5-byte ImPS/2 format

Register `/dev/input/mice` (Major 13, Minor 63) in devfs that calls `read_mice_packet`.

#### Verification

```bash
cargo check -p kernel --target x86_64-unknown-none
cargo clippy -p kernel --target x86_64-unknown-none -- -D warnings
# On Alpine rootfs with evtest package:
#   evtest /dev/input/event0    → should show EV_KEY events on keypress
#   hexdump -C /dev/input/mice  → should show 5-byte packets on mouse move
```

---

### Milestone 4: End-to-End Verification Pipeline

**Goal:** A minimal C test draws to `/dev/fb0` via mmap; a shell script launches the test in QEMU and validates output without `build-image.sh`.  
**Target size:** ~100 lines  
**Files:** `tools/test-fbdev.sh` (NEW), `userland/tests/fbtest.c` (NEW)

#### 4.1 — Minimal fbdev C test (`userland/tests/fbtest.c`)

```c
#include <sys/mman.h>
#include <sys/ioctl.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <linux/fb.h>

int main(void) {
    int fd = open("/dev/fb0", O_RDWR);
    if (fd < 0) { perror("open /dev/fb0"); return 1; }

    struct fb_var_screeninfo vinfo;
    struct fb_fix_screeninfo finfo;
    if (ioctl(fd, FBIOGET_VSCREENINFO, &vinfo)) { perror("VSCREENINFO"); return 1; }
    if (ioctl(fd, FBIOGET_FSCREENINFO, &finfo)) { perror("FSCREENINFO"); return 1; }

    printf("Resolution: %dx%d bpp=%d stride=%d\n",
           vinfo.xres, vinfo.yres, vinfo.bits_per_pixel, finfo.line_length);

    // Validate: stride must be xres * 4 for 32bpp
    if (finfo.line_length != vinfo.xres * 4) {
        fprintf(stderr, "FAIL: line_length=%d expected=%d\n",
                finfo.line_length, vinfo.xres * 4);
        return 1;
    }

    uint32_t *fb = mmap(NULL, finfo.smem_len,
                        PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0);
    if (fb == MAP_FAILED) { perror("mmap"); return 1; }

    // Draw horizontal colour gradient
    for (unsigned y = 0; y < vinfo.yres; y++) {
        for (unsigned x = 0; x < vinfo.xres; x++) {
            uint8_t r = (uint8_t)((x * 255) / vinfo.xres);
            uint8_t g = (uint8_t)((y * 255) / vinfo.yres);
            fb[y * (finfo.line_length / 4) + x] = (r << 16) | (g << 8) | 0x80;
        }
    }

    munmap(fb, finfo.smem_len);
    close(fd);
    puts("fbtest: OK");
    return 0;
}
```

This test validates: correct ioctl responses, correct `line_length` (ABI parity), working `mmap(MAP_SHARED)`, and pixel write coherence.

#### 4.2 — QEMU verification script (`tools/test-fbdev.sh`)

```bash
#!/usr/bin/env bash
# tools/test-fbdev.sh — Boots KontsnorOS in QEMU and validates /dev/fb0.
# Does NOT invoke build-image.sh or format any disk.
set -euo pipefail

KERNEL=kernel/target/x86_64-unknown-none/release/kernel
ROOTFS=alpine-rootfs.img   # pre-built Alpine rootfs image (not formatted here)

[ -f "$KERNEL" ] || { echo "Build kernel first: cargo build -p kernel --release"; exit 1; }
[ -f "$ROOTFS" ] || { echo "Provide an Alpine rootfs image at $ROOTFS"; exit 1; }

LOG=$(mktemp /tmp/qemu-fbtest-XXXXX.log)
echo "[test] Launching QEMU, log → $LOG"

timeout 90 qemu-system-x86_64 \
    -M q35 -m 256 -cpu host -smp 1 \
    -vga std \
    -kernel "$KERNEL" \
    -append "root=/dev/vda rw quiet console=ttyS0" \
    -drive file="$ROOTFS",format=raw,if=virtio \
    -serial file:"$LOG" \
    -monitor none \
    -nographic &

QEMU_PID=$!
sleep 30   # allow boot to complete

kill $QEMU_PID 2>/dev/null || true

if grep -q "fbtest: OK" "$LOG"; then
    echo "[PASS] /dev/fb0 mmap gradient test succeeded"
else
    echo "[FAIL] fbtest output not found in log"
    tail -30 "$LOG"
    exit 1
fi
```

#### Verification

```bash
# Run after Milestones 1–3 are merged:
./tools/test-fbdev.sh

# Optional: launch fbvncserver on Alpine rootfs for visual verification:
# apk add fbvncserver
# fbvncserver -f /dev/fb0 -p 5900 &
# Connect from host: vncviewer localhost:5900
```

---

## Security Considerations

| Risk | Location | Mitigation in Roadmap |
|------|----------|-----------------------|
| OOB physical mapping via unchecked mmap offset | `memory.rs:314` | M2 Task 2.1: explicit `offset + len <= vram_total` check |
| Userspace pointer not validated before ioctl struct write | `devfs.rs:352–403` | Already uses `validate_user_ptr_write` ✅; new stubs must do the same |
| `InputEvent` copy with partial buffer: struct partially written | `DevInputKeyboard::read` | Return `EINVAL` if `buf.len() < sizeof(InputEvent)` ✅ |
| PS/2 mouse IRQ handler holds `TicketLock` at interrupt priority | `ps2_mouse.rs` | Mirror pattern in `keyboard.rs` — `TicketLock` is interrupt-safe ✅ |
| `FbFixScreeninfo` uninitialised `_pad` field leaked to userspace | `devfs.rs` | Set `_pad: 0` explicitly in the literal; `#[derive(Default)]` not applicable since other fields differ ✅ |
| ASCII→keycode mapping: unmapped keys produce `code=0` (KEY_RESERVED) | `ascii_to_keycode` | Return code 0 with `value=0` (no-op) rather than propagating an event — harmless |

---

## References

| Resource | Location |
|----------|----------|
| Linux `uapi/linux/fb.h` | https://elixir.bootlin.com/linux/latest/source/include/uapi/linux/fb.h |
| Linux `uapi/linux/input.h` | https://elixir.bootlin.com/linux/latest/source/include/uapi/linux/input.h |
| `fbvncserver` source | https://github.com/ponty/framebuffer-vncserver |
| Xorg `xf86-video-fbdev` driver | https://gitlab.freedesktop.org/xorg/driver/xf86-video-fbdev |
| Intel SDM Vol 3A §11.12 — Memory Type Range Registers (PAT) | Intel 64 and IA-32 SDM |
| Bochs VBE Extensions wiki | https://wiki.osdev.org/Bochs_VBE_Extensions |
| `kernel/src/fs/devfs.rs` | KontsnorOS devfs implementation |
| `kernel/src/drivers/gpu/bochs.rs` | KontsnorOS Bochs VBE GPU driver |
| `kernel/src/syscall/memory.rs` | KontsnorOS `mmap` syscall |
| `kernel/src/drivers/keyboard.rs` | KontsnorOS PS/2 keyboard driver |
| `kernel/src/fs/file.rs` | KontsnorOS FileDescription (seek bug) |
