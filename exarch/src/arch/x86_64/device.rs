//! x86-64 ACPI platform discovery.

use alloc::vec::Vec;
use core::{ptr::NonNull, time::Duration};

use acpi::{AcpiTables, Handler, PhysicalMapping};
use ecraldr_base::{PhysAddr, PlatformBootArg};

use crate::{
    kernel_if::phys_to_virt,
    platform::{CpuTopology, PlatformDescription, PlatformInterruptController},
    power::PhysicalCpuId,
    time,
    trap::irq::{IoApicConfig, Polarity, TriggerMode, X86ExternalIrqConfig, X86IrqOverride},
};

/// Reads one byte from an x86 I/O port.
unsafe fn in_u8(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Reads one word from an x86 I/O port.
unsafe fn in_u16(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Reads one double word from an x86 I/O port.
unsafe fn in_u32(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Writes one byte to an x86 I/O port.
unsafe fn out_u8(port: u16, value: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Writes one word to an x86 I/O port.
unsafe fn out_u16(port: u16, value: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
}

/// Writes one double word to an x86 I/O port.
unsafe fn out_u32(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

/// Builds an address for the legacy x86 PCI configuration mechanism.
fn pci_config_address(address: acpi::PciAddress, offset: u16) -> u32 {
    0x8000_0000
        | ((address.bus() as u32) << 16)
        | ((address.device() as u32) << 11)
        | ((address.function() as u32) << 8)
        | ((offset as u32) & 0xfc)
}

/// Provides ACPI with access to the kernel's permanent physical mapping and platform services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcpiHandler;

impl Handler for AcpiHandler {
    /// Maps an ACPI physical region through the kernel direct map.
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        let virtual_start =
            NonNull::new(phys_to_virt(PhysAddr::from_usize(physical_address)).as_mut_ptr_of::<T>())
                .expect("direct-mapped ACPI address must be non-null");
        PhysicalMapping {
            physical_start: physical_address,
            virtual_start,
            region_length: size,
            mapped_length: size,
            handler: *self,
        }
    }

    /// Leaves an ACPI mapping in place because the direct map is permanent.
    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {}

    /// Reads an ACPI byte through the direct map.
    fn read_u8(&self, address: usize) -> u8 {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_ptr_of::<u8>()
                .read_volatile()
        }
    }
    /// Reads an ACPI word through the direct map.
    fn read_u16(&self, address: usize) -> u16 {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_ptr_of::<u16>()
                .read_volatile()
        }
    }
    /// Reads an ACPI double word through the direct map.
    fn read_u32(&self, address: usize) -> u32 {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_ptr_of::<u32>()
                .read_volatile()
        }
    }
    /// Reads an ACPI quad word through the direct map.
    fn read_u64(&self, address: usize) -> u64 {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_ptr_of::<u64>()
                .read_volatile()
        }
    }
    /// Writes an ACPI byte through the direct map.
    fn write_u8(&self, address: usize, value: u8) {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_mut_ptr_of::<u8>()
                .write_volatile(value)
        }
    }
    /// Writes an ACPI word through the direct map.
    fn write_u16(&self, address: usize, value: u16) {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_mut_ptr_of::<u16>()
                .write_volatile(value)
        }
    }
    /// Writes an ACPI double word through the direct map.
    fn write_u32(&self, address: usize, value: u32) {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_mut_ptr_of::<u32>()
                .write_volatile(value)
        }
    }
    /// Writes an ACPI quad word through the direct map.
    fn write_u64(&self, address: usize, value: u64) {
        unsafe {
            phys_to_virt(PhysAddr::from_usize(address))
                .as_mut_ptr_of::<u64>()
                .write_volatile(value)
        }
    }
    /// Reads an x86 I/O byte.
    fn read_io_u8(&self, port: u16) -> u8 {
        unsafe { in_u8(port) }
    }
    /// Reads an x86 I/O word.
    fn read_io_u16(&self, port: u16) -> u16 {
        unsafe { in_u16(port) }
    }
    /// Reads an x86 I/O double word.
    fn read_io_u32(&self, port: u16) -> u32 {
        unsafe { in_u32(port) }
    }
    /// Writes an x86 I/O byte.
    fn write_io_u8(&self, port: u16, value: u8) {
        unsafe { out_u8(port, value) }
    }
    /// Writes an x86 I/O word.
    fn write_io_u16(&self, port: u16, value: u16) {
        unsafe { out_u16(port, value) }
    }
    /// Writes an x86 I/O double word.
    fn write_io_u32(&self, port: u16, value: u32) {
        unsafe { out_u32(port, value) }
    }
    /// Reads one byte from PCI configuration space.
    fn read_pci_u8(&self, address: acpi::PciAddress, offset: u16) -> u8 {
        (self.read_pci_u32(address, offset) >> ((offset & 3) * 8)) as u8
    }
    /// Reads one word from PCI configuration space.
    fn read_pci_u16(&self, address: acpi::PciAddress, offset: u16) -> u16 {
        (self.read_pci_u32(address, offset) >> ((offset & 3) * 8)) as u16
    }
    /// Reads one double word from PCI configuration space.
    fn read_pci_u32(&self, address: acpi::PciAddress, offset: u16) -> u32 {
        self.write_io_u32(0xcf8, pci_config_address(address, offset));
        self.read_io_u32(0xcfc)
    }
    /// Writes one byte to PCI configuration space.
    fn write_pci_u8(&self, address: acpi::PciAddress, offset: u16, value: u8) {
        let shift = (offset & 3) * 8;
        let mask = 0xff_u32 << shift;
        let old = self.read_pci_u32(address, offset);
        self.write_pci_u32(address, offset, (old & !mask) | (u32::from(value) << shift));
    }
    /// Writes one word to PCI configuration space.
    fn write_pci_u16(&self, address: acpi::PciAddress, offset: u16, value: u16) {
        let shift = (offset & 3) * 8;
        let mask = 0xffff_u32 << shift;
        let old = self.read_pci_u32(address, offset);
        self.write_pci_u32(address, offset, (old & !mask) | (u32::from(value) << shift));
    }
    /// Writes one double word to PCI configuration space.
    fn write_pci_u32(&self, address: acpi::PciAddress, offset: u16, value: u32) {
        self.write_io_u32(0xcf8, pci_config_address(address, offset));
        self.write_io_u32(0xcfc, value);
    }
    /// Returns the monotonic time used by ACPI.
    fn nanos_since_boot(&self) -> u64 {
        time::monotonic_time().as_nanos() as u64
    }
    /// Busy-waits for the requested ACPI stall interval.
    fn stall(&self, microseconds: u64) {
        time::spin_wait_for(Duration::from_micros(microseconds));
    }
    /// Busy-waits for the requested ACPI sleep interval.
    fn sleep(&self, milliseconds: u64) {
        time::spin_wait_for(Duration::from_millis(milliseconds));
    }

    /// Creates the placeholder mutex used by the current single-threaded ACPI setup.
    fn create_mutex(&self) -> acpi::Handle {
        acpi::Handle(0)
    }

    /// Acquires the placeholder ACPI mutex.
    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        Ok(())
    }

    /// Releases the placeholder ACPI mutex.
    fn release(&self, _mutex: acpi::Handle) {}
}

/// Locates the BIOS-provided ACPI root pointer.
fn find_rsdp() -> usize {
    unsafe { acpi::rsdp::Rsdp::search_for_on_bios(AcpiHandler) }
        .expect("failed to find ACPI RSDP")
        .physical_start
}

/// Discovers the x86-64 CPU topology and external interrupt controller.
pub fn discover_platform(arg: PlatformBootArg) -> PlatformDescription {
    assert!(
        matches!(arg, PlatformBootArg::Multiboot(_)),
        "x86-64 platform discovery requires Multiboot boot information"
    );
    let rsdp = find_rsdp();
    log::info!("Found ACPI RSDP at {rsdp:#x}");
    let tables =
        unsafe { AcpiTables::from_rsdp(AcpiHandler, rsdp) }.expect("failed to load ACPI tables");
    let (interrupt_model, processors) = acpi::platform::interrupt::InterruptModel::new(&tables)
        .expect("failed to enumerate ACPI interrupt topology");
    let processors = processors.expect("ACPI did not report processor information");

    let mut cpu_ids = Vec::with_capacity(1 + processors.application_processors.len());
    cpu_ids.push(processors.boot_processor.local_apic_id as PhysicalCpuId);
    cpu_ids.extend(
        processors
            .application_processors
            .iter()
            .map(|processor| processor.local_apic_id as PhysicalCpuId),
    );
    let cpu_topology = CpuTopology::new(
        cpu_ids.into_boxed_slice(),
        processors.boot_processor.local_apic_id as PhysicalCpuId,
    );

    let acpi::platform::interrupt::InterruptModel::Apic(apic) = interrupt_model else {
        panic!("ACPI did not report an APIC interrupt model")
    };
    let io_apics = apic
        .io_apics
        .iter()
        .map(|ioapic| IoApicConfig {
            id: ioapic.id,
            physical_base: PhysAddr::from_usize(ioapic.address as usize),
            gsi_base: ioapic.global_system_interrupt_base,
            pin_count: 0,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let overrides = apic
        .interrupt_source_overrides
        .iter()
        .map(|entry| X86IrqOverride {
            isa_source: entry.isa_source,
            gsi: entry.global_system_interrupt,
            polarity: match entry.polarity {
                acpi::platform::interrupt::Polarity::SameAsBus
                | acpi::platform::interrupt::Polarity::ActiveHigh => Polarity::ActiveHigh,
                acpi::platform::interrupt::Polarity::ActiveLow => Polarity::ActiveLow,
            },
            trigger: match entry.trigger_mode {
                acpi::platform::interrupt::TriggerMode::SameAsBus
                | acpi::platform::interrupt::TriggerMode::Edge => TriggerMode::Edge,
                acpi::platform::interrupt::TriggerMode::Level => TriggerMode::Level,
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let controller = X86ExternalIrqConfig {
        io_apics,
        overrides,
        bsp_apic_id: processors.boot_processor.local_apic_id,
    };

    PlatformDescription::new(
        cpu_topology,
        PlatformInterruptController::X86_64(controller),
    )
}
