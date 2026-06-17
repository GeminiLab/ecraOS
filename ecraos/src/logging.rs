use core::sync::atomic::{AtomicBool, Ordering};

use log::{Level, Log};

use crate::{kprintln, logging::colors::ColorForLevel};

static INITED: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
mod colors {
    pub const COLOR_BLACK: &str = "\u{1B}[30m";
    pub const COLOR_RED: &str = "\u{1B}[31m";
    pub const COLOR_GREEN: &str = "\u{1B}[32m";
    pub const COLOR_YELLOW: &str = "\u{1B}[33m";
    pub const COLOR_BLUE: &str = "\u{1B}[34m";
    pub const COLOR_MAGENTA: &str = "\u{1B}[35m";
    pub const COLOR_CYAN: &str = "\u{1B}[36m";
    pub const COLOR_WHITE: &str = "\u{1B}[37m";
    pub const COLOR_BRIGHT_BLACK: &str = "\u{1B}[90m";
    pub const COLOR_BRIGHT_RED: &str = "\u{1B}[91m";
    pub const COLOR_BRIGHT_GREEN: &str = "\u{1B}[92m";
    pub const COLOR_BRIGHT_YELLOW: &str = "\u{1B}[93m";
    pub const COLOR_BRIGHT_BLUE: &str = "\u{1B}[94m";
    pub const COLOR_BRIGHT_MAGENTA: &str = "\u{1B}[95m";
    pub const COLOR_BRIGHT_CYAN: &str = "\u{1B}[96m";
    pub const COLOR_BRIGHT_WHITE: &str = "\u{1B}[97m";

    pub const COLOR_RESET: &str = "\u{1B}[m";

    pub const COLOR_NOTHING: &str = "";

    pub struct ColorForLevel {
        pub level_str_color: &'static str,
        pub content_color: &'static str,
        pub reset_color: &'static str,
    }

    pub const COLOR_FOR_LEVEL_ERROR: ColorForLevel = ColorForLevel {
        level_str_color: COLOR_BRIGHT_RED,
        content_color: COLOR_RED,
        reset_color: COLOR_RESET,
    };
    pub const COLOR_FOR_LEVEL_WARN: ColorForLevel = ColorForLevel {
        level_str_color: COLOR_BRIGHT_YELLOW,
        content_color: COLOR_YELLOW,
        reset_color: COLOR_RESET,
    };
    pub const COLOR_FOR_LEVEL_INFO: ColorForLevel = ColorForLevel {
        level_str_color: COLOR_BRIGHT_GREEN,
        content_color: COLOR_GREEN,
        reset_color: COLOR_RESET,
    };
    pub const COLOR_FOR_LEVEL_DEBUG: ColorForLevel = ColorForLevel {
        level_str_color: COLOR_BRIGHT_CYAN,
        content_color: COLOR_CYAN,
        reset_color: COLOR_RESET,
    };
    pub const COLOR_FOR_LEVEL_TRACE: ColorForLevel = ColorForLevel {
        level_str_color: COLOR_BRIGHT_BLACK,
        content_color: COLOR_BRIGHT_BLACK,
        reset_color: COLOR_RESET,
    };
    pub const COLOR_FOR_LEVEL_NONE: ColorForLevel = ColorForLevel {
        level_str_color: COLOR_NOTHING,
        content_color: COLOR_NOTHING,
        reset_color: COLOR_NOTHING,
    };
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

const fn fixed_width_level(level: Level) -> &'static str {
    match level {
        Level::Error => "E",
        Level::Warn => "W",
        Level::Info => "I",
        Level::Debug => "D",
        Level::Trace => "T",
    }
}

struct Logger;

impl Log for Logger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let level = record.level();
        let args = record.args();
        let target = record.target();
        let line = record.line().unwrap_or(0);

        let mono_time = exarch::time::monotonic_time();
        let secs = mono_time.as_secs();
        let micros = mono_time.subsec_micros();

        let level_str = fixed_width_level(level);
        let ColorForLevel {
            level_str_color,
            content_color,
            reset_color,
        } = colors::color_for_level(level);

        kprintln!(
            "[{level_str_color}{level_str}{reset_color} {secs: >3}.{micros:06} {target}:{line}] {content_color}{args}{reset_color}"
        );
    }

    fn flush(&self) {}
}

pub fn init() {
    log::set_logger(&Logger).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    INITED.store(true, Ordering::Release);
}

pub fn is_inited() -> bool {
    INITED.load(Ordering::Acquire)
}
