// SPDX-License-Identifier: Apache-2.0

//! Cross-platform process supervision, execution telemetry, and crash forensics.
//!
//! Classifies child process exit codes, Unix signals, and Windows NTSTATUS / SEH
//! exception codes into actionable studio diagnostics, and writes reproducible
//! crash bundles (`.json` forensics report + `.rxt` context snapshot).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::context::ResolvedContext;
use crate::errors::Result;
use crate::platform::Platform;

/// High-level classification of how a launched process terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationKind {
    /// Exited normally with code 0.
    Success,
    /// Exited normally with a regular non-zero application status code.
    NonZeroExit,
    /// Interrupted by user signal (`SIGINT`, `SIGHUP`, Windows `STATUS_CONTROL_C_EXIT`).
    Interrupted,
    /// Terminated externally (`SIGTERM`, `SIGKILL`, `DBG_TERMINATE_PROCESS`).
    Terminated,
    /// Dynamic linker / loader failure before `main` (`STATUS_DLL_NOT_FOUND`, `STATUS_ENTRYPOINT_NOT_FOUND`, etc.).
    LoaderFailure,
    /// Fatal hardware exception, memory fault, or abort (`SIGSEGV`, `SIGABRT`, `SIGBUS`, `STATUS_ACCESS_VIOLATION`, etc.).
    Crash,
}

impl TerminationKind {
    /// Returns true if this termination represents an abnormal crash or dynamic loader failure.
    pub fn is_crash_or_loader_failure(self) -> bool {
        matches!(self, Self::Crash | Self::LoaderFailure)
    }
}

/// Detailed cross-platform classification of a process exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitClassification {
    /// High-level termination category.
    pub kind: TerminationKind,
    /// Raw process exit code, if reported by the OS.
    pub raw_exit_code: Option<i32>,
    /// Unix signal number (either native OS waitstatus signal or 128+sig shell convention).
    pub signal: Option<i32>,
    /// Windows NTSTATUS / SEH hex string (e.g., `"0xC0000005"`), if applicable.
    pub ntstatus_hex: Option<String>,
    /// Symbolic identifier for the signal or NTSTATUS (e.g., `"SIGSEGV"`, `"STATUS_ACCESS_VIOLATION"`).
    pub code_name: String,
    /// Concise human-readable summary of the termination reason.
    pub summary: String,
    /// Probable root causes in a VFX/DCC studio environment.
    pub probable_causes: Vec<String>,
    /// Actionable remediation hints for developers or pipeline TDs.
    pub remediation_hints: Vec<String>,
}

impl ExitClassification {
    /// Returns true if the process crashed or failed in the dynamic loader.
    pub fn is_crash_or_loader_failure(&self) -> bool {
        self.kind.is_crash_or_loader_failure()
    }
}

/// Classify a `std::process::ExitStatus` on the current host platform.
pub fn classify_exit_status(status: &ExitStatus) -> ExitClassification {
    #[cfg(unix)]
    let native_signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let native_signal: Option<i32> = None;

    classify_exit_code(status.code(), native_signal, Platform::current())
}

/// Pure, cross-platform exit code and signal classifier (testable for any target `Platform`).
pub fn classify_exit_code(
    raw_code: Option<i32>,
    native_signal: Option<i32>,
    platform: Platform,
) -> ExitClassification {
    // 1. Direct Unix signal termination (process killed directly by signal without shell wrapper)
    if let Some(sig) = native_signal {
        return classify_unix_signal(sig, raw_code, platform);
    }

    let Some(code) = raw_code else {
        return ExitClassification {
            kind: TerminationKind::Terminated,
            raw_exit_code: None,
            signal: None,
            ntstatus_hex: None,
            code_name: "UNKNOWN_TERMINATION".to_string(),
            summary: "Process terminated without an exit code or signal".to_string(),
            probable_causes: vec![
                "Process was forcibly terminated by the operating system".to_string()
            ],
            remediation_hints: vec![
                "Check system kernel logs (dmesg / Event Viewer) for OOM or watchdog kills"
                    .to_string(),
            ],
        };
    };

    if code == 0 {
        return ExitClassification {
            kind: TerminationKind::Success,
            raw_exit_code: Some(0),
            signal: None,
            ntstatus_hex: None,
            code_name: "EXIT_SUCCESS".to_string(),
            summary: "Process completed successfully (exit code 0)".to_string(),
            probable_causes: Vec::new(),
            remediation_hints: Vec::new(),
        };
    }

    // 2. Windows NTSTATUS / SEH exception codes (32-bit unsigned values >= 0x80000000 or 0x40010004)
    let unsigned = code as u32;
    if platform == Platform::Windows
        || (unsigned >= 0x8000_0000 && matches!(unsigned >> 28, 0x8 | 0xC))
        || unsigned == 0x4001_0004
    {
        if let Some(win_class) = classify_windows_ntstatus(code, unsigned) {
            return win_class;
        }
    }

    // 3. Unix shell 128 + signal convention (129..=165) on Linux / macOS
    if platform != Platform::Windows && (129..=165).contains(&code) {
        let sig = code - 128;
        return classify_unix_signal(sig, Some(code), platform);
    }

    // 4. Standard non-zero exit code
    ExitClassification {
        kind: TerminationKind::NonZeroExit,
        raw_exit_code: Some(code),
        signal: None,
        ntstatus_hex: None,
        code_name: format!("EXIT_CODE_{code}"),
        summary: format!("Process exited with non-zero status code {code}"),
        probable_causes: vec![
            "Application reported an error or unhandled Python/CLI exception".to_string(),
        ],
        remediation_hints: vec![
            "Inspect command stderr output and verify package versions in the resolved context"
                .to_string(),
        ],
    }
}

fn classify_windows_ntstatus(raw_code: i32, unsigned: u32) -> Option<ExitClassification> {
    let hex = format!("0x{unsigned:08X}");
    let (kind, name, summary, causes, hints) = match unsigned {
        0xC000_0005 => (
            TerminationKind::Crash,
            "STATUS_ACCESS_VIOLATION",
            "Segmentation fault / memory access violation (read or write to invalid address)",
            vec![
                "Null or dangling pointer dereference in native C/C++ plugin or DCC".to_string(),
                "ABI incompatibility between resolved C++/Python extension packages".to_string(),
                "GPU driver or viewport shader crash".to_string(),
            ],
            vec![
                "Verify all C++ plugins in the resolved context were built against matching SDK versions"
                    .to_string(),
                "Reproduce using the generated .rxt snapshot under a debugger (cdb / WinDbg / VS)"
                    .to_string(),
            ],
        ),
        0xC000_0409 => (
            TerminationKind::Crash,
            "STATUS_STACK_BUFFER_OVERRUN",
            "Stack buffer overrun or security cookie / abort() fast-fail termination",
            vec![
                "C runtime abort() or std::terminate() called due to unhandled C++ exception or assertion"
                    .to_string(),
                "Stack buffer corruption detected by MSVC /GS security check".to_string(),
            ],
            vec![
                "Check stderr for assertion failure messages or fatal log output prior to abort"
                    .to_string(),
            ],
        ),
        0xC000_00FD => (
            TerminationKind::Crash,
            "STATUS_STACK_OVERFLOW",
            "Thread stack overflow",
            vec![
                "Infinite recursion in Python/C++ evaluation graph or scene callback".to_string(),
                "Excessive stack allocation in a native plugin".to_string(),
            ],
            vec![
                "Inspect scene evaluation graph or Python recursion depth".to_string(),
            ],
        ),
        0xC000_0374 => (
            TerminationKind::Crash,
            "STATUS_HEAP_CORRUPTION",
            "Windows heap manager detected heap metadata corruption",
            vec![
                "Double-free, use-after-free, or out-of-bounds heap write".to_string(),
                "Cross-CRT allocation mismatch (allocating in one MSVC runtime / debug CRT and freeing in another)"
                    .to_string(),
            ],
            vec![
                "Ensure all resolved native libraries share the same MSVC runtime (MD vs MDd)"
                    .to_string(),
                "Run with PageHeap (gflags /p /enable) using the saved .rxt context".to_string(),
            ],
        ),
        0xC000_0135 => (
            TerminationKind::LoaderFailure,
            "STATUS_DLL_NOT_FOUND",
            "Dynamic library (.dll) required by the executable could not be found",
            vec![
                "A required runtime package did not prepend its bin/ or lib/ directory to PATH"
                    .to_string(),
                "Missing transitive runtime dependency in package.py requires".to_string(),
            ],
            vec![
                "Inspect PATH in the crash report and run `dumpbin /dependents` on the target binary"
                    .to_string(),
                "Use `rez env --inspect-env` to check PATH and sanitized host variables".to_string(),
            ],
        ),
        0xC000_0139 => (
            TerminationKind::LoaderFailure,
            "STATUS_ENTRYPOINT_NOT_FOUND",
            "Required function/symbol export was not found in a loaded DLL",
            vec![
                "An older or incompatible DLL version appeared earlier on PATH than expected"
                    .to_string(),
                "ABI mismatch between executable and resolved shared library version".to_string(),
            ],
            vec![
                "Check PATH ordering in the crash forensics report for shadowing DLLs".to_string(),
                "Run `rez env --clean-room` to prevent host PATH pollution".to_string(),
            ],
        ),
        0xC000_007B => (
            TerminationKind::LoaderFailure,
            "STATUS_INVALID_IMAGE_FORMAT",
            "Invalid PE image format (typically 32-bit vs 64-bit DLL architecture mismatch)",
            vec![
                "A 64-bit process attempted to load a 32-bit DLL (or vice versa) from PATH"
                    .to_string(),
            ],
            vec![
                "Verify arch requirements (arch-AMD64) across all resolved packages".to_string(),
            ],
        ),
        0xC000_0142 => (
            TerminationKind::LoaderFailure,
            "STATUS_DLL_INIT_FAILED",
            "DLL initialization routine (DllMain) failed or static C++ initializer threw",
            vec![
                "A loaded DLL failed during DllMain(DLL_PROCESS_ATTACH)".to_string(),
                "License check, GPU init, or global static constructor failed on startup".to_string(),
            ],
            vec![
                "Enable loader snaps (gflags +sls) with the saved .rxt context to identify the failing DLL"
                    .to_string(),
            ],
        ),
        0xC000_001D => (
            TerminationKind::Crash,
            "STATUS_ILLEGAL_INSTRUCTION",
            "CPU executed an illegal or unsupported instruction",
            vec![
                "Binary was compiled with AVX2 / AVX-512 instructions unsupported by the current CPU"
                    .to_string(),
                "Jump to corrupted function pointer or vtable".to_string(),
            ],
            vec![
                "Verify CPU microarchitecture flags used when building the package".to_string(),
            ],
        ),
        0xC000_0094 => (
            TerminationKind::Crash,
            "STATUS_INTEGER_DIVIDE_BY_ZERO",
            "Hardware integer division by zero exception",
            vec!["Unchecked integer division by zero in native code".to_string()],
            vec!["Reproduce with the saved .rxt context under a debugger".to_string()],
        ),
        0xC000_0008 => (
            TerminationKind::Crash,
            "STATUS_INVALID_HANDLE",
            "An invalid OS handle was passed to a system call",
            vec!["Double-close of a Win32 HANDLE or mismatched handle ownership".to_string()],
            vec!["Inspect native plugin handle lifecycle".to_string()],
        ),
        0xC000_041D => (
            TerminationKind::Crash,
            "STATUS_FATAL_USER_CALLBACK_EXCEPTION",
            "Unhandled exception inside a Win32 user/GUI callback",
            vec![
                "Exception thrown across a Win32 window procedure (WndProc) boundary in Qt/UI"
                    .to_string(),
            ],
            vec!["Check UI / Qt plugin callbacks and Python event filters".to_string()],
        ),
        0x8000_0003 => (
            TerminationKind::Crash,
            "STATUS_BREAKPOINT",
            "Unhandled debug breakpoint (__debugbreak / int 3) encountered without debugger",
            vec!["Release binary hit a hard coded __debugbreak() or fatal check".to_string()],
            vec!["Attach a debugger and re-run with the saved .rxt context".to_string()],
        ),
        0xC000_013A => (
            TerminationKind::Interrupted,
            "STATUS_CONTROL_C_EXIT",
            "Process was interrupted by Ctrl+C or Ctrl+Break",
            vec!["User or parent supervisor sent Ctrl+C".to_string()],
            Vec::new(),
        ),
        0x4001_0004 => (
            TerminationKind::Terminated,
            "DBG_TERMINATE_PROCESS",
            "Process was terminated by debugger or external supervisor",
            vec!["External process termination requested".to_string()],
            Vec::new(),
        ),
        _ => return None,
    };

    Some(ExitClassification {
        kind,
        raw_exit_code: Some(raw_code),
        signal: None,
        ntstatus_hex: Some(hex),
        code_name: name.to_string(),
        summary: summary.to_string(),
        probable_causes: causes,
        remediation_hints: hints,
    })
}

fn classify_unix_signal(sig: i32, raw_code: Option<i32>, platform: Platform) -> ExitClassification {
    let is_macos = platform == Platform::MacOS;
    let sigbus = if is_macos { 10 } else { 7 };
    let sigsys = if is_macos { 12 } else { 31 };

    let (kind, name, summary, causes, hints) = match sig {
        1 => (
            TerminationKind::Interrupted,
            "SIGHUP",
            "Terminal hangup signal (SIGHUP)",
            vec!["Controlling terminal or SSH session closed".to_string()],
            Vec::new(),
        ),
        2 => (
            TerminationKind::Interrupted,
            "SIGINT",
            "Interrupted by user (SIGINT / Ctrl+C)",
            vec!["User pressed Ctrl+C or supervisor sent SIGINT".to_string()],
            Vec::new(),
        ),
        3 => (
            TerminationKind::Crash,
            "SIGQUIT",
            "Quit signal with core dump (SIGQUIT)",
            vec!["User pressed Ctrl+\\ or watchdog requested a core dump".to_string()],
            vec!["Inspect generated core dump with gdb/lldb and the saved .rxt context".to_string()],
        ),
        4 => (
            TerminationKind::Crash,
            "SIGILL",
            "Illegal hardware instruction (SIGILL)",
            vec![
                "Binary uses CPU instructions (e.g. AVX2/AVX-512) unsupported on this host"
                    .to_string(),
                "Corrupted function pointer or vtable jump".to_string(),
            ],
            vec![
                "Check compiler -march/-mtune flags of resolved native packages".to_string(),
            ],
        ),
        5 => (
            TerminationKind::Crash,
            "SIGTRAP",
            "Trace or breakpoint trap (SIGTRAP)",
            vec!["Unhandled debug trap or GLib/Qt fatal log handler".to_string()],
            vec!["Check stderr logs right before the trap".to_string()],
        ),
        6 => (
            TerminationKind::Crash,
            "SIGABRT",
            "Process aborted (SIGABRT / abort())",
            vec![
                "Failed C/C++ assert(), std::terminate(), or unhandled C++ exception".to_string(),
                "glibc/malloc detected heap corruption or double-free".to_string(),
                "Dynamic linker (ld.so / dyld) aborted on missing symbol or library".to_string(),
            ],
            vec![
                "Check stderr for assertion text, C++ exception type, or dyld/ld.so errors"
                    .to_string(),
                "Run with `--clean-room` if host LD_LIBRARY_PATH / DYLD_LIBRARY_PATH leaked"
                    .to_string(),
            ],
        ),
        8 => (
            TerminationKind::Crash,
            "SIGFPE",
            "Floating-point or integer arithmetic exception (SIGFPE)",
            vec![
                "Integer division by zero or unmasked floating-point exception in native code"
                    .to_string(),
            ],
            vec!["Reproduce under gdb/lldb using the saved .rxt snapshot".to_string()],
        ),
        9 => (
            TerminationKind::Terminated,
            "SIGKILL",
            "Forcibly killed (SIGKILL / OS Out-Of-Memory Killer)",
            vec![
                "Linux OOM killer terminated the process due to memory exhaustion".to_string(),
                "Render farm scheduler or user sent `kill -9`".to_string(),
            ],
            vec![
                "Check `dmesg -T | grep -i oom` or cgroup memory limits".to_string(),
            ],
        ),
        11 => (
            TerminationKind::Crash,
            "SIGSEGV",
            "Segmentation fault (SIGSEGV — invalid memory reference)",
            vec![
                "Null or dangling pointer dereference in native C/C++ code or Python C-extension"
                    .to_string(),
                "ABI mismatch between resolved shared libraries (.so / .dylib)".to_string(),
            ],
            vec![
                "Check LD_LIBRARY_PATH / DYLD_LIBRARY_PATH in the crash report for conflicting libraries"
                    .to_string(),
                "Reproduce with `rez env -i <crash.rxt> -- gdb <cmd>`".to_string(),
            ],
        ),
        15 => (
            TerminationKind::Terminated,
            "SIGTERM",
            "Terminated by signal (SIGTERM)",
            vec!["Process manager, render queue, or user requested graceful termination".to_string()],
            Vec::new(),
        ),
        s if s == sigbus => (
            TerminationKind::Crash,
            "SIGBUS",
            "Bus error (SIGBUS — unaligned memory access or unmapped file page)",
            vec![
                "A memory-mapped (.so / .dylib / .pyc / USD crate) file on NFS/SMB was truncated or modified in-place while running"
                    .to_string(),
                "Unaligned memory access in native code".to_string(),
            ],
            vec![
                "Enable Rez package caching (`cache_packages_path`) to stage payloads on local disk instead of network mounts"
                    .to_string(),
            ],
        ),
        s if s == sigsys => (
            TerminationKind::Crash,
            "SIGSYS",
            "Bad system call (SIGSYS — seccomp or sandbox violation)",
            vec!["Process invoked a syscall blocked by container/sandbox policy".to_string()],
            vec!["Check container seccomp profile or macOS sandbox entitlements".to_string()],
        ),
        other => (
            TerminationKind::Terminated,
            "SIGNAL",
            "Terminated by Unix signal",
            vec![format!("Process received Unix signal {other}")],
            Vec::new(),
        ),
    };

    ExitClassification {
        kind,
        raw_exit_code: raw_code,
        signal: Some(sig),
        ntstatus_hex: None,
        code_name: if name == "SIGNAL" {
            format!("SIG_{sig}")
        } else {
            name.to_string()
        },
        summary: summary.to_string(),
        probable_causes: causes,
        remediation_hints: hints,
    }
}

/// Package summary entry recorded in a crash forensics report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashPackageEntry {
    pub name: String,
    pub version: String,
    pub variant_index: Option<usize>,
    pub root: Option<String>,
    pub repo_path: Option<String>,
}

/// Structured telemetry and crash forensics report for a supervised execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashForensicsReport {
    pub schema_version: u32,
    pub session_id: String,
    pub rez_version: String,
    pub user: String,
    pub host: String,
    pub platform: String,
    pub arch: String,
    pub os: String,
    pub shell: String,
    pub command: Option<String>,
    pub started_at_epoch_ms: u64,
    pub finished_at_epoch_ms: u64,
    pub duration_ms: u64,
    pub exit: ExitClassification,
    pub package_requests: Vec<String>,
    pub resolved_packages: Vec<CrashPackageEntry>,
    pub important_env: HashMap<String, String>,
    pub sanitized_host_leaks: Vec<String>,
    pub active_host_leaks: Vec<String>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub rxt_snapshot_path: Option<String>,
}

/// Important runtime environment keys captured in crash forensics reports.
const IMPORTANT_FORENSICS_ENV_KEYS: &[&str] = &[
    "PATH",
    "PYTHONPATH",
    "PYTHONHOME",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "QT_PLUGIN_PATH",
    "PXR_PLUGINPATH_NAME",
    "OCIO",
    "MAYA_PLUG_IN_PATH",
    "MAYA_MODULE_PATH",
    "HOUDINI_PATH",
    "NUKE_PATH",
    "REZ_USED_RESOLVE",
    "REZ_RXT_FILE",
];

impl CrashForensicsReport {
    /// Build a forensics report from a `ResolvedContext` and execution telemetry.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        ctx: &ResolvedContext,
        shell: &str,
        command: Option<&str>,
        started_at_epoch_ms: u64,
        finished_at_epoch_ms: u64,
        exit: ExitClassification,
        resolved_env: &HashMap<String, String>,
        sanitized_host_leaks: Vec<String>,
        active_host_leaks: Vec<String>,
        stdout_tail: Option<String>,
        stderr_tail: Option<String>,
    ) -> Self {
        let duration_ms = finished_at_epoch_ms.saturating_sub(started_at_epoch_ms);
        let pid = std::process::id();
        let session_id = format!("rez-{started_at_epoch_ms}-{pid}");

        let resolved_packages = ctx
            .resolved_packages()
            .unwrap_or(&[])
            .iter()
            .map(|p| CrashPackageEntry {
                name: p.name.clone(),
                version: p.version.to_string(),
                variant_index: p.variant_index,
                root: p.root.as_ref().map(|r| r.display().to_string()),
                repo_path: p.repo_path.as_ref().map(|r| r.display().to_string()),
            })
            .collect();

        let mut important_env = HashMap::new();
        for &target_key in IMPORTANT_FORENSICS_ENV_KEYS {
            if let Some((k, v)) = resolved_env
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(target_key))
            {
                important_env.insert(k.clone(), v.clone());
            }
        }

        Self {
            schema_version: 1,
            session_id,
            rez_version: ctx.rez_version.clone(),
            user: ctx.user.clone(),
            host: ctx.host.clone(),
            platform: ctx.platform_str.clone(),
            arch: ctx.arch.clone(),
            os: ctx.os_str.clone(),
            shell: shell.to_string(),
            command: command.map(str::to_owned),
            started_at_epoch_ms,
            finished_at_epoch_ms,
            duration_ms,
            exit,
            package_requests: ctx
                .package_requests()
                .iter()
                .map(ToString::to_string)
                .collect(),
            resolved_packages,
            important_env,
            sanitized_host_leaks,
            active_host_leaks,
            stdout_tail,
            stderr_tail,
            rxt_snapshot_path: None,
        }
    }

    /// Write both the `.json` crash forensics report and accompanying `.rxt` snapshot into `dir`.
    /// Returns `(json_report_path, rxt_snapshot_path)`.
    pub fn write_bundle(
        &mut self,
        ctx: &ResolvedContext,
        dir: &Path,
    ) -> Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(dir)?;
        let stem = &self.session_id;
        let rxt_path = dir.join(format!("{stem}.rxt"));
        let json_path = dir.join(format!("{stem}.json"));

        ctx.save(&rxt_path)?;
        self.rxt_snapshot_path = Some(rxt_path.display().to_string());

        let json_bytes = serde_json::to_string_pretty(self)?;
        crate::serialise::atomic_write(&json_path, json_bytes)?;

        Ok((json_path, rxt_path))
    }

    /// Format a concise human-readable crash diagnostics summary for terminal display.
    pub fn format_human_summary(
        &self,
        json_path: Option<&Path>,
        rxt_path: Option<&Path>,
    ) -> String {
        let mut lines = Vec::new();
        lines.push(
            "================================================================================"
                .to_string(),
        );
        lines.push(format!(
            "[rez-rs Crash Forensics] Process terminated abnormally: {}",
            self.exit.code_name
        ));
        lines.push(
            "================================================================================"
                .to_string(),
        );
        if let Some(ref cmd) = self.command {
            lines.push(format!("  Command:      {cmd}"));
        } else {
            lines.push(format!("  Shell:        {}", self.shell));
        }
        lines.push(format!(
            "  Status:       {} ({:?})",
            self.exit.summary, self.exit.kind
        ));
        if let Some(code) = self.exit.raw_exit_code {
            if let Some(ref hex) = self.exit.ntstatus_hex {
                lines.push(format!("  Exit Code:    {code} ({hex})"));
            } else {
                lines.push(format!("  Exit Code:    {code}"));
            }
        }
        if let Some(sig) = self.exit.signal {
            lines.push(format!("  Signal:       {sig} ({})", self.exit.code_name));
        }
        lines.push(format!("  Duration:     {} ms", self.duration_ms));

        if !self.exit.probable_causes.is_empty() {
            lines.push(String::new());
            lines.push("  Probable Causes:".to_string());
            for cause in &self.exit.probable_causes {
                lines.push(format!("    - {cause}"));
            }
        }

        if !self.active_host_leaks.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "  Warning — Unsanitized Host Environment Leaks Present: {}",
                self.active_host_leaks.join(", ")
            ));
        }

        if !self.exit.remediation_hints.is_empty() {
            lines.push(String::new());
            lines.push("  Remediation Hints:".to_string());
            for hint in &self.exit.remediation_hints {
                lines.push(format!("    * {hint}"));
            }
        }

        if json_path.is_some() || rxt_path.is_some() {
            lines.push(String::new());
            lines.push("  Forensics Artifacts:".to_string());
            if let Some(jp) = json_path {
                lines.push(format!("    Report JSON:  {}", jp.display()));
            }
            if let Some(rp) = rxt_path {
                lines.push(format!("    Repro RXT:    {}", rp.display()));
                lines.push(format!("    Reproduce:    rez env -i \"{}\"", rp.display()));
            }
        }
        lines.push(
            "================================================================================"
                .to_string(),
        );
        lines.join("\n")
    }
}

/// Return current epoch time in milliseconds.
pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Truncate captured output bytes to the last `max_lines` lines for crash telemetry.
pub fn tail_output_bytes(bytes: &[u8], max_lines: usize) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(max_lines);
    Some(lines[start..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_windows_ntstatus_classifications() {
        let av = classify_exit_code(Some(0xC000_0005u32 as i32), None, Platform::Windows);
        assert_eq!(av.kind, TerminationKind::Crash);
        assert_eq!(av.code_name, "STATUS_ACCESS_VIOLATION");
        assert_eq!(av.ntstatus_hex.as_deref(), Some("0xC0000005"));
        assert!(av.is_crash_or_loader_failure());

        let dll = classify_exit_code(Some(0xC000_0135u32 as i32), None, Platform::Windows);
        assert_eq!(dll.kind, TerminationKind::LoaderFailure);
        assert_eq!(dll.code_name, "STATUS_DLL_NOT_FOUND");
        assert!(dll.is_crash_or_loader_failure());

        let sym = classify_exit_code(Some(0xC000_0139u32 as i32), None, Platform::Windows);
        assert_eq!(sym.kind, TerminationKind::LoaderFailure);
        assert_eq!(sym.code_name, "STATUS_ENTRYPOINT_NOT_FOUND");

        let heap = classify_exit_code(Some(0xC000_0374u32 as i32), None, Platform::Windows);
        assert_eq!(heap.kind, TerminationKind::Crash);
        assert_eq!(heap.code_name, "STATUS_HEAP_CORRUPTION");

        let ctrl_c = classify_exit_code(Some(0xC000_013Au32 as i32), None, Platform::Windows);
        assert_eq!(ctrl_c.kind, TerminationKind::Interrupted);
        assert_eq!(ctrl_c.code_name, "STATUS_CONTROL_C_EXIT");
        assert!(!ctrl_c.is_crash_or_loader_failure());
    }

    #[test]
    fn test_unix_signals_and_shell_128_convention() {
        // Direct signal 11 (SIGSEGV)
        let segv = classify_exit_code(None, Some(11), Platform::Linux);
        assert_eq!(segv.kind, TerminationKind::Crash);
        assert_eq!(segv.code_name, "SIGSEGV");
        assert_eq!(segv.signal, Some(11));
        assert!(segv.is_crash_or_loader_failure());

        // Shell 128 + 6 = 134 (SIGABRT)
        let abrt = classify_exit_code(Some(134), None, Platform::Linux);
        assert_eq!(abrt.kind, TerminationKind::Crash);
        assert_eq!(abrt.code_name, "SIGABRT");
        assert_eq!(abrt.signal, Some(6));

        // SIGBUS on Linux (7 -> 135) vs macOS (10 -> 138)
        let bus_linux = classify_exit_code(Some(135), None, Platform::Linux);
        assert_eq!(bus_linux.code_name, "SIGBUS");
        let bus_macos = classify_exit_code(Some(138), None, Platform::MacOS);
        assert_eq!(bus_macos.code_name, "SIGBUS");

        // SIGINT (130) is Interrupted, not Crash
        let sigint = classify_exit_code(Some(130), None, Platform::Linux);
        assert_eq!(sigint.kind, TerminationKind::Interrupted);
        assert_eq!(sigint.code_name, "SIGINT");
        assert!(!sigint.is_crash_or_loader_failure());
    }

    #[test]
    fn test_normal_and_nonzero_exit_codes() {
        let ok = classify_exit_code(Some(0), None, Platform::Linux);
        assert_eq!(ok.kind, TerminationKind::Success);
        assert!(!ok.is_crash_or_loader_failure());

        let err = classify_exit_code(Some(2), None, Platform::Windows);
        assert_eq!(err.kind, TerminationKind::NonZeroExit);
        assert_eq!(err.raw_exit_code, Some(2));
        assert!(!err.is_crash_or_loader_failure());
    }
}
