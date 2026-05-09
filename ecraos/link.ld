ENTRY(__exboot_kernel_entry)

SECTIONS {
    /*
        The kernel, the .text.kernel_entry section, and the symbol
        `__exboot_kernel_entry` must be placed at VMA 0, for relocator to work
        properly.
    */
    . = 0;
    _skernel = .;

    /* Text sections */
    .text : ALIGN(4K) {
        _stext = .;
        *(.text.kernel_entry)
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
        _edata = .;
    }

    /* Relocation sections */
    .rela.dyn : ALIGN(4K) {
        _srela_dyn = .;
        *(.rela.dyn)
        _erela_dyn = .;
    }

    .got : ALIGN(4K) {
        _sgot = .;
        *(.got)
        _egot = .;
    }

    /* BSS sections */
    .bss : ALIGN(4K) {
        _sbss = .;
        *(.bss .bss.*)
        . = ALIGN(4K);
        _ebss = .;
    }

    _ekernel = .;

    /* 
        .symtab, .shstrtab and .strtab are kept implicitly by the linker
        for debugging purposes.
    */

    /DISCARD/ : {
        *(.comment) *(.gnu*) *(.note*) *(.eh_frame*) *(.debug*)
        *(.dynsym) *(.dynstr) *(.hash) *(.dynamic)
    }
}
