# Research & Technical Roadmap: Running a Desktop Environment (Ubuntu Rootfs) in QEMU under WSL2

## Executive Summary
This document outlines the gap analysis, architecture options, recommended display stack, QEMU/WSL2 host configuration, and step-by-step roadmap required to launch a graphical desktop environment (GUI) on KontsnorOS using our Ubuntu 24.04 userspace rootfs inside QEMU under WSL2.

---

## 1. Kernel Driver & Hardware Emulation Gap Analysis

### 1.1 GPU & Framebuffer Subsystem
- **Current State:**
  - KontsnorOS includes a Bochs VBE PCI graphics driver (`kernel/src/drivers/gpu/bochs.rs`) that switches the card to `1024x768x32` linear framebuffer (LFB) mode.
  - The LFB physical address (BAR0) is mapped into the higher-half virtual memory (`0xffff_c000_0000_0000`).
  - Kernel-side backbuffer rendering and software font text console (`GraphicsConsole`) are functional.
  - **Gap:** Missing `/dev/fb0` device node in `devfs` (`kernel/src/fs/devfs.rs`). Userspace programs (like `Xfbdev`, `Xorg` fbdev driver, or `Pixman` compositors) rely on opening `/dev/fb0`, issuing `FBIOGET_FSCREENINFO` / `FBIOGET_VSCREENINFO` `ioctl`s, and `mmap()`ing the file descriptor to access video memory directly.

- **Target QEMU Display Device:**
  - Standard VGA / Bochs VBE (`-vga std` or `-vga bochs`) is the optimal initial target. It requires no complex DRM/KMS hardware acceleration pipeline in the kernel while providing full 32-bit linear framebuffers.
  - Future step: `virtio-gpu` with DRM/KMS support (`/dev/dri/card0`) once atomic modesetting and GEM memory management are implemented in KontsnorOS.

- **Userspace Compatibility:**
  - Standalone X servers using the framebuffer (`Xfbdev` / `kdrive` or Xorg with `xserver-xorg-video-fbdev`) can run directly on top of simple `/dev/fb0`.

### 1.2 Input Devices (Keyboard & Mouse)
- **Current State:**
  - PS/2 keyboard driver (`kernel/src/drivers/keyboard.rs`) translates scancodes into ASCII and pushes into stdin/tty ring buffers.
  - **Gap:** Mouse/pointer input support is absent. Linux userspace display servers expect mouse events exposed via `/dev/input/mice` or `evdev` (`/dev/input/eventX`).
  - **Requirement:** Implement PS/2 auxiliary mouse driver (translating 3-byte PS/2 mouse packets into relative X/Y offsets and button flags) or `virtio-input` / USB HID tablet (providing absolute X/Y coordinates). Register `/dev/input/mice` character device node in devfs.

---

## 2. Syscall & Subsystem Prerequisites

### 2.1 Memory Subsystem
- `mmap`: Must support mmapping `/dev/fb0` into user space, anonymous shared memory (`MAP_SHARED`), and file-backed shared memory.
- `shmget` / `shmat` / `shmdt` / `shmctl`: System V IPC shared memory or `/dev/shm` (POSIX shared memory via `tmpfs`) for X11 MIT-SHM extension.

### 2.2 IPC & Sockets
- `AF_UNIX` domain sockets (`SOCK_STREAM` and `SOCK_DGRAM`): Crucial for X11 (`/tmp/.X11-unix/X0`) and Wayland IPC socket communication between clients and the display server.

### 2.3 Signals & Terminals (VT / TTY)
- VT ioctls: `KDSETMODE` (switching console between text mode `KD_TEXT` and graphics mode `KD_GRAPHICS`), `VT_ACTIVATE`, `VT_WAITACTIVE`, `VT_GETSTATE`.
- Terminals: Pseudo-terminal pairs (`/dev/ptmx` and `/dev/pts/X`) are already present in KontsnorOS `devfs` and support terminal emulators (`xterm`, `rxvt`, `st`).

### 2.4 Event Handling
- `epoll`, `poll`, `select`, `timerfd`, `signalfd`, `eventfd`: Essential for event loops in display servers and window managers. KontsnorOS currently has initial implementations for these, which must be verified against Xorg/Weston requirements.

---

## 3. Display Server & Desktop Candidate Selection

### Option A: Standalone Framebuffer X Server + Lightweight WM (Recommended First Target)
- **Stack:** `Xfbdev` (from `xserver-xephyr` or `xserver-xorg-legacy` / `tinyx`) + `openbox` / `jwm` / `twm` / `flwm`.
- **Pros:** Minimal dependencies; requires only `/dev/fb0`, `/dev/input/mice` (or PS/2 mouse), and `AF_UNIX` sockets. Avoids full DRM/KMS hardware requirements.
- **Cons:** Software rasterization; no 3D hardware acceleration.

### Option B: Guest VNC / Framebuffer Server (Robust Remote Debugging)
- **Stack:** `Xvfb` (Virtual Framebuffer X Server) + `x11vnc` / `tigervnc-standalone-server` + `Openbox`.
- **Pros:** Completely detaches graphics rendering from host display drivers. Communicates purely over TCP loopback (`127.0.0.1:5900`).
- **Cons:** Overhead of VNC encoding inside guest userspace.

### Option C: Minimal Wayland Compositor (Future Goal)
- **Stack:** `cage` or `labwc` with Pixman software backend.
- **Pros:** Modern display protocol.
- **Cons:** Requires libinput, systemd/logind or seatd integration, and DRM/KMS `/dev/dri/card0` support.

---

## 4. QEMU & WSL2 Integration

### 4.1 QEMU Display Configuration
For running inside WSL2 without relying on WSLg direct X11/Wayland forwarding overhead or driver glitches:
- **Recommended Mode:** QEMU built-in VNC server (`-vnc 127.0.0.1:0`) or SPICE server (`-spice port=5900,addr=127.0.0.1,disable-ticketing=on`).
- **Host Viewer:** Connect from Windows host or WSL2 using any standard VNC client (e.g., TightVNC, RealVNC, `remmina`, or `remote-viewer`).
- **QEMU Launcher Update (`tools/run-ubuntu-qemu.sh`):**
  ```bash
  qemu-system-x86_64 \
      -vga std \
      -vnc 127.0.0.1:0 \
      -device usb-ehci,id=usb \
      -device usb-tablet \
      ...
  ```

---

## 5. Technical Roadmap & Step-by-Step Milestones

1. **Milestone 1: Kernel Driver Infrastructure (Graphics & Input)**
   - Expose `/dev/fb0` in devfs with `mmap` support and VBE info `ioctl`s.
   - Implement PS/2 Auxiliary Mouse device driver and expose `/dev/input/mice`.

2. **Milestone 2: Essential VT & Memory Syscall Hardening**
   - Verify `AF_UNIX` sockets and POSIX `/dev/shm` mounting.
   - Implement basic `KDSETMODE` VT ioctl support for `/dev/tty`.

3. **Milestone 3: Minimal X11 User Environment Setup**
   - Install `xserver-xorg-video-fbdev`, `xinit`, `x11-apps`, `openbox` inside Ubuntu rootfs.
   - Configure `~/.xinitrc` to launch `openbox` and `xclock` / `xterm`.

4. **Milestone 4: Full GUI Launch & Verification**
   - Launch KontsnorOS in QEMU with `-vga std -vnc 127.0.0.1:0`.
   - Connect via VNC client and verify interactive desktop session.
