SECTIONS
{
    .percpu : ALIGN(64) {
        _percpu_start = .;
        KEEP(*(.percpu .percpu.*))
        _percpu_end = .;
    }

    .pes : ALIGN(64) {
        _percpu_early_slot_start = .;
        KEEP(*(.pes .pes.*))
        . = MAX(., _percpu_early_slot_start + (_percpu_end - _percpu_start));
        _percpu_early_slot_end = .;
    }
}
INSERT AFTER .bss;
