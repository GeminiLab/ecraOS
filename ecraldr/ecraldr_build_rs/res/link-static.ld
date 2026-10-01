BASE_ADDRESS = %BASE_ADDRESS%;
ENTRY(_start)

SECTIONS {
    . = BASE_ADDRESS;
    _sloader = .;

    /* Text sections */
    .text : ALIGN(64K) {
        _stext = .;
        *(.text.boot)
        *(.text .text.*)
        . = ALIGN(64K);
        _etext = .;
    }

    /* Read-only data sections */
    _srodata = .;
    .rodata : ALIGN(64K) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.sdata2 .sdata2.*)
    }

    .init_array : ALIGN(0x10) {
        __init_array_start = .;
        *(.init_array .init_array.*)
        __init_array_end = .;
    }

    . = ALIGN(64K);
    _erodata = .;

    /* Data sections */
    .data : ALIGN(64K) {
        _sdata = .;
        *(.data .data.*)
        . = ALIGN(64K);

        /* Boot stack section */
        _sbootstack = .;
        *(.boot_stack)
        _ebootstack = .;

        . = ALIGN(64K);
        _edata = .;
    }

    /* Kernel payload section */
    .kernel : ALIGN(64K) {
        _skernel = .;
        __ecraos_kernel_entry = .;
        *(.kernel)
        . = ALIGN(64K);
        _ekernel = .;
    }

    /* BSS sections */
    .bss : ALIGN(64K) {
        _sbss = .;
        *(.bss .bss.*)
        . = ALIGN(64K);
        _ebss = .;
    }

    _eloader = .;

    /DISCARD/ : {
        *(.comment) *(.eh_frame*) *(.debug*)
    }
}
