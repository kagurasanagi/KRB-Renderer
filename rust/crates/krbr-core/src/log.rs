//! 起動からの秒数つきのログ（`[   0.333] info  ...`）

use std::fmt::Arguments;
use std::sync::atomic::{AtomicU8, Ordering};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
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
/// 最近のログ（ビューアの起動中の画面に出す）
static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
const RECENT_MAX: usize = 200;

/// 最近のログの行（古い順、最大 n 行）
pub fn recent(n: usize) -> Vec<String> {
    let r = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    r.iter().skip(r.len().saturating_sub(n)).cloned().collect()
}

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
    let line = format!("[{t:8.3}] {name} {args}");
    eprintln!("{line}");
    let mut r = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    if r.len() >= RECENT_MAX {
        r.pop_front();
    }
    r.push_back(line);
}

#[macro_export]
macro_rules! debug { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Debug, format_args!($($arg)*)) }; }
#[macro_export]
macro_rules! info { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Info, format_args!($($arg)*)) }; }
#[macro_export]
macro_rules! warn { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Warn, format_args!($($arg)*)) }; }
#[macro_export]
macro_rules! error { ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Error, format_args!($($arg)*)) }; }
