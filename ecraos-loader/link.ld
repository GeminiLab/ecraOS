/*
    TODO: how to handle this?
*/
BASE_ADDRESS = 0x80200000;
/* BASE_ADDRESS = 0x200000; */
ENTRY(_start)

SECTIONS {
    . = BASE_ADDRESS;
    _sloader = .;

    /* Text sections */
    .text : ALIGN(4K) {
        _stext = .;
        *(.text.boot)
        *(.text .text.*)
        . = ALIGN(4K);
        _etext = .;
    }

    /* Read-only data sections */
    _srodata = .;
    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.sdata2 .sdata2.*)
    }

    .init_array : ALIGN(0x10) {
        __init_array_start = .;
        *(.init_array .init_array.*)
        __init_array_end = .;
    }

    . = ALIGN(4K);
    _erodata = .;

    /* Data sections */
    .data : ALIGN(4K) {
        _sdata = .;
        *(.data .data.*)
        . = ALIGN(4K);

        /* Boot stack section */
        _sbootstack = .;
        *(.boot_stack)
        _ebootstack = .;

        . = ALIGN(4K);
        _edata = .;
    }

    /* Kernel payload section */
    .kernel : ALIGN(4K) {
        _skernel = .;
        __exboot_kernel_entry = .;
        *(.kernel)
        . = ALIGN(4K);
        _ekernel = .;
    }

    /* BSS sections */
    .bss : ALIGN(4K) {
        _sbss = .;
        *(.bss .bss.*)
        . = ALIGN(4K);
        _ebss = .;
    }

    _eloader = .;

    /DISCARD/ : {
        *(.comment) *(.eh_frame*) *(.debug*)
    }
}
