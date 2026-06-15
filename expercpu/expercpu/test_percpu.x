SECTIONS
{
    .percpu : ALIGN(64) {
        _percpu_start = .;
        *(.percpu .percpu.*)
        _percpu_end = .;
    }
    . = _percpu_start + SIZEOF(.percpu);
}
INSERT AFTER .bss;
