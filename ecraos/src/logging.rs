//! Kernel logging subsystem.
//!
//! # Backend
//!
//! The only backend is currently the platform-provided debug console. More backends and backend
//! selection/configuration are planned.
//!
//! # Usage
//!
//! Use the [`log`] crate macros, such as [`log::info!`] and [`log::error!`], to log messages.
//! The kernel calls [`init`] during boot.
//!
//! The default maximum level is Debug and cannot be changed currently, so Trace messages are
//! disabled. It's planned to allow configuration of the maximum level in the future.
//!
//! # Output format
//!
//! ```text
//! [I   1.234567 0:2 ecraos::mem:123] message
//! ```
//!
//! The prefix contains the level, monotonic time in seconds with six fractional digits, logical
//! CPU ID, task ID, target, and source line. Levels use `E`, `W`, `I`, `D`, and `T` for Error,
//! Warn, Info, Debug, and Trace. The task ID is zero before scheduler initialization, and a
//! missing source line is shown as zero. The target defaults to the module path and can be set by
//! the log macro. The level and message use ANSI colors for their severity.

use core::sync::atomic::{AtomicBool, Ordering};

use exspin::SpinNoIrq;
use log::{Level, Log};

use crate::{kprintln, logging::colors::ColorForLevel};

/// Whether the logging subsystem has been initialized.
static INITED: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
mod colors {
    //! ANSI terminal colors for log levels.

    /// Black foreground.
    pub const COLOR_BLACK: &str = "\u{1B}[30m";
    /// Red foreground.
    pub const COLOR_RED: &str = "\u{1B}[31m";
    /// Green foreground.
    pub const COLOR_GREEN: &str = "\u{1B}[32m";
    /// Yellow foreground.
    pub const COLOR_YELLOW: &str = "\u{1B}[33m";
    /// Blue foreground.
    pub const COLOR_BLUE: &str = "\u{1B}[34m";
    /// Magenta foreground.
    pub const COLOR_MAGENTA: &str = "\u{1B}[35m";
    /// Cyan foreground.
    pub const COLOR_CYAN: &str = "\u{1B}[36m";
    /// White foreground.
    pub const COLOR_WHITE: &str = "\u{1B}[37m";
    /// Bright black foreground.
    pub const COLOR_BRIGHT_BLACK: &str = "\u{1B}[90m";
    /// Bright red foreground.
    pub const COLOR_BRIGHT_RED: &str = "\u{1B}[91m";
    /// Bright green foreground.
    pub const COLOR_BRIGHT_GREEN: &str = "\u{1B}[92m";
    /// Bright yellow foreground.
    pub const COLOR_BRIGHT_YELLOW: &str = "\u{1B}[93m";
    /// Bright blue foreground.
    pub const COLOR_BRIGHT_BLUE: &str = "\u{1B}[94m";
    /// Bright magenta foreground.
    pub const COLOR_BRIGHT_MAGENTA: &str = "\u{1B}[95m";
    /// Bright cyan foreground.
    pub const COLOR_BRIGHT_CYAN: &str = "\u{1B}[96m";
    /// Bright white foreground.
    pub const COLOR_BRIGHT_WHITE: &str = "\u{1B}[97m";

    /// Reset sequence for terminal text attributes.
    pub const COLOR_RESET: &str = "\u{1B}[m";

    /// Empty sequence for uncolored output.
    pub const COLOR_NOTHING: &str = "";

    /// Colors for a log record's level and message.
    pub struct ColorForLevel {
        /// Foreground color of the level abbreviation letter.
        pub level_abbr_color: &'static str,
        /// Foreground color of the message.
        pub content_color: &'static str,
        /// Sequence restoring terminal text attributes.
        pub reset_color: &'static str,
    }

    /// Error log colors.
    pub const COLOR_FOR_LEVEL_ERROR: ColorForLevel = ColorForLevel {
        level_abbr_color: COLOR_BRIGHT_RED,
        content_color: COLOR_RED,
        reset_color: COLOR_RESET,
    };
    /// Warning log colors.
    pub const COLOR_FOR_LEVEL_WARN: ColorForLevel = ColorForLevel {
        level_abbr_color: COLOR_BRIGHT_YELLOW,
        content_color: COLOR_YELLOW,
        reset_color: COLOR_RESET,
    };
    /// Informational log colors.
    pub const COLOR_FOR_LEVEL_INFO: ColorForLevel = ColorForLevel {
        level_abbr_color: COLOR_BRIGHT_GREEN,
        content_color: COLOR_GREEN,
        reset_color: COLOR_RESET,
    };
    /// Debug log colors.
    pub const COLOR_FOR_LEVEL_DEBUG: ColorForLevel = ColorForLevel {
        level_abbr_color: COLOR_BRIGHT_CYAN,
        content_color: COLOR_CYAN,
        reset_color: COLOR_RESET,
    };
    /// Trace log colors.
    pub const COLOR_FOR_LEVEL_TRACE: ColorForLevel = ColorForLevel {
        level_abbr_color: COLOR_BRIGHT_BLACK,
        content_color: COLOR_BRIGHT_BLACK,
        reset_color: COLOR_RESET,
    };
    /// Color settings that emit no terminal sequences.
    pub const COLOR_FOR_LEVEL_NONE: ColorForLevel = ColorForLevel {
        level_abbr_color: COLOR_NOTHING,
        content_color: COLOR_NOTHING,
        reset_color: COLOR_NOTHING,
    };
    /// Returns the colors for a log level.
    pub const fn color_for_level(level: log::Level) -> ColorForLevel {
        match level {
            log::Level::Error => COLOR_FOR_LEVEL_ERROR,
            log::Level::Warn => COLOR_FOR_LEVEL_WARN,
            log::Level::Info => COLOR_FOR_LEVEL_INFO,
            log::Level::Debug => COLOR_FOR_LEVEL_DEBUG,
            log::Level::Trace => COLOR_FOR_LEVEL_TRACE,
        }
    }
}

/// Returns the single-letter abbreviation for a log level.
const fn level_abbr(level: Level) -> &'static str {
    match level {
        Level::Error => "E",
        Level::Warn => "W",
        Level::Info => "I",
        Level::Debug => "D",
        Level::Trace => "T",
    }
}

/// The debug console logger.
struct Logger;

/// Lock serializing log output with local interrupts and preemption disabled.
static LOGGER_LOCK: SpinNoIrq<()> = SpinNoIrq::new(());

/// Debug console logging.
impl Log for Logger {
    /// Accepts all levels and targets, leaving filtering to the `log` macros.
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    /// Prints a log record while holding [`LOGGER_LOCK`].
    fn log(&self, record: &log::Record) {
        let lock = LOGGER_LOCK.lock();

        let level = record.level();
        let args = record.args();
        let target = record.target();
        let line = record.line().unwrap_or(0);

        let mono_time = exarch::time::monotonic_time();
        let secs = mono_time.as_secs();
        let micros = mono_time.subsec_micros();

        let cpu_id = crate::mp::current_cpu_id();
        let task_id = crate::task::current_task_id().map_or(0, |id| id.into_repr());

        let level_abbr = level_abbr(level);
        let ColorForLevel {
            level_abbr_color: level_str_color,
            content_color,
            reset_color,
        } = colors::color_for_level(level);

        kprintln!(
            "[{level_str_color}{level_abbr}{reset_color} {secs: >3}.{micros:06} {cpu_id}:{task_id} {target}:{line}] {content_color}{args}{reset_color}"
        );

        drop(lock);
    }

    /// Does nothing because the logger does not buffer records.
    fn flush(&self) {}
}

/// Initializes the logging subsystem.
///
/// Registers the debug console logger and sets the maximum log level to Debug.
///
/// # Panics
///
/// Panics if a logger has already been registered.
pub(crate) fn init() {
    log::set_logger(&Logger).unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    INITED.store(true, Ordering::Release);
}

/// Returns whether the logging subsystem has been initialized.
pub fn is_inited() -> bool {
    INITED.load(Ordering::Acquire)
}
