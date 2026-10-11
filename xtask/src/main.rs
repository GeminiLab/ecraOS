//! ecraOS xtask runner.
//!
//! This binary is temporary and is expected to be rewritten into a separate
//! `cargo-ecraos` binary, which works independently of the ecraOS source tree.
//!
//! Exmaples for the expected workflow in the future:
//!
//! ```sh
//! cargo ecraos new-project exvisor/ --kernel-version 0.4.2
//! cd exvisor/
//! cargo ecraos build --target x86_64-unknown-none --no-loader
//! cargo ecraos qemu --target aarch64-unknown-none-softfloat --loader ecraldr-aarch64-linux
//! ```

use std::{path::Path, process::ExitCode};

use clap::Parser;

mod build;
mod cli;
mod host;
mod process;
mod qemu;

/// A fallible repository task result.
///
/// Retains the original command or I/O error for the public diagnostic.
type TaskResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Executes the selected repository task.
///
/// Resolves resource paths from the xtask crate rather than the caller's working directory.
fn execute(cli: cli::Cli) -> TaskResult<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    match cli.task {
        cli::Task::Build(options) => {
            println!("{}", build::image(root, &options, None)?.loader.display());
            Ok(0)
        }
        cli::Task::Qemu(args) => qemu::run(root, &args),
        cli::Task::TestHost => {
            host::test(root)?;
            Ok(0)
        }
    }
}

/// Parses and dispatches the repository task.
///
/// Command-line validation precedes all build and guest execution.
fn main() -> ExitCode {
    match execute(cli::Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}
