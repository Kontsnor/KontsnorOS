# Phase D Next Steps: Evdev Completion, uinput & GUI Pipeline

**Author:** Principal Systems Engineer (User-Space & Tooling Specialist)  
**Date:** 2026-09-17  
**Prerequisite:** All four milestones in `docs/fbdev_userspace_pipeline_audit.md` have been implemented and verified (see agent walkthrough at `brain/c433d2f3-c011-473a-8bae-d96210a17d4b/walkthrough.md`).  
**Target agent persona:** `userspace` — read `SKILL.md` at `.antigravity/skills/libc-userspace/SKILL.md` before starting.

---

## Context

The `/dev/fb0` pipeline is complete and passing all tests. The `/dev/input/event0` (keyboard) and `/dev/input/mice` (PS/2 mouse) device nodes now exist in devfs. However, **evdev nodes without capability ioctls are invisible to every real-world evdev consumer.** libinput, SDL2, Xfbdev, and `evtest` all open the device, call `EVIOCGVERSION` / `EVIOCGBIT`, and silently skip any device that does not respond. The kernel-side input pipeline is structurally complete but not yet functional end-to-end.

This document is a self-contained implementation spec for the next agent. Each work item includes the exact files to touch, the exact change to make, and the acceptance criterion.

---

## Work Items — Ordered by Priority

---

### P0-A · Evdev Capability Ioctls on `/dev/input/event0` and `/dev/input/mice`

**Why blocking:** Without these, `evtest /dev/input/event0` returns immediately with no output, libinput marks the device as "not an evdev device", and SDL2/Xfbdev skip input entirely.

**File:** `kernel/src/fs/devfs.rs`

**What to implement:** Add an `ioctl` method to both `DevInputKeyboard` and `DevInputMice` implementing the following ioctls. All codes use the Linux `_IOC` encoding — the high bits encode direction and size, but for our purposes the full 32-bit request value is matched directly.

#### Required ioctl table

| IOCTL name | Request value | Direction | Arg type | Behaviour |
|------------|--------------|-----------|----------|-----------|
| `EVIOCGVERSION` | `0x45_01` | read (kernel→user) | `*mut i32` | Write `0x0001_0001i32` (evdev protocol v1.1) |
| `EVIOCGID` | `0x45_02` | read | `*mut InputId` (8 bytes) | Write `InputId { bustype: 0x11, vendor: 0, product: 0, version: 1 }` (bustype 0x11 = BUS_I8042) |
| `EVIOCGNAME(n)` | `0x8000_4506 \| (n << 16)` | read | `*mut u8`, max `n` bytes | Write null-terminated ASCII name: keyboard = `"KontsnorOS PS/2 Keyboard"`, mice = `"KontsnorOS PS/2 Mouse"` |
| `EVIOCGBIT(0, n)` | `0x8000_4520 \| (n << 16)` | read | `*mut u8` bitmask | Event type bitmask: keyboard sets bit `EV_KEY` (1) and `EV_SYN` (0); mice sets bit `EV_REL` (2), `EV_KEY` (1), `EV_SYN` (0) |
| `EVIOCGBIT(EV_KEY, n)` | `0x8000_4521 \| (n << 16)` | read | `*mut u8` bitmask | Supported key codes. Set bits for: `KEY_A`–`KEY_Z` (30–55), `KEY_0`–`KEY_9` (11, 2–10), `KEY_ENTER` (28), `KEY_ESC` (1), `KEY_BACKSPACE` (14), `KEY_SPACE` (57), `BTN_LEFT` (0x110), `BTN_RIGHT` (0x111) |
| `EVIOCGBIT(EV_REL, n)` | `0x8000_4522 \| (n << 16)` | read | `*mut u8` bitmask | Relative axes: set bits for `REL_X` (0) and `REL_Y` (1). Keyboard returns zeroed buffer. |
| `EVIOCGPHYS(n)` | `0x8000_4508 \| (n << 16)` | read | `*mut u8` | Write `"isa0060/serio0"` (keyboard) or `"isa0060/serio1"` (mouse) — standard PS/2 physical path |
| `EVIOCGUNIQ(n)` | `0x8000_4509 \| (n << 16)` | read | `*mut u8` | Write empty string `""` (no unique ID) |

> **Matching strategy:** The top byte of a EVIOC request encodes the buffer length, so match with a mask. Use `(request & 0xFFFF_00FF)` to strip the length field, then compare against the base code. The length is `(request >> 16) & 0x3FFF` bytes.

#### `InputId` struct to define alongside the ioctl handler

```rust
/// Linux input_id structure (8 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InputId {
    pub bustype: u16,
    pub vendor:  u16,
    pub product: u16,
    pub version: u16,
}
const _: () = assert!(core::mem::size_of::<InputId>() == 8);
```

#### Bit-setting helper

```rust
/// Set bit `bit` in a byte slice bitmask (Linux evdev bit-field format).
fn set_bit(mask: &mut [u8], bit: usize) {
    if bit / 8 < mask.len() {
        mask[bit / 8] |= 1 << (bit % 8);
    }
}
```

#### Pointer validation pattern (same as fb0 ioctls)

Every ioctl that writes to userspace must call `validate_user_ptr_write` before the write:

```rust
crate::syscall::validation::validate_user_ptr_write(arg as *mut u8, size)
    .map_err(|_| -14i32)?; // -EFAULT
```

#### Acceptance criterion

```bash
# On Alpine rootfs with evtest installed:
evtest /dev/input/event0
# Must print: "Input driver version is 1.0.1"
# Must print: "Input device name: KontsnorOS PS/2 Keyboard"
# Must print: "Supported events: EV_SYN EV_KEY"
# Must then block waiting for key events — NOT exit immediately.
```

---

### P0-B · Key-Up and EV_SYN Events from `/dev/input/event0`

**Why blocking:** Every evdev consumer expects events in pairs: a key-down `EV_KEY` event followed by a `EV_SYN/SYN_REPORT` frame terminator, then later a key-up `EV_KEY` and another `EV_SYN`. Without key-up events, held modifier keys (Shift, Ctrl, Alt) will permanently latch in any application.

**File:** `kernel/src/fs/devfs.rs` — `DevInputKeyboard::read`

**Current behaviour:** Emits one 24-byte `InputEvent` with `value: 1` (key-down) per `try_read_char()` call and returns.

**Required behaviour:** When a keypress is available, emit a **burst of three `InputEvent` structs** (72 bytes total if the buffer is large enough) or emit them one per `read()` call via an internal per-fd state machine:

1. `{ type: EV_KEY, code: keycode, value: 1 }` — key down
2. `{ type: EV_SYN, code: SYN_REPORT, value: 0 }` — frame sync
3. `{ type: EV_KEY, code: keycode, value: 0 }` — key up
4. `{ type: EV_SYN, code: SYN_REPORT, value: 0 }` — frame sync

**Simplest correct implementation:** If `buf.len() >= 4 * sizeof(InputEvent)` (96 bytes), write all four events at once and return `4 * 24 = 96`. Otherwise write one event per call, using a small per-node `AtomicU8` counter to track the sub-event state (0=down, 1=syn, 2=up, 3=syn, wrap to 0).

**Constants needed:**

```rust
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const SYN_REPORT: u16 = 0;
```

**Acceptance criterion:**

```bash
evtest /dev/input/event0
# Press 'a' — must show:
#   Event: time X.X, type 1 (EV_KEY), code 30 (KEY_A), value 1
#   Event: time X.X, type 0 (EV_SYN), code 0 (SYN_REPORT), value 0
#   Event: time X.X, type 1 (EV_KEY), code 30 (KEY_A), value 0
#   Event: time X.X, type 0 (EV_SYN), code 0 (SYN_REPORT), value 0
```

---

### P0-C · `/dev/input/uinput` — VNC Back-Channel Input Injection

**Why blocking for interactive VNC:** `fbvncserver` and `x11vnc` receive keyboard/mouse events from VNC clients and inject them into the guest by writing to `/dev/input/uinput`. Without this, VNC is display-only.

**Files to create/modify:**
- NEW: `kernel/src/fs/devfs.rs` — add `DevUinput` node
- REGISTER in `create_devfs()`: `/dev/input/uinput` (Major 10, Minor 223)

**What `uinput` must support:**

The userspace tool opens the fd, writes a `uinput_setup` struct, issues `UI_DEV_CREATE` ioctl, then writes `InputEvent` structs directly to inject events. The kernel processes these as if they came from a real input device and delivers them to other `/dev/input/eventN` readers.

For a **minimal viable uinput** (sufficient for fbvncserver):

| Step | Operation |
|------|-----------|
| 1 | `open("/dev/input/uinput", O_WRONLY)` |
| 2 | `ioctl(fd, UI_SET_EVBIT, EV_KEY)` — register key event type |
| 3 | `ioctl(fd, UI_SET_EVBIT, EV_REL)` — register relative motion |
| 4 | `ioctl(fd, UI_SET_KEYBIT, KEY_*)` — register individual keys |
| 5 | `ioctl(fd, UI_DEV_CREATE)` — activate the virtual device |
| 6 | `write(fd, &input_event, 24)` — inject events |
| 7 | `ioctl(fd, UI_DEV_DESTROY)` — teardown |

**Minimal implementation strategy:** `DevUinput::write()` receives raw `InputEvent` bytes. Parse each 24-byte struct and route:
- `type == EV_KEY` → call `crate::drivers::keyboard::push_char(ascii_from_keycode(code))` to re-inject into the keyboard ring buffer (which `/dev/input/event0` reads from)
- `type == EV_REL` → call `crate::drivers::ps2_mouse::push_synthesised_packet(dx, dy)` — add a new function that directly pushes a `MousePacket` without going through the IRQ parser

**Key ioctls to stub:**

| IOCTL | Code | Behaviour |
|-------|------|-----------|
| `UI_SET_EVBIT` | `0x4_0045_64` | Accept and ignore (log event type) |
| `UI_SET_KEYBIT` | `0x4_0045_65` | Accept and ignore |
| `UI_SET_RELBIT` | `0x4_0045_66` | Accept and ignore |
| `UI_DEV_CREATE` | `0x_0000_5501` | Return 0 (device already "active") |
| `UI_DEV_DESTROY` | `0x_0000_5502` | Return 0 |

**Acceptance criterion:**

```bash
# On Alpine rootfs:
apk add fbvncserver
fbvncserver -f /dev/fb0 &
# Connect from host with vncviewer, move mouse, press keys
# → events must appear at the guest shell / application
```

---

### P1-A · PAT Write-Combining for `/dev/fb0` mmap

**Why:** Currently using `NO_CACHE` (correct, safe). Enabling PAT Write-Combining yields ~3× framebuffer write throughput — visible as smoother redraws when running SDL2 apps or a compositor.

**Files:**
- `kernel/src/arch/x86_64/boot.rs` (or wherever BSP init runs) — configure `IA32_PAT` MSR
- `kernel/src/syscall/memory.rs` — change the fb0 `page_flags`

**PAT MSR setup (one-time at BSP boot):**

```rust
// IA32_PAT MSR = 0x277
// Default PAT layout: UC UC- WC WB UC UC- WC WB
// We want entry 1 (PAT1) = WC (Write-Combining = 0x01)
// Default value is 0x0007_0406_0007_0406; PAT1 is bits [15:8]
// Set PAT1 = WC: bits [15:8] = 0x01
// Result: 0x0007_0406_0007_0106
unsafe {
    x86_64::registers::model_specific::Msr::new(0x277)
        .write(0x0007_0406_0007_0106);
}
```

**Page flag change in `memory.rs`:**

```diff
-            let page_flags = PageTableFlags::PRESENT
-                | PageTableFlags::USER_ACCESSIBLE
-                | PageTableFlags::WRITABLE
-                | PageTableFlags::NO_CACHE;
+            // PAT1 = WC: PWT=1, PCD=0, PAT=0 selects PAT entry 1 = Write-Combining.
+            let page_flags = PageTableFlags::PRESENT
+                | PageTableFlags::USER_ACCESSIBLE
+                | PageTableFlags::WRITABLE
+                | PageTableFlags::WRITE_THROUGH; // PWT=1 → PAT entry 1 = WC
```

> **Note:** Only make this change *after* the PAT MSR is confirmed written. If the PAT is not configured, `WRITE_THROUGH` will mean literal write-through caching (slower than WC). Verify with `rdmsr 0x277` in QEMU monitor.

**Acceptance criterion:**

```bash
# In QEMU monitor:
(qemu) info registers | grep PAT
# OR: time dd if=/dev/zero of=/dev/fb0 bs=4M count=1
# → Write speed should improve from ~100MB/s (NO_CACHE) to ~300MB/s (WC)
```

---

### P1-B · Shell History & Tab Completion (`tools/sh.c`)

**Why:** The SKILL.md explicitly targets `sh.c` upgrades. The current shell has no persistent command history and no tab-completion, making it difficult to use for development directly inside the VM.

**File:** `tools/sh.c`

**Changes:**

1. **Command history ring buffer** — store up to 50 previous commands in a static `char history[50][256]` array. On `\n`, push the current line. On `\x1b[A` (Up arrow), display previous command. On `\x1b[B` (Down arrow), display next.

2. **Tab completion** — on `\t`, call `sys_getdents64(".", ...)` to enumerate the current directory and print any entries that prefix-match the current partial word on the command line. If exactly one match, complete it in place.

3. **Arrow key parsing** — the current shell does not handle the three-byte VT100 escape sequence `ESC [ A/B/C/D`. Add a simple three-state parser: `NORMAL` → on `\x1b` → `ESC` → on `[` → `BRACKET` → on `A/B/C/D` → dispatch.

**Acceptance criterion:**

```bash
# In KontsnorOS shell:
ls /dev        # enter command
# Press Up arrow → "ls /dev" should reappear on the prompt
# Type "ls /de" then press Tab → "ls /dev/" should auto-complete
```

---

## Verification Commands

After all P0 items are implemented, run the standard quality gates:

```bash
# Kernel build & lint
cargo check -p kernel --target x86_64-unknown-none
cargo clippy -p kernel --target x86_64-unknown-none -- -D warnings
cargo fmt --check

# Full test suite
./tools/run-tests.sh

# Evdev smoke test (on Alpine rootfs)
evtest /dev/input/event0   # must block, show capability listing, emit events on keypress
hexdump -C /dev/input/mice # must show 5-byte packets when QEMU mouse moves

# VNC integration test (on Alpine rootfs)
fbvncserver -f /dev/fb0 &
# Connect from host: vncviewer localhost:5900
# Keyboard and mouse events from VNC client must reach the guest
```

---

## Files Modified / Created (Summary)

| File | Change |
|------|--------|
| `kernel/src/fs/devfs.rs` | Add `InputId` struct; add `ioctl` to `DevInputKeyboard` and `DevInputMice`; add key-up + EV_SYN emission; add `DevUinput` node; register `/dev/input/uinput` |
| `kernel/src/drivers/ps2_mouse.rs` | Add `push_synthesised_packet(dx: i8, dy: i8)` for uinput back-channel |
| `kernel/src/arch/x86_64/boot.rs` | (P1-A) Write `IA32_PAT` MSR to enable PAT entry 1 = WC |
| `kernel/src/syscall/memory.rs` | (P1-A) Switch fb0 mmap flag to PAT WC after MSR is confirmed |
| `tools/sh.c` | (P1-B) History ring buffer, tab completion, arrow key VT100 parser |

---

## Security Checklist for Implementing Agent

Per `AGENTS.md` SOP, before finishing:

- [ ] All new ioctl handlers that write to userspace call `validate_user_ptr_write` before the write
- [ ] `DevUinput::write` validates that each 24-byte `InputEvent` is fully within the supplied buffer before parsing
- [ ] `push_synthesised_packet` has the same interrupt-safety guarantees as `push_byte` (uses `TicketLock`)
- [ ] No new `unsafe` block is added without a `// SAFETY:` comment
- [ ] Zero new Clippy warnings: `cargo clippy -p kernel --target x86_64-unknown-none -- -D warnings`
