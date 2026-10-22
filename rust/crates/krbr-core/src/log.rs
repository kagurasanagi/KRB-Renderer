//! 起動からの秒数つきのログ（`[   0.333] info  ...`）

use std::fmt::Arguments;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);
static START: OnceLock<Instant> = OnceLock::new();

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 >= LEVEL.load(Ordering::Relaxed)
}

pub fn log(level: Level, args: Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    let name = match level {
        Level::Debug => "debug",
        Level::Info => "info ",
        Level::Warn => "warn ",
        Level::Error => "error",
    };
    eprintln!("[{t:8.3}] {name} {args}");
}

#[macro_export]
macro_rules! debug { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Debug, format_args!($($arg)*)) }; }
#[macro_export]
macro_rules! info { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Info, format_args!($($arg)*)) }; }
#[macro_export]
macro_rules! warn { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Warn, format_args!($($arg)*)) }; }
#[macro_export]
macro_rules! error { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Error, format_args!($($arg)*)) }; }
