//! Task command definitions.
//!
//! Validates scalar build options and expands comma-separated QEMU options into concrete runs.

use std::{
    ffi::OsString,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
};

use clap::{Args, Parser, Subcommand, ValueEnum};

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;

/// The repository task interface.
///
/// Each subcommand performs one of the build or test operations maintained by this repository.
#[derive(Debug, Parser)]
#[command(about = "Builds and tests ecraOS applications", version)]
pub struct Cli {
    /// The operation to execute.
    ///
    /// Matrix runs use comma-separated options on the qemu operation.
    #[command(subcommand)]
    pub task: Task,
}

/// The supported task operations.
///
/// Local scripts and CI use the same QEMU command for scalar and vector runs.
#[derive(Debug, Subcommand)]
pub enum Task {
    /// Builds and strips an application and its loader.
    ///
    /// Prints the loader ELF path on stdout after a successful build.
    Build(BuildOptions),
    /// Builds and runs applications in QEMU.
    ///
    /// Comma-separated options run their Cartesian product and summarize the outcomes.
    Qemu(QemuArgs),
    /// Runs the host test suites in both workspaces.
    ///
    /// Includes kernel library tests with the host-test feature.
    TestHost,
}

/// A supported guest architecture.
///
/// Each architecture selects its own loader binary and QEMU machine configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Target {
    /// The x86-64 bare-metal target.
    ///
    /// Boots through the Multiboot loader.
    #[value(name = "x86_64-unknown-none")]
    X86_64,
    /// The RISC-V 64 bare-metal target.
    ///
    /// Boots on the QEMU virt machine through OpenSBI.
    #[value(name = "riscv64gc-unknown-none-elf")]
    Riscv64,
    /// The AArch64 soft-float bare-metal target.
    ///
    /// Boots the flat loader image on the QEMU virt machine.
    #[value(name = "aarch64-unknown-none-softfloat")]
    Aarch64,
}

impl Target {
    /// Returns the Cargo target triple.
    ///
    /// Uses the same spelling accepted by the command-line interface.
    pub fn triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-none",
            Self::Riscv64 => "riscv64gc-unknown-none-elf",
            Self::Aarch64 => "aarch64-unknown-none-softfloat",
        }
    }

    /// Returns the architecture's loader package name.
    ///
    /// The loader is built from the independent ecraldr workspace.
    pub fn loader(self) -> &'static str {
        match self {
            Self::X86_64 => "ecraldr-x86_64-multiboot",
            Self::Riscv64 => "ecraldr-riscv64-none",
            Self::Aarch64 => "ecraldr-aarch64-linux",
        }
    }
}

/// A supported Cargo build profile.
///
/// Both profiles use the existing kernel and loader relocation settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    /// The development build profile.
    ///
    /// Artifacts are stored in Cargo's debug directory.
    Debug,
    /// The optimized build profile.
    ///
    /// Uses Cargo's release profile and artifact directory.
    Release,
}

impl Profile {
    /// Returns the profile's artifact directory name.
    ///
    /// Cargo stores development-profile artifacts under debug rather than dev.
    pub fn directory(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

/// Scalar options for one application build.
///
/// QEMU vector expansion creates one set of these options per concrete run.
#[derive(Args, Clone, Debug)]
pub struct BuildOptions {
    /// The application crate directory.
    ///
    /// Relative paths are resolved from the repository root.
    #[arg(long = "crate", env = "CRATE", default_value = "smoke-apps/default", value_parser = crate_path)]
    pub crate_path: PathBuf,
    /// The guest target triple.
    ///
    /// Selects the loader and architecture-specific build flags.
    #[arg(long, env = "TARGET", default_value = "x86_64-unknown-none")]
    pub target: Target,
    /// The application and loader build profile.
    ///
    /// Defaults to the development profile.
    #[arg(long, env = "PROFILE", default_value = "debug")]
    pub profile: Profile,
    /// The request to clean artifacts before building.
    ///
    /// Removes the selected guest target directory, retaining host caches and QEMU logs.
    #[arg(long, env = "CLEAN", value_parser = clap::builder::BoolishValueParser::new())]
    pub clean: bool,
}

/// Options for one or more QEMU runs.
///
/// Applications, targets, profiles, and CPU counts accept comma-separated vectors.
#[derive(Args, Debug)]
pub struct QemuArgs {
    /// The application directories to run.
    ///
    /// Accepts a scalar path or comma-separated paths.
    #[arg(long = "crate", env = "CRATE", default_value = "smoke-apps/default", value_delimiter = ',', value_parser = crate_path)]
    pub crates: Vec<PathBuf>,
    /// The guest architectures to run.
    ///
    /// Accepts scalar or comma-separated target triples.
    #[arg(
        long,
        env = "TARGET",
        default_value = "x86_64-unknown-none",
        value_delimiter = ','
    )]
    pub target: Vec<Target>,
    /// The build profiles to run.
    ///
    /// Accepts debug, release, or debug,release.
    #[arg(long, env = "PROFILE", default_value = "debug", value_delimiter = ',')]
    pub profile: Vec<Profile>,
    /// The guest CPU counts to run.
    ///
    /// Each count must be positive, for example 1,4.
    #[arg(long, env = "CPUS", default_value = "1", value_delimiter = ',')]
    pub cpus: Vec<NonZeroUsize>,
    /// The timeout for each guest in seconds.
    ///
    /// Build time does not count against the guest runtime budget.
    #[arg(long, env = "TIMEOUT_SEC", default_value = "15")]
    pub timeout: NonZeroU64,
    /// The request to clean artifacts before each build.
    ///
    /// Applies to all selected applications and architectures.
    #[arg(long, env = "CLEAN", value_parser = clap::builder::BoolishValueParser::new())]
    pub clean: bool,
    /// The additional arguments passed directly to QEMU.
    ///
    /// Repeat this option once per argument, including arguments that begin with a hyphen.
    #[arg(long = "qemu-arg", allow_hyphen_values = true)]
    pub qemu_args: Vec<OsString>,
}

/// One concrete QEMU invocation.
///
/// Contains scalar build options and a single positive CPU count.
#[derive(Clone, Debug)]
pub struct Cell {
    /// The application build configuration.
    ///
    /// Includes the selected architecture and profile.
    pub build: BuildOptions,
    /// The number of guest CPUs.
    ///
    /// Has already been validated by clap.
    pub cpus: NonZeroUsize,
}

impl QemuArgs {
    /// Expands the options into their Cartesian product.
    ///
    /// Keeps the user's ordering and executes CPU variants consecutively for each build.
    pub fn cells(&self) -> Vec<Cell> {
        let mut cells = Vec::new();
        for crate_path in &self.crates {
            for &target in &self.target {
                for &profile in &self.profile {
                    for &cpus in &self.cpus {
                        cells.push(Cell {
                            build: BuildOptions {
                                crate_path: crate_path.clone(),
                                target,
                                profile,
                                clean: self.clean,
                            },
                            cpus,
                        });
                    }
                }
            }
        }
        cells
    }
}

/// Validates an application path argument.
///
/// Empty vector entries are rejected before filesystem or build operations begin.
fn crate_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        Err("application path must not be empty".into())
    } else {
        Ok(PathBuf::from(value))
    }
}
