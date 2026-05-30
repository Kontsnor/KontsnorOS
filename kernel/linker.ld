/* KontsnorOS Linker Script — x86_64 */
/* Maps the kernel into the higher-half virtual address space */

ENTRY(_start)

KERNEL_OFFSET = 0xFFFFFFFF80000000;

SECTIONS
{
    /* Place kernel at conventional higher-half address */
    . = KERNEL_OFFSET + 0x100000;

    /* Text (code) section */
    .text ALIGN(4K) :
    {
        KEEP(*(.text.boot))    /* Boot entry point first */
        *(.text .text.*)
    }

    /* Read-only data */
    .rodata ALIGN(4K) :
    {
        *(.rodata .rodata.*)
    }

    /* Bootloader configuration */
    . = ALIGN(4K);
    .bootloader-config :
    {
        KEEP(*(.bootloader-config))
    }

    /* Global Offset Table (GOT) */
    . = ALIGN(4K);
    .got :
    {
        *(.got)
        *(.got.plt)
    }

    /* Initialized data */
    .data ALIGN(4K) :
    {
        *(.data .data.*)
    }

    /* Thread-local storage */
    .tdata ALIGN(4K) :
    {
        _tdata_start = .;
        *(.tdata .tdata.*)
        _tdata_end = .;
    }

    .tbss ALIGN(4K) :
    {
        _tbss_start = .;
        *(.tbss .tbss.*)
        _tbss_end = .;
    }

    /* Uninitialized data (BSS) */
    .bss ALIGN(4K) :
    {
        _bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        _bss_end = .;
    }

    /* Kernel end marker */
    _kernel_end = .;

    /* Discard unnecessary sections */
    /DISCARD/ :
    {
        *(.comment)
        *(.eh_frame*)
    }
}
