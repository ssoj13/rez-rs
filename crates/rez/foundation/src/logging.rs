// SPDX-License-Identifier: Apache-2.0
//! Logging for rez-rs, controlled by REZ_LOG_LEVEL/config log_level,
//! -v (INFO), -vv (DEBUG), -vvv (TRACE), and --log [file].

use chrono::Local;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::sync::{Mutex, RwLock};

/// Threshold for optional diagnostic logging.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Off = 0,
    Error = 1,
    Warning = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl LogLevel {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "OFF" => Some(Self::Off),
            "ERROR" => Some(Self::Error),
            "WARNING" => Some(Self::Warning),
            "INFO" => Some(Self::Info),
            "DEBUG" => Some(Self::Debug),
            "TRACE" => Some(Self::Trace),
            _ => None,
        }
    }

    pub fn from_verbosity(v: u8) -> Self {
        match v {
            0 => LogLevel::Off,
            1 => LogLevel::Info,
            2 => LogLevel::Debug,
            _ => LogLevel::Trace,
        }
    }
}

struct LoggerState {
    level: LogLevel,
    writer: Option<Mutex<Box<dyn Write + Send>>>,
}

static LOGGER: RwLock<Option<LoggerState>> = RwLock::new(None);

/// Initialize logging. Call early from main().
/// - verbosity: 0=off, 1=INFO, 2=DEBUG, 3+=TRACE
/// - log_file: None = stderr only; Some(None) = default name (rez.log); Some(Some(path)) = custom path
pub fn init(verbosity: u8, log_file: Option<Option<String>>) -> io::Result<()> {
    init_level(LogLevel::from_verbosity(verbosity), log_file)
}

/// Initialize logging with an explicit named threshold.
pub fn init_level(level: LogLevel, log_file: Option<Option<String>>) -> io::Result<()> {
    let writer: Option<Mutex<Box<dyn Write + Send>>> = match log_file {
        None => None,
        Some(None) => {
            let path = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "rez".into());
            let path = format!("{}.log", path);
            let f = OpenOptions::new().create(true).append(true).open(&path)?;
            eprintln!("Logging to {}", path);
            Some(Mutex::new(Box::new(f)))
        }
        Some(Some(path)) => {
            let path = if !path.ends_with(".log") {
                format!("{}.log", path)
            } else {
                path
            };
            let f = OpenOptions::new().create(true).append(true).open(&path)?;
            eprintln!("Logging to {}", path);
            Some(Mutex::new(Box::new(f)))
        }
    };
    let mut state = LOGGER.write().unwrap_or_else(|e| e.into_inner());
    *state = Some(LoggerState { level, writer });
    Ok(())
}

/// Update the threshold while keeping the configured log destination.
pub fn set_level(level: LogLevel) {
    let mut state = LOGGER.write().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = state.as_mut() {
        state.level = level;
    } else {
        *state = Some(LoggerState {
            level,
            writer: None,
        });
    }
}

/// Internal logging sink. Explicit config diagnostics can bypass verbosity with `forced`;
/// they still use the same stderr and optional file destination.
pub fn do_log(level: LogLevel, target: &str, args: std::fmt::Arguments, forced: bool) {
    let state = LOGGER.read().unwrap_or_else(|e| e.into_inner());
    if level != LogLevel::Off
        && (forced || state.as_ref().is_some_and(|state| level <= state.level))
    {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let line = format!("{} [{}] {} {}\n", ts, level_str(level), target, args);
        let _ = io::stderr().write_all(line.as_bytes());
        if let Some(writer) = state.as_ref().and_then(|state| state.writer.as_ref()) {
            if let Ok(mut writer) = writer.lock() {
                let _ = writer.write_all(line.as_bytes());
                let _ = writer.flush();
            }
        }
    }
}

fn level_str(l: LogLevel) -> &'static str {
    match l {
        LogLevel::Off => "OFF",
        LogLevel::Error => "ERROR",
        LogLevel::Warning => "WARNING",
        LogLevel::Info => "INFO",
        LogLevel::Debug => "DEBUG",
        LogLevel::Trace => "TRACE",
    }
}

/// Log at INFO level (important steps, new steps and results).
#[macro_export]
macro_rules! log_info {
    ($target:expr, $($arg:tt)*) => {
        $crate::logging::do_log($crate::logging::LogLevel::Info, $target, format_args!($($arg)*), false);
    };
}

/// Log at DEBUG level (technical details).
#[macro_export]
macro_rules! log_debug {
    ($target:expr, $($arg:tt)*) => {
        $crate::logging::do_log($crate::logging::LogLevel::Debug, $target, format_args!($($arg)*), false);
    };
}

/// Log at TRACE level (behaviours and finer details).
#[macro_export]
macro_rules! log_trace {
    ($target:expr, $($arg:tt)*) => {
        $crate::logging::do_log($crate::logging::LogLevel::Trace, $target, format_args!($($arg)*), false);
    };
}
