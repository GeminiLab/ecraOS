ENTRY(__ecraos_kernel_entry)

SECTIONS {
    /*
        The kernel, the .text.kernel_entry section, and the symbol
        `__ecraos_kernel_entry` must be placed at VMA 0, for relocator to work
        properly.
    */
    . = 0;
    _skernel = .;

    /* Text sections */
    .text : ALIGN(64K) {
        _stext = .;
        *(.text.kernel_entry)
        *(.text .text.*)
        _etext = .;
        . = ALIGN(64K);
        _ftext = .;
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

    _erodata = .;
    . = ALIGN(64K);
    _frodata = .;

    /* PerCPU data sections */
    .percpu : ALIGN(64K) {
        _spercpu = .;
        _percpu_start = .;
        *(.percpu .percpu.*)
        _percpu_end = .;
        _epercpu = .;
        . = ALIGN(64K);
        _fpercpu = .;
    }

    /* Data sections */
    .data : ALIGN(64K) {
        _sdata = .;
        *(.data .data.*)
        _edata = .;
        . = ALIGN(64K);
        _fdata = .;
    }

    /* Relocation sections */
    .rela.dyn : ALIGN(64K) {
        _srela_dyn = .;
        *(.rela.dyn)
        _erela_dyn = .;
        . = ALIGN(64K);
        _frela_dyn = .;
    }

    .got : ALIGN(64K) {
        _sgot = .;
        *(.got)
        _egot = .;
        . = ALIGN(64K);
        _fgot = .;
    }

    /* BSS sections */
    .bss : ALIGN(64K) {
        _sbss = .;

        /* PerCPU early slot reservation */
        _percpu_early_slot_start = .;
        . = MAX(., _percpu_early_slot_start + (_percpu_end - _percpu_start));
        _percpu_early_slot_end = .;

        *(.bss .bss.*)
        _ebss = .;
        . = ALIGN(64K);
        _fbss = .;
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
