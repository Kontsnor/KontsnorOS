// Copyright (C) 2026 KontsnorOS Contributors
//
// Minimal fbdev userspace test program.
// Validates FBIOGET_VSCREENINFO, FBIOGET_FSCREENINFO stride calculation,
// mmap(MAP_SHARED) of /dev/fb0, and renders a test gradient.

#include <sys/mman.h>
#include <sys/ioctl.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <linux/fb.h>

int main(void) {
    int fd = open("/dev/fb0", O_RDWR);
    if (fd < 0) {
        perror("open /dev/fb0");
        return 1;
    }

    struct fb_var_screeninfo vinfo;
    struct fb_fix_screeninfo finfo;
    if (ioctl(fd, FBIOGET_VSCREENINFO, &vinfo)) {
        perror("VSCREENINFO");
        close(fd);
        return 1;
    }
    if (ioctl(fd, FBIOGET_FSCREENINFO, &finfo)) {
        perror("FSCREENINFO");
        close(fd);
        return 1;
    }

    printf("Resolution: %dx%d bpp=%d stride=%d\n",
           vinfo.xres, vinfo.yres, vinfo.bits_per_pixel, finfo.line_length);

    // Validate: stride must be xres * 4 for 32bpp
    if (finfo.line_length != vinfo.xres * 4) {
        fprintf(stderr, "FAIL: line_length=%d expected=%d\n",
                finfo.line_length, vinfo.xres * 4);
        close(fd);
        return 1;
    }

    uint32_t *fb = (uint32_t *)mmap(NULL, finfo.smem_len,
                                    PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (fb == MAP_FAILED) {
        perror("mmap");
        close(fd);
        return 1;
    }

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
