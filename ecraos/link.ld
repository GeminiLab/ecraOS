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
    .text : ALIGN(4K) {
        _stext = .;
        *(.text.kernel_entry)
        *(.text .text.*)
        _etext = .;
        . = ALIGN(4K);
        _ftext = .;
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

    _erodata = .;
    . = ALIGN(4K);
    _frodata = .;

    /* PerCPU data sections */
    .percpu : ALIGN(4K) {
        _spercpu = .;
        _percpu_start = .;
        *(.percpu .percpu.*)
        _percpu_end = .;
        _epercpu = .;
        . = ALIGN(4K);
        _fpercpu = .;
    }

    /* Data sections */
    .data : ALIGN(4K) {
        _sdata = .;
        *(.data .data.*)
        _edata = .;
        . = ALIGN(4K);
        _fdata = .;
    }

    /* Relocation sections */
    .rela.dyn : ALIGN(4K) {
        _srela_dyn = .;
        *(.rela.dyn)
        _erela_dyn = .;
        . = ALIGN(4K);
        _frela_dyn = .;
    }

    .got : ALIGN(4K) {
        _sgot = .;
        *(.got)
        _egot = .;
        . = ALIGN(4K);
        _fgot = .;
    }

    /* BSS sections */
    .bss : ALIGN(4K) {
        _sbss = .;

        /* PerCPU early slot reservation */
        _percpu_early_slot_start = .;
        . = MAX(., _percpu_early_slot_start + (_percpu_end - _percpu_start));
        _percpu_early_slot_end = .;

        *(.bss .bss.*)
        _ebss = .;
        . = ALIGN(4K);
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
