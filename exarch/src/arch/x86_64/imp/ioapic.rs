use alloc::{boxed::Box, vec::Vec};
use kspin::SpinNoIrq;
use memory_addr::PhysAddr;
use x2apic::ioapic::{IoApic, IrqFlags, IrqMode};

/// Describes one IOAPIC discovered from ACPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoApicConfig {
    /// Identifies the IOAPIC in the ACPI topology.
    pub id: u8,
    /// Stores the physical MMIO base address.
    pub physical_base: PhysAddr,
    /// Stores the first GSI owned by this IOAPIC.
    pub gsi_base: u32,
    /// Stores the number of redirection-table pins.
    pub pin_count: u32,
}

/// Reports invalid IOAPIC topology configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoApicConfigError {
    /// Reports an IOAPIC with no usable pins.
    ZeroPins,
    /// Reports an overflowing GSI range.
    GsiRangeOverflow,
    /// Reports overlapping GSI ranges.
    OverlappingGsiRanges,
    /// Reports duplicate IOAPIC identifiers.
    DuplicateId,
    /// Reports an ACPI pin count that disagrees with the hardware version register.
    PinCountMismatch,
}

/// Owns the immutable IOAPIC topology and GSI lookup operations.
pub struct IoApicTopology {
    controllers: Box<[IoApicConfig]>,
}

impl IoApicTopology {
    /// Validates and stores the discovered IOAPIC topology.
    pub fn new(configs: Vec<IoApicConfig>) -> Result<Self, IoApicConfigError> {
        for (index, config) in configs.iter().enumerate() {
            if config.pin_count == 0 {
                return Err(IoApicConfigError::ZeroPins);
            }
            if config.gsi_base.checked_add(config.pin_count).is_none() {
                return Err(IoApicConfigError::GsiRangeOverflow);
            }
            if configs[..index]
                .iter()
                .any(|previous| previous.id == config.id)
            {
                return Err(IoApicConfigError::DuplicateId);
            }
            let end = config.gsi_base + config.pin_count;
            if configs[..index].iter().any(|previous| {
                let previous_end = previous.gsi_base + previous.pin_count;
                config.gsi_base < previous_end && previous.gsi_base < end
            }) {
                return Err(IoApicConfigError::OverlappingGsiRanges);
            }
        }

        Ok(Self {
            controllers: configs.into_boxed_slice(),
        })
    }

    /// Locates the controller ID and local pin for a GSI.
    pub fn locate_gsi(&self, gsi: u32) -> Option<(u8, u32)> {
        self.controllers.iter().find_map(|config| {
            let pin = gsi.checked_sub(config.gsi_base)?;
            (pin < config.pin_count).then_some((config.id, pin))
        })
    }
}

struct IoApicController {
    config: IoApicConfig,
    ioapic: SpinNoIrq<IoApic>,
}

/// Owns mapped IOAPIC controllers and their redirection entries.
pub struct IoApicSet {
    topology: IoApicTopology,
    controllers: Box<[IoApicController]>,
}

/// Describes the signal polarity of an external x86 source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// Delivers an active-high signal.
    ActiveHigh,
    /// Delivers an active-low signal.
    ActiveLow,
}

/// Describes the trigger mode of an external x86 source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// Delivers an edge transition.
    Edge,
    /// Delivers an asserted level.
    Level,
}

impl IoApicSet {
    /// Returns one past the highest GSI represented by the hardware set.
    pub fn gsi_limit(&self) -> usize {
        self.controllers
            .iter()
            .map(|controller| {
                controller.config.gsi_base as usize + controller.config.pin_count as usize
            })
            .max()
            .unwrap_or(0)
    }

    /// Reports whether a GSI belongs to a discovered IOAPIC pin range.
    pub fn contains_gsi(&self, gsi: u32) -> bool {
        self.topology.locate_gsi(gsi).is_some()
    }

    /// Maps and initializes IOAPIC controllers from ACPI configuration.
    pub unsafe fn new(configs: Vec<IoApicConfig>) -> Result<Self, IoApicConfigError> {
        let mut controllers = Vec::with_capacity(configs.len());
        let mut hardware_configs = Vec::with_capacity(configs.len());
        for mut config in configs {
            let virtual_base = crate::kernel_if::phys_to_virt(config.physical_base);
            // SAFETY: The caller has verified that the VMM maps this physical MMIO page as device
            // memory before constructing the controller.
            let ioapic = unsafe { IoApic::new(virtual_base.as_usize() as u64) };
            let mut ioapic = ioapic;
            // The ACPI table does not carry the redirection-table length. Read the
            // hardware version register and use it as the authoritative pin count.
            let hardware_pin_count = unsafe { ioapic.max_table_entry() as u32 + 1 };
            if config.pin_count != 0 && config.pin_count != hardware_pin_count {
                return Err(IoApicConfigError::PinCountMismatch);
            }
            config.pin_count = hardware_pin_count;
            hardware_configs.push(config);
            let config = config;
            let controller = IoApicController {
                config,
                ioapic: SpinNoIrq::new(ioapic),
            };
            for pin in 0..config.pin_count {
                let pin = u8::try_from(pin).expect("IOAPIC pin count exceeds the register width");
                // SAFETY: The pin was checked against the ACPI-discovered controller range.
                let mut ioapic = controller.ioapic.lock();
                let mut entry = unsafe { ioapic.table_entry(pin) };
                entry.set_flags(IrqFlags::MASKED);
                unsafe { ioapic.set_table_entry(pin, entry) };
            }
            controllers.push(controller);
        }
        let topology = IoApicTopology::new(hardware_configs)?;
        Ok(Self {
            topology,
            controllers: controllers.into_boxed_slice(),
        })
    }

    /// Programs a masked redirection entry for a GSI.
    pub fn prepare_source(
        &self,
        gsi: u32,
        vector: u8,
        destination: u8,
        polarity: Polarity,
        trigger: TriggerMode,
    ) -> Result<(), IoApicConfigError> {
        let (id, pin) = self
            .topology
            .locate_gsi(gsi)
            .ok_or(IoApicConfigError::GsiRangeOverflow)?;
        let controller = self
            .controllers
            .iter()
            .find(|controller| controller.config.id == id)
            .expect("IOAPIC topology and hardware state diverged");
        let mut ioapic = controller.ioapic.lock();
        let pin = u8::try_from(pin).expect("IOAPIC pin count exceeds the register width");
        // SAFETY: The pin belongs to this initialized controller.
        let mut entry = unsafe { ioapic.table_entry(pin) };
        entry.set_vector(vector);
        entry.set_mode(IrqMode::Fixed);
        entry.set_dest(destination);
        let mut flags = IrqFlags::MASKED;
        if matches!(polarity, Polarity::ActiveLow) {
            flags |= IrqFlags::LOW_ACTIVE;
        }
        if matches!(trigger, TriggerMode::Level) {
            flags |= IrqFlags::LEVEL_TRIGGERED;
        }
        entry.set_flags(flags);
        // SAFETY: The pin belongs to this initialized controller.
        unsafe { ioapic.set_table_entry(pin, entry) };
        Ok(())
    }

    /// Masks a GSI source.
    pub fn mask(&self, gsi: u32) {
        self.update_mask(gsi, true);
    }

    /// Unmasks a GSI source.
    pub fn unmask(&self, gsi: u32) {
        self.update_mask(gsi, false);
    }

    fn update_mask(&self, gsi: u32, masked: bool) {
        let Some((id, pin)) = self.topology.locate_gsi(gsi) else {
            return;
        };
        let controller = self
            .controllers
            .iter()
            .find(|controller| controller.config.id == id)
            .expect("IOAPIC topology and hardware state diverged");
        let mut ioapic = controller.ioapic.lock();
        let pin = u8::try_from(pin).expect("IOAPIC pin count exceeds the register width");
        // SAFETY: The pin belongs to this initialized controller.
        let mut entry = unsafe { ioapic.table_entry(pin) };
        let mut flags = entry.flags();
        flags.set(IrqFlags::MASKED, masked);
        entry.set_flags(flags);
        // SAFETY: The pin belongs to this initialized controller.
        unsafe { ioapic.set_table_entry(pin, entry) };
        // Read back to complete the IOAPIC MMIO write before returning.
        // SAFETY: The pin belongs to this initialized controller.
        _ = unsafe { ioapic.table_entry(pin) };
    }
}

#[cfg(test)]
mod tests {
    use memory_addr::pa;

    use super::{IoApicConfig, IoApicTopology};

    #[test]
    fn locates_a_gsi_in_the_controller_range() {
        let set = IoApicTopology::new(alloc::vec![IoApicConfig {
            id: 3,
            physical_base: pa!(0xfec0_0000),
            gsi_base: 24,
            pin_count: 24,
        }])
        .unwrap();

        assert_eq!(set.locate_gsi(30), Some((3, 6)));
        assert_eq!(set.locate_gsi(23), None);
        assert_eq!(set.locate_gsi(48), None);
    }
}
