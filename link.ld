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

    .data : {
        *(.data .data.*)
    }

    .bss : {
        *(.bss)
    }

    _ekernel = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame .eh_frame*)
    }
}
