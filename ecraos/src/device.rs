use alloc::{
    alloc::{alloc, dealloc},
    vec::Vec,
};
use core::{
    alloc::GlobalAlloc, mem as core_mem, ptr::NonNull, slice, str::FromStr, time::Duration,
};
use exboot::PhysAddr;
use memory_addr::va;

#[cfg(target_arch = "x86_64")]
use acpi::{AmlTable, Handler, PhysicalMapping};
use exarch::{device::DeviceInfoSource, time};
use log::{error, info, warn};

use crate::mem;

#[cfg(target_arch = "x86_64")]
unsafe fn in_u8(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[cfg(target_arch = "x86_64")]
unsafe fn in_u16(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[cfg(target_arch = "x86_64")]
unsafe fn in_u32(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[cfg(target_arch = "x86_64")]
unsafe fn out_u8(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn out_u16(port: u16, value: u16) {
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn out_u32(port: u16, value: u32) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[cfg(target_arch = "x86_64")]
fn pci_config_address(address: acpi::PciAddress, offset: u16) -> u32 {
    0x8000_0000
        | ((address.bus() as u32) << 16)
        | ((address.device() as u32) << 11)
        | ((address.function() as u32) << 8)
        | ((offset as u32) & 0xfc)
}

#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiHandler;

#[cfg(target_arch = "x86_64")]
impl Handler for AcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        let virtual_address = physical_address + mem::vmm::direct_mapping_offset();
        let virtual_start = NonNull::new(virtual_address as *mut T)
            .expect("direct-mapped ACPI physical address must be non-null");

        PhysicalMapping {
            physical_start: physical_address,
            virtual_start,
            region_length: size,
            mapped_length: size,
            handler: *self,
        }
    }

    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {
        // The VMM direct map is permanent for the lifetime of the kernel.
    }

    fn read_u8(&self, address: usize) -> u8 {
        unsafe {
            core::ptr::read_volatile((address + mem::vmm::direct_mapping_offset()) as *const u8)
        }
    }

    fn read_u16(&self, address: usize) -> u16 {
        unsafe {
            core::ptr::read_volatile((address + mem::vmm::direct_mapping_offset()) as *const u16)
        }
    }

    fn read_u32(&self, address: usize) -> u32 {
        unsafe {
            core::ptr::read_volatile((address + mem::vmm::direct_mapping_offset()) as *const u32)
        }
    }

    fn read_u64(&self, address: usize) -> u64 {
        unsafe {
            core::ptr::read_volatile((address + mem::vmm::direct_mapping_offset()) as *const u64)
        }
    }

    fn write_u8(&self, address: usize, value: u8) {
        unsafe {
            core::ptr::write_volatile(
                (address + mem::vmm::direct_mapping_offset()) as *mut u8,
                value,
            )
        }
    }

    fn write_u16(&self, address: usize, value: u16) {
        unsafe {
            core::ptr::write_volatile(
                (address + mem::vmm::direct_mapping_offset()) as *mut u16,
                value,
            )
        }
    }

    fn write_u32(&self, address: usize, value: u32) {
        unsafe {
            core::ptr::write_volatile(
                (address + mem::vmm::direct_mapping_offset()) as *mut u32,
                value,
            )
        }
    }

    fn write_u64(&self, address: usize, value: u64) {
        unsafe {
            core::ptr::write_volatile(
                (address + mem::vmm::direct_mapping_offset()) as *mut u64,
                value,
            )
        }
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            return in_u8(port);
        }

        #[cfg(not(target_arch = "x86_64"))]
        panic!("ACPI system I/O is only implemented on x86_64")
    }

    fn read_io_u16(&self, port: u16) -> u16 {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            return in_u16(port);
        }

        #[cfg(not(target_arch = "x86_64"))]
        panic!("ACPI system I/O is only implemented on x86_64")
    }

    fn read_io_u32(&self, port: u16) -> u32 {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            return in_u32(port);
        }

        #[cfg(not(target_arch = "x86_64"))]
        panic!("ACPI system I/O is only implemented on x86_64")
    }

    fn write_io_u8(&self, port: u16, value: u8) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            out_u8(port, value);
        }

        #[cfg(not(target_arch = "x86_64"))]
        panic!("ACPI system I/O is only implemented on x86_64")
    }

    fn write_io_u16(&self, port: u16, value: u16) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            out_u16(port, value);
        }

        #[cfg(not(target_arch = "x86_64"))]
        panic!("ACPI system I/O is only implemented on x86_64")
    }

    fn write_io_u32(&self, port: u16, value: u32) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            out_u32(port, value);
        }

        #[cfg(not(target_arch = "x86_64"))]
        panic!("ACPI system I/O is only implemented on x86_64")
    }

    fn read_pci_u8(&self, address: acpi::PciAddress, offset: u16) -> u8 {
        let shift = ((offset & 3) * 8) as u32;
        (self.read_pci_u32(address, offset) >> shift) as u8
    }

    fn read_pci_u16(&self, address: acpi::PciAddress, offset: u16) -> u16 {
        let shift = ((offset & 3) * 8) as u32;
        (self.read_pci_u32(address, offset) >> shift) as u16
    }

    fn read_pci_u32(&self, address: acpi::PciAddress, offset: u16) -> u32 {
        self.write_io_u32(0xcf8, pci_config_address(address, offset));
        self.read_io_u32(0xcfc)
    }

    fn write_pci_u8(&self, address: acpi::PciAddress, offset: u16, value: u8) {
        let shift = ((offset & 3) * 8) as u32;
        let mask = 0xff_u32 << shift;
        let old = self.read_pci_u32(address, offset);
        let new = (old & !mask) | ((value as u32) << shift);
        self.write_pci_u32(address, offset, new);
    }

    fn write_pci_u16(&self, address: acpi::PciAddress, offset: u16, value: u16) {
        let shift = ((offset & 3) * 8) as u32;
        let mask = 0xffff_u32 << shift;
        let old = self.read_pci_u32(address, offset);
        let new = (old & !mask) | ((value as u32) << shift);
        self.write_pci_u32(address, offset, new);
    }

    fn write_pci_u32(&self, address: acpi::PciAddress, offset: u16, value: u32) {
        self.write_io_u32(0xcf8, pci_config_address(address, offset));
        self.write_io_u32(0xcfc, value);
    }

    fn nanos_since_boot(&self) -> u64 {
        time::monotonic_time().as_nanos() as u64
    }

    fn stall(&self, microseconds: u64) {
        time::spin_wait_for(Duration::from_micros(microseconds));
    }

    fn sleep(&self, milliseconds: u64) {
        time::spin_wait_for(Duration::from_millis(milliseconds));
    }

    fn create_mutex(&self) -> acpi::Handle {
        acpi::Handle(0)
    }

    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        Ok(())
    }

    fn release(&self, _mutex: acpi::Handle) {}

    fn handle_fatal_error(&self, fatal_type: u8, fatal_code: u32, fatal_arg: u64) {
        warn!(
            "AML fatal error: type {}, code {}, arg {}",
            fatal_type, fatal_code, fatal_arg
        );
    }
}

#[cfg(target_arch = "x86_64")]
fn find_rsdp() -> Option<usize> {
    match unsafe { acpi::rsdp::Rsdp::search_for_on_bios(AcpiHandler) } {
        Ok(mapping) => Some(mapping.physical_start),
        Err(error) => {
            warn!("Failed to find ACPI RSDP via BIOS search: {:?}", error);
            None
        }
    }
}

// TODO: move to exarch
#[cfg(target_arch = "x86_64")]
fn probe_acpi(rsdp: usize) {
    info!("Probing ACPI from RSDP {:#x}", rsdp);

    let tables = match unsafe { acpi::AcpiTables::from_rsdp(AcpiHandler, rsdp) } {
        Ok(tables) => tables,
        Err(error) => {
            warn!(
                "Failed to load ACPI tables from RSDP {:#x}: {:?}",
                rsdp, error
            );
            return;
        }
    };

    for (address, header) in tables.table_headers() {
        let signature = header.signature;
        let length = header.length;
        let revision = header.revision;

        info!(
            "ACPI table {:?} at {:#x}, length {}, revision {}",
            signature, address, length, revision
        );
    }

    print_acpi_processors(&tables);

    #[cfg(false)]
    {
        let platform = match acpi::platform::AcpiPlatform::new(tables, AcpiHandler) {
            Ok(platform) => platform,
            Err(error) => {
                warn!("Failed to construct ACPI platform: {:?}", error);
                return;
            }
        };

        let interpreter = match load_aml_namespace(&platform) {
            Ok(interpreter) => interpreter,
            Err(error) => {
                warn!("Failed to load AML namespace: {:?}", error);
                return;
            }
        };

        info!("Initializing ACPI AML namespace");
        interpreter.initialize_namespace();
        info!("Printing ACPI AML namespace devices");
        print_aml_namespace_devices(&interpreter);
    }
}

#[cfg(target_arch = "x86_64")]
fn print_acpi_processors(tables: &acpi::AcpiTables<AcpiHandler>) {
    use alloc::vec;

    use crate::mp;

    let (_, Some(processors)) = (match acpi::platform::interrupt::InterruptModel::new(tables) {
        Ok(result) => result,
        Err(error) => {
            warn!("Failed to enumerate ACPI processors from MADT: {:?}", error);
            return;
        }
    }) else {
        warn!("ACPI MADT did not report processor information");
        return;
    };

    let mut cpu_ids = vec![];
    print_acpi_processor("BSP", processors.boot_processor);
    cpu_ids.push(processors.boot_processor.local_apic_id as _);

    for processor in processors.application_processors {
        print_acpi_processor("AP", processor);
        cpu_ids.push(processor.local_apic_id as _);
    }

    crate::mp::init_cpu_list(
        cpu_ids.into_boxed_slice(),
        processors.boot_processor.local_apic_id as _,
    );
}

#[cfg(target_arch = "x86_64")]
fn print_acpi_processor(role: &str, processor: acpi::platform::Processor) {
    info!(
        "ACPI processor {}: uid {}, local APIC id {}, state {:?}, is_ap {}",
        role, processor.processor_uid, processor.local_apic_id, processor.state, processor.is_ap
    );
}

#[cfg(target_arch = "x86_64")]
fn load_aml_table(
    handler: AcpiHandler,
    interpreter: &acpi::aml::Interpreter<AcpiHandler>,
    table: AmlTable,
    label: &str,
) -> Result<(), acpi::AcpiError> {
    info!(
        "Loading ACPI AML {} at {:#x}, length {}, revision {}",
        label, table.phys_address, table.length, table.revision
    );

    let mapping = unsafe {
        handler
            .map_physical_region::<acpi::sdt::SdtHeader>(table.phys_address, table.length as usize)
    };
    let stream = unsafe {
        slice::from_raw_parts(
            mapping
                .virtual_start
                .as_ptr()
                .byte_add(core_mem::size_of::<acpi::sdt::SdtHeader>()) as *const u8,
            table.length as usize - core_mem::size_of::<acpi::sdt::SdtHeader>(),
        )
    };

    interpreter.load_table(stream).map_err(acpi::AcpiError::Aml)
}

#[cfg(target_arch = "x86_64")]
fn load_aml_namespace(
    platform: &acpi::platform::AcpiPlatform<AcpiHandler>,
) -> Result<acpi::aml::Interpreter<AcpiHandler>, acpi::AcpiError> {
    let facs = {
        platform
            .tables
            .find_table::<acpi::sdt::fadt::Fadt>()
            .and_then(|fadt| fadt.facs_address().ok())
            .map(|facs_address| {
                info!("Mapping ACPI FACS at {:#x}", facs_address);
                unsafe {
                    platform.handler.map_physical_region(
                        facs_address,
                        core_mem::size_of::<acpi::sdt::facs::Facs>(),
                    )
                }
            })
    };

    let dsdt = platform.tables.dsdt()?;
    info!("Creating ACPI AML interpreter");
    let interpreter = acpi::aml::Interpreter::new(
        platform.handler,
        dsdt.revision,
        platform.registers.clone(),
        facs,
    );

    load_aml_table(platform.handler, &interpreter, dsdt, "DSDT")?;

    for ssdt in platform.tables.ssdts() {
        load_aml_table(platform.handler, &interpreter, ssdt, "SSDT")?;
    }

    Ok(interpreter)
}

#[cfg(target_arch = "x86_64")]
fn print_aml_namespace_devices(interpreter: &acpi::aml::Interpreter<AcpiHandler>) {
    let mut namespace = interpreter.namespace.lock().clone();
    let result = namespace.traverse(|path, level| {
        use acpi::aml::namespace::NamespaceLevelKind;

        let is_device_like = matches!(
            level.kind,
            NamespaceLevelKind::Device
                | NamespaceLevelKind::Processor
                | NamespaceLevelKind::ThermalZone
                | NamespaceLevelKind::PowerResource
        );

        if is_device_like {
            info!("ACPI namespace {:?}: {}", level.kind, path);
            print_aml_field(interpreter, path, "_STA");
            print_aml_field(interpreter, path, "_HID");
            print_aml_field(interpreter, path, "_CID");
            print_aml_field(interpreter, path, "_UID");
            print_aml_field(interpreter, path, "_ADR");
            print_aml_field(interpreter, path, "_STR");
        }

        Ok(true)
    });

    if let Err(error) = result {
        warn!("Failed while traversing ACPI namespace: {:?}", error);
    }
}

#[cfg(target_arch = "x86_64")]
fn print_aml_field(
    interpreter: &acpi::aml::Interpreter<AcpiHandler>,
    path: &acpi::aml::namespace::AmlName,
    field: &str,
) {
    let Ok(field_name) = acpi::aml::namespace::AmlName::from_str(field) else {
        return;
    };
    let Ok(field_path) = field_name.resolve(path) else {
        return;
    };

    match interpreter.evaluate_if_present(field_path, Vec::new()) {
        Ok(Some(value)) => info!("  {} = {}", field, *value),
        Ok(None) => {}
        Err(error) => warn!("  {} evaluation failed: {:?}", field, error),
    }
}

pub fn probe_device_info_source(boot_arg: exboot::PlatformBootArg) -> Vec<DeviceInfoSource> {
    let mut result = Vec::new();

    #[cfg(target_arch = "x86_64")]
    {
        if matches!(boot_arg, exboot::PlatformBootArg::Multiboot(_)) {
            if let Some(rsdp) = find_rsdp() {
                log::info!("Found ACPI RSDP at {:#x}", rsdp);
                result.push(DeviceInfoSource::ACPI(rsdp.into()));
            } else {
                log::warn!("No ACPI RSDP found");
            }
        }
    }

    #[cfg(target_arch = "riscv64")]
    {
        if let exboot::PlatformBootArg::DeviceTree(dtb) = boot_arg {
            log::info!("Found Device Tree at {:#x}", dtb.as_usize());
            result.push(DeviceInfoSource::DeviceTree(dtb));
        }
    }

    result
}

fn probe_device_tree(addr: PhysAddr) {
    use fdt_rs::{base::*, index::*, prelude::*};

    // Only detect CPUs here
    let vaddr = va!(addr.as_usize() + crate::mem::vmm::direct_mapping_offset());

    let dtb = unsafe { DevTree::from_raw_pointer(vaddr.as_ptr()).expect("failed to load dtb") };
    let dtb_boot_cpuid_phys = dtb.boot_cpuid_phys();
    let runtime_boot_cpuid_phys = crate::mp::current_cpu_phys_id();
    info!(
        "Boot CPU ID: runtime {:#x}, device tree {:#x}",
        runtime_boot_cpuid_phys, dtb_boot_cpuid_phys
    );
    if runtime_boot_cpuid_phys != dtb_boot_cpuid_phys as usize {
        warn!(
            "Device Tree boot CPU ID {:#x} differs from runtime hart ID {:#x}; using runtime hart ID as BSP",
            dtb_boot_cpuid_phys, runtime_boot_cpuid_phys
        );
    }

    let layout = DevTreeIndex::get_layout(&dtb).expect("failed to get layout");
    let buf_ptr = unsafe { alloc(layout) };
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, layout.size()) };
    let index = DevTreeIndex::new(dtb, buf).expect("failed to create index");

    let _ = index;

    let cpu_nodes = dtb.nodes().filter(|node| {
        node.props()
            .find(|p| {
                if p.name()? != "device_type" {
                    return Ok(false);
                }
                Ok(p.str()? == "cpu")
            })
            .map(|p| p.is_some())
    });

    let mut cpu_ids = Vec::new();
    for node in cpu_nodes.iterator() {
        let node = node.unwrap_or_else(|e| panic!("failed to get cpu node: {:?}", e));

        let cpu_id = node
            .props()
            .find(|p| p.name().map(|name| name == "reg"))
            .unwrap_or_else(|e| panic!("failed to get CPU node reg property: {:?}", e))
            .unwrap_or_else(|| {
                panic!(
                    "CPU node {} has no reg property",
                    node.name().unwrap_or_default()
                )
            })
            .u32(0)
            .unwrap_or_else(|e| panic!("failed to parse CPU node reg property: {:?}", e));

        info!(
            "Found CPU node {}: hart ID {:#x}",
            node.name().unwrap_or_default(),
            cpu_id
        );
        cpu_ids.push(cpu_id as _);
    }

    if cpu_ids.is_empty() {
        panic!("no CPU nodes found in Device Tree");
    }
    if !cpu_ids.contains(&runtime_boot_cpuid_phys) {
        panic!(
            "runtime BSP hart ID {:#x} is not present in Device Tree CPU nodes",
            runtime_boot_cpuid_phys
        );
    }

    drop(index);
    unsafe { dealloc(buf_ptr, layout) };

    crate::mp::init_cpu_list(cpu_ids.into_boxed_slice(), runtime_boot_cpuid_phys);
}

/// Probes and prints platform device information sources.
///
/// This routine runs after the VMM and heap are initialized. It is best-effort
/// and should not prevent the kernel from continuing.
pub fn probe_devices(boot_arg: exboot::PlatformBootArg) {
    let sources = probe_device_info_source(boot_arg);
    info!("Device information sources: {:?}", sources);

    for source in sources {
        match source {
            #[cfg(target_arch = "x86_64")]
            DeviceInfoSource::ACPI(rsdp) => probe_acpi(rsdp.as_usize()),
            DeviceInfoSource::DeviceTree(addr) => {
                probe_device_tree(addr);
            }
            #[cfg(not(target_arch = "x86_64"))]
            DeviceInfoSource::ACPI(addr) => {
                info!(
                    "ACPI source at {:#x} is not supported on this target",
                    addr.as_usize()
                );
            }
        }
    }
}
