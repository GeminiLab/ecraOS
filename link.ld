BASE_ADDRESS = 0x200000;
ENTRY(_start)

SECTIONS {
    . = BASE_ADDRESS;
    _skernel = .;

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
    }
    . = ALIGN(4K);
    _edata = .;

    /* BSS sections */
    .bss : ALIGN(4K) {
        _sbss = .;
        *(.bss)
    }
    . = ALIGN(4K);
    _ebss = .;

    _ekernel = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame*)
    }
}
