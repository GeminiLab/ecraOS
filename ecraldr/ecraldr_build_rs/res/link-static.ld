BASE_ADDRESS = %BASE_ADDRESS%;
ENTRY(_start)

SECTIONS {
    . = BASE_ADDRESS;
    _sloader = .;

    /* Text sections */
    .text : ALIGN(%SECTION_ALIGN%) {
        _stext = .;
        *(.text.boot)
        *(.text .text.*)
        . = ALIGN(%SECTION_ALIGN%);
        _etext = .;
    }

    /* Read-only data sections */
    _srodata = .;
    .rodata : ALIGN(%SECTION_ALIGN%) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.sdata2 .sdata2.*)
    }

    .init_array : ALIGN(0x10) {
        __init_array_start = .;
        *(.init_array .init_array.*)
        __init_array_end = .;
    }

    . = ALIGN(%SECTION_ALIGN%);
    _erodata = .;

    /* Data sections */
    .data : ALIGN(%SECTION_ALIGN%) {
        _sdata = .;
        *(.data .data.*)
        . = ALIGN(%SECTION_ALIGN%);

        /* Boot stack section */
        _sbootstack = .;
        *(.boot_stack)
        _ebootstack = .;

        . = ALIGN(%SECTION_ALIGN%);
        _edata = .;
    }

    /* Kernel payload section */
    .kernel : ALIGN(%SECTION_ALIGN%) {
        _skernel = .;
        __ecraos_kernel_entry = .;
        *(.kernel)
        . = ALIGN(%SECTION_ALIGN%);
        _ekernel = .;
    }

    /* BSS sections */
    .bss : ALIGN(%SECTION_ALIGN%) {
        _sbss = .;
        *(.bss .bss.*)
        . = ALIGN(%SECTION_ALIGN%);
        _ebss = .;
    }

    _eloader = .;

    /DISCARD/ : {
        *(.comment) *(.eh_frame*) *(.debug*)
    }
}
