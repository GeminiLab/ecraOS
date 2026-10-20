//! Application and loader image construction.
//!
//! Builds the independent application and loader Cargo graphs with their respective relocation
//! settings and obtains their artifact directories from Cargo metadata.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
};

use cargo_metadata::{Metadata, MetadataCommand};

use crate::{
    TaskResult,
    cli::{BuildOptions, Profile, Target},
    process,
};

/// The built loader images.
///
/// x86-64 and RISC-V use the ELF image, while AArch64 uses the flat binary image.
pub struct Images {
    /// The loader's ELF artifact.
    ///
    /// Also returned by the public build command.
    pub loader: PathBuf,
    /// The stripped loader binary.
    ///
    /// Used by AArch64's Linux-image boot protocol.
    pub loader_binary: PathBuf,
}

/// Reads Cargo metadata without resolving dependencies.
///
/// Uses the repository root as the working directory so target-directory overrides match builds.
fn metadata(root: &Path, manifest: &Path) -> TaskResult<Metadata> {
    let mut command = MetadataCommand::new();
    command.current_dir(root).manifest_path(manifest).no_deps();
    if let Some(cargo) = std::env::var_os("CARGO") {
        command.cargo_path(cargo);
    }
    Ok(command.exec()?)
}

/// Selects the selected package's only binary target.
///
/// Other packages' binaries do not participate in the application target selection.
fn binary_name<'a>(metadata: &'a Metadata, manifest: &Path) -> TaskResult<&'a str> {
    let package = metadata
        .packages
        .iter()
        .find(|package| package.manifest_path.as_std_path() == manifest)
        .ok_or_else(|| format!("package not found for manifest {}", manifest.display()))?;
    let names: Vec<_> = package
        .targets
        .iter()
        .filter(|target| target.is_bin())
        .collect();
    match names.as_slice() {
        [target] => Ok(&target.name),
        _ => Err(format!(
            "crate must define exactly one binary target: {}",
            manifest.display()
        )
        .into()),
    }
}

/// Prepares a locked architecture-specific Cargo build.
///
/// Non-x86 architectures rebuild the standard libraries to match their relocation settings.
fn build_command(root: &Path, manifest: &Path, options: &BuildOptions) -> Command {
    let mut command = process::cargo(root);
    command
        .args(["build", "--locked", "--manifest-path"])
        .arg(manifest)
        .args(["--target", options.target.triple()]);
    if options.profile == Profile::Release {
        command.args(["--profile", "release"]);
    }
    if options.target != Target::X86_64 {
        command.args(["-Z", "build-std=core,alloc,compiler_builtins"]);
    }
    command
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    command
}

/// Encodes compiler options without shell splitting.
///
/// Cargo's unit-separator format preserves linker paths containing whitespace.
fn encoded_flags(flags: &[OsString]) -> OsString {
    let mut encoded = OsString::new();
    for (index, flag) in flags.iter().enumerate() {
        if index > 0 {
            encoded.push("\x1f");
        }
        encoded.push(flag);
    }
    encoded
}

/// Strips an ELF image to a flat binary.
///
/// Uses the rust-objcopy tool supplied by cargo-binutils.
fn strip(root: &Path, input: &Path, output: &Path, log: Option<&File>) -> TaskResult<()> {
    let mut command = Command::new("rust-objcopy");
    command
        .current_dir(root)
        .arg(input)
        .args(["--strip-all", "-O", "binary"])
        .arg(output);
    process::run(&mut command, log)
}

/// Removes artifacts for the selected guest architecture.
///
/// Retains sibling guest targets, host caches, and QEMU logs even when both Cargo graphs share
/// their artifact root. Missing directories are already clean, while other errors are reported.
fn clean_artifacts(kernel_target: &Path, loader_target: &Path, target: Target) -> TaskResult<()> {
    let directories = [
        kernel_target.join(target.triple()),
        loader_target.join(target.triple()),
    ];
    for (index, directory) in directories.iter().enumerate() {
        if index > 0 && directory == &directories[0] {
            continue;
        }
        match fs::remove_dir_all(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("clean artifacts {}: {error}", directory.display()).into());
            }
        }
    }
    Ok(())
}

/// Builds the selected application and architecture loader.
///
/// The two manifests retain separate target directories, including custom Cargo directory settings.
pub fn image(root: &Path, options: &BuildOptions, log: Option<&File>) -> TaskResult<Images> {
    let manifest = root
        .join(&options.crate_path)
        .join("Cargo.toml")
        .canonicalize()
        .map_err(|error| {
            format!(
                "application manifest {}: {error}",
                options.crate_path.display()
            )
        })?;
    let kernel_metadata = metadata(root, &manifest)?;
    let name = binary_name(&kernel_metadata, &manifest)?;
    let loader_manifest = root.join("ecraldr/Cargo.toml");
    let loader_metadata = metadata(root, &loader_manifest)?;
    if options.clean {
        clean_artifacts(
            kernel_metadata.target_directory.as_std_path(),
            loader_metadata.target_directory.as_std_path(),
            options.target,
        )?;
    }
    let artifact_suffix = Path::new(options.target.triple()).join(options.profile.directory());
    let kernel_dir = kernel_metadata
        .target_directory
        .as_std_path()
        .join(&artifact_suffix);
    let loader_dir = loader_metadata
        .target_directory
        .as_std_path()
        .join(&artifact_suffix);
    let kernel = kernel_dir.join(name);
    let kernel_binary = kernel_dir.join(format!("{name}.bin"));
    let loader = loader_dir.join(options.target.loader());
    let loader_binary = loader_dir.join(format!("{}.bin", options.target.loader()));

    let mut linker = OsString::from("link-arg=-T");
    linker.push(root.join("ecraos/link.ld"));
    let flags = encoded_flags(&[
        "-C".into(),
        "relocation-model=pie".into(),
        "-C".into(),
        linker,
        "-C".into(),
        "link-arg=-pie".into(),
    ]);
    let mut command = build_command(root, &manifest, options);
    command.env("CARGO_ENCODED_RUSTFLAGS", flags);
    process::run(&mut command, log)?;
    strip(root, &kernel, &kernel_binary, log)?;

    let mut command = build_command(root, &loader_manifest, options);
    command
        .args(["-p", options.target.loader()])
        .env("KERNEL_BIN", &kernel_binary)
        .env(
            "CARGO_ENCODED_RUSTFLAGS",
            encoded_flags(&["-C".into(), "relocation-model=static".into()]),
        );
    process::run(&mut command, log)?;
    strip(root, &loader, &loader_binary, log)?;
    Ok(Images {
        loader,
        loader_binary,
    })
}
