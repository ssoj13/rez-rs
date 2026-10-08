//! Shared operating-system environment for platform bindings and clean shells.
//!
//! Package and application variables inherited from the parent enter a clean shell
//! only through the explicit parent-variable allowlist. Package commands can add
//! their own values afterward. Native identity lookups never launch subprocesses.

use crate::platform::Platform;
use std::collections::HashMap;

fn value<'a>(
    platform: Platform,
    parent: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    parent.get(name).map(String::as_str).or_else(|| {
        if platform == Platform::Windows {
            parent
                .iter()
                .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                .min_by(|(left, _), (right, _)| left.cmp(right))
                .map(|(_, value)| value.as_str())
        } else {
            None
        }
    })
}

fn nonempty<'a>(
    platform: Platform,
    parent: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    value(platform, parent, name).filter(|value| !value.is_empty())
}

// Paths must target the requested OS, including cross-platform tests.
fn windows_path(base: &str, relative: &str) -> String {
    let separator = if base.ends_with(['\\', '/']) {
        ""
    } else {
        "\\"
    };
    format!("{base}{separator}{relative}")
}

/// Return the platform binding's ordered executable search directories.
pub fn system_paths(platform: Platform, parent: &HashMap<String, String>) -> Vec<String> {
    if platform == Platform::Windows {
        let root = nonempty(platform, parent, "SystemRoot").unwrap_or(r"C:\WINDOWS");
        vec![
            windows_path(root, "system32"),
            root.to_owned(),
            windows_path(root, r"System32\Wbem"),
            windows_path(root, r"System32\WindowsPowerShell\v1.0"),
            windows_path(root, r"System32\OpenSSH"),
        ]
    } else {
        [
            "/usr/local/sbin",
            "/usr/local/bin",
            "/usr/sbin",
            "/usr/bin",
            "/sbin",
            "/bin",
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

/// Build the standard OS environment, excluding package/application variables.
///
/// Windows names are canonical uppercase, including explicitly inherited names.
/// Native identity fallback is used only for the host platform, so synthetic
/// environments for another platform never inherit the host's account paths.
pub fn platform_environ(
    platform: Platform,
    parent: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let windows = platform == Platform::Windows;
    let mut put = |name: &str, fallback: String| {
        let key = if windows {
            name.to_ascii_uppercase()
        } else {
            name.to_owned()
        };
        let selected = nonempty(platform, parent, name)
            .map(str::to_owned)
            .unwrap_or(fallback);
        result.insert(key, selected);
    };
    // PATH is deliberately rebuilt; inheriting it requires an explicit allowlist entry.
    let path = system_paths(platform, parent).join(if windows { ";" } else { ":" });
    if windows {
        let root = nonempty(platform, parent, "SystemRoot").unwrap_or(r"C:\WINDOWS");
        let drive = nonempty(platform, parent, "SystemDrive").unwrap_or_else(|| {
            if root.as_bytes().get(1) == Some(&b':') {
                &root[..2]
            } else {
                "C:"
            }
        });
        let program_files = nonempty(platform, parent, "ProgramFiles")
            .map(str::to_owned)
            .unwrap_or_else(|| format!(r"{drive}\Program Files"));
        let program_files86 = nonempty(platform, parent, "ProgramFiles(x86)")
            .map(str::to_owned)
            .unwrap_or_else(|| format!(r"{drive}\Program Files (x86)"));
        let program_w6432 = nonempty(platform, parent, "ProgramW6432")
            .map(str::to_owned)
            .unwrap_or_else(|| program_files.clone());
        let temp = nonempty(platform, parent, "TEMP")
            .or_else(|| nonempty(platform, parent, "TMP"))
            .map(str::to_owned)
            .unwrap_or_else(|| windows_path(root, "Temp"));
        for (name, fallback) in [
            ("SystemRoot", root.to_owned()),
            ("SystemDrive", drive.to_owned()),
            (
                "PATHEXT",
                ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC".to_owned(),
            ),
            ("ProgramData", format!(r"{drive}\ProgramData")),
            ("ProgramFiles", program_files.clone()),
            ("ProgramFiles(x86)", program_files86.clone()),
            ("ProgramW6432", program_w6432.clone()),
            (
                "NUMBER_OF_PROCESSORS",
                std::thread::available_parallelism()
                    .map(|count| count.get())
                    .unwrap_or(1)
                    .to_string(),
            ),
            (
                "PROCESSOR_ARCHITECTURE",
                // Correct the fork's hardcoded AMD64 fallback for native ARM/x86
                // builds. Windows uses process architecture for this variable.
                // A foreign synthetic Windows environment keeps the source default.
                if Platform::current() == Platform::Windows {
                    match std::env::consts::ARCH {
                        "x86_64" => "AMD64",
                        "x86" => "x86",
                        "aarch64" => "ARM64",
                        "arm" => "ARM",
                        arch => arch,
                    }
                } else {
                    "AMD64"
                }
                .to_owned(),
            ),
            (
                "CommonProgramFiles",
                windows_path(&program_files, "Common Files"),
            ),
            (
                "CommonProgramFiles(x86)",
                windows_path(&program_files86, "Common Files"),
            ),
            (
                "CommonProgramW6432",
                windows_path(&program_w6432, "Common Files"),
            ),
            (
                "DriverData",
                windows_path(root, r"System32\Drivers\DriverData"),
            ),
        ] {
            put(name, fallback);
        }
        // The platform binding derives these from SystemRoot rather than
        // inheriting potentially inconsistent host values. A clean-shell
        // allowlist may explicitly override them after this baseline is built.
        result.insert("WINDIR".to_owned(), root.to_owned());
        result.insert(
            "COMSPEC".to_owned(),
            windows_path(root, r"System32\cmd.exe"),
        );
        result.insert("OS".to_owned(), "Windows_NT".to_owned());
        result.insert("TEMP".to_owned(), temp.clone());
        result.insert("TMP".to_owned(), temp);
    } else {
        put("SHELL", "/bin/sh".to_owned());
        put("TMPDIR", "/tmp".to_owned());
    }
    result.insert("PATH".to_owned(), path);

    let user = nonempty(platform, parent, if windows { "USERNAME" } else { "USER" })
        .or_else(|| nonempty(platform, parent, if windows { "USER" } else { "LOGNAME" }))
        .map(str::to_owned)
        .or_else(|| {
            (platform == Platform::current())
                .then(|| whoami::username().ok())
                .flatten()
        });
    let home = nonempty(
        platform,
        parent,
        if windows { "USERPROFILE" } else { "HOME" },
    )
    .map(str::to_owned)
    .or_else(|| {
        (platform == Platform::current())
            .then(dirs::home_dir)
            .flatten()
            .and_then(|path| path.into_os_string().into_string().ok())
    })
    .or_else(|| {
        if windows {
            user.as_ref().map(|user| {
                let drive = nonempty(platform, parent, "HOMEDRIVE").unwrap_or("C:");
                format!(r"{drive}\Users\{user}")
            })
        } else {
            None
        }
    });
    if let Some(user) = user {
        for name in ["USER", "USERNAME", "LOGNAME"] {
            if !windows || name != "LOGNAME" {
                result.insert(
                    name.to_owned(),
                    nonempty(platform, parent, name).unwrap_or(&user).to_owned(),
                );
            }
        }
    }
    if let Some(home) = home {
        result.insert(
            if windows { "USERPROFILE" } else { "HOME" }.to_owned(),
            home,
        );
    }
    let hostname_key = if windows { "COMPUTERNAME" } else { "HOSTNAME" };
    let hostname = nonempty(platform, parent, hostname_key)
        .map(str::to_owned)
        .or_else(|| {
            (platform == Platform::current())
                .then(|| hostname::get().ok())
                .flatten()
                .and_then(|name| name.into_string().ok())
        });
    if let Some(hostname) = hostname {
        result.insert(hostname_key.to_owned(), hostname);
    }
    for name in [
        "REZ_CONFIG_FILE",
        "USERDOMAIN",
        "APPDATA",
        "LOCALAPPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
    ] {
        if windows || name == "REZ_CONFIG_FILE" {
            if let Some(value) = value(platform, parent, name) {
                result.insert(name.to_owned(), value.to_owned());
            }
        }
    }
    result
}

/// Return an OS baseline plus the explicitly allowed parent values.
///
/// Empty allowlisted values are meaningful and override the baseline. Windows
/// matching and output naming prevent aliases such as Path and PATH coexisting.
pub fn clean_environ(
    platform: Platform,
    parent: &HashMap<String, String>,
    parent_variables: &[String],
) -> HashMap<String, String> {
    let mut result = platform_environ(platform, parent);
    for name in parent_variables {
        if let Some(value) = value(platform, parent, name) {
            let key = if platform == Platform::Windows {
                name.to_ascii_uppercase()
            } else {
                name.clone()
            };
            result.insert(key, value.to_owned());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn windows_baseline_matches_platform_binding() {
        let parent = parent(&[
            ("SystemRoot", r"D:\Windows"),
            ("SystemDrive", "D:"),
            ("ProgramFiles", r"E:\Applications"),
            ("ProgramFiles(x86)", r"E:\Applications86"),
            ("ProgramW6432", r"E:\Applications64"),
            ("TMP", r"F:\Temp"),
            ("USERNAME", "alice"),
            ("USERPROFILE", r"D:\Users\alice"),
            ("COMPUTERNAME", "station"),
            ("NUMBER_OF_PROCESSORS", "12"),
        ]);
        let env = platform_environ(Platform::Windows, &parent);
        assert_eq!(
            system_paths(Platform::Windows, &parent),
            [
                r"D:\Windows\system32",
                r"D:\Windows",
                r"D:\Windows\System32\Wbem",
                r"D:\Windows\System32\WindowsPowerShell\v1.0",
                r"D:\Windows\System32\OpenSSH",
            ]
        );
        assert_eq!(
            env["PATH"],
            system_paths(Platform::Windows, &parent).join(";")
        );
        assert_eq!(env["SYSTEMROOT"], r"D:\Windows");
        assert_eq!(env["WINDIR"], r"D:\Windows");
        assert_eq!(env["COMSPEC"], r"D:\Windows\System32\cmd.exe");
        assert_eq!(env["TEMP"], r"F:\Temp");
        assert_eq!(env["TMP"], r"F:\Temp");
        let mut conflicting = parent.clone();
        conflicting.insert("TEMP".to_owned(), r"G:\Preferred".to_owned());
        let shared = platform_environ(Platform::Windows, &conflicting);
        assert_eq!(shared["TEMP"], r"G:\Preferred");
        assert_eq!(shared["TMP"], r"G:\Preferred");
        let explicit = clean_environ(Platform::Windows, &conflicting, &["TMP".to_owned()]);
        assert_eq!(explicit["TEMP"], r"G:\Preferred");
        assert_eq!(explicit["TMP"], r"F:\Temp");
        assert_eq!(env["PROGRAMDATA"], r"D:\ProgramData");
        assert_eq!(env["COMMONPROGRAMFILES"], r"E:\Applications\Common Files");
        assert_eq!(
            env["COMMONPROGRAMFILES(X86)"],
            r"E:\Applications86\Common Files"
        );
        assert_eq!(env["COMMONPROGRAMW6432"], r"E:\Applications64\Common Files");
        assert_eq!(env["DRIVERDATA"], r"D:\Windows\System32\Drivers\DriverData");
        assert_eq!(env["NUMBER_OF_PROCESSORS"], "12");
        assert_eq!(env["USER"], "alice");
        assert_eq!(env["USERPROFILE"], r"D:\Users\alice");
        assert_eq!(env["OS"], "Windows_NT");
        // Explicit parent architecture is retained, including ARM64.
        let mut arm = parent.clone();
        arm.insert("PROCESSOR_ARCHITECTURE".to_owned(), "ARM64".to_owned());
        assert_eq!(
            platform_environ(Platform::Windows, &arm)["PROCESSOR_ARCHITECTURE"],
            "ARM64"
        );
        assert!(env["PATHEXT"].ends_with(";.MSC"));
    }

    #[test]
    fn windows_fixed_fields_ignore_parent_until_explicitly_allowed() {
        let parent = parent(&[
            ("SystemRoot", r"D:\Windows"),
            ("windir", r"E:\WrongWindows"),
            ("ComSpec", r"E:\UnexpectedShell.exe"),
            ("OS", "Unexpected_OS"),
        ]);
        let baseline = clean_environ(Platform::Windows, &parent, &[]);
        assert_eq!(baseline["WINDIR"], r"D:\Windows");
        assert_eq!(baseline["COMSPEC"], r"D:\Windows\System32\cmd.exe");
        assert_eq!(baseline["OS"], "Windows_NT");
        let allowlist = ["windir", "ComSpec", "OS"].map(str::to_owned);
        let inherited = clean_environ(Platform::Windows, &parent, &allowlist);
        assert_eq!(inherited["WINDIR"], r"E:\WrongWindows");
        assert_eq!(inherited["COMSPEC"], r"E:\UnexpectedShell.exe");
        assert_eq!(inherited["OS"], "Unexpected_OS");
    }

    #[test]
    fn windows_path_join_preserves_existing_separator() {
        for root in [r"D:\Windows\", "D:/Windows/"] {
            let parent = parent(&[("SystemRoot", root), ("ProgramFiles", root)]);
            let env = platform_environ(Platform::Windows, &parent);
            assert_eq!(
                system_paths(Platform::Windows, &parent)[0],
                format!("{root}system32")
            );
            assert_eq!(env["COMSPEC"], format!(r"{root}System32\cmd.exe"));
            assert_eq!(env["COMMONPROGRAMFILES"], format!("{root}Common Files"));
            assert_eq!(env["TEMP"], format!("{root}Temp"));
        }
    }

    #[test]
    fn windows_home_requires_explicit_allowlist() {
        let parent = parent(&[("HOME", r"E:\Home"), ("USERNAME", "alice")]);
        assert!(!clean_environ(Platform::Windows, &parent, &[]).contains_key("HOME"));
        assert_eq!(
            clean_environ(Platform::Windows, &parent, &["HOME".to_owned()])["HOME"],
            r"E:\Home"
        );
    }

    #[test]
    fn unix_baseline_preserves_os_identity_on_linux_and_macos() {
        let parent = parent(&[
            ("HOME", "/users/alice"),
            ("USER", "alice"),
            ("LOGNAME", "login"),
            ("SHELL", "/bin/zsh"),
            ("TMPDIR", "/private/tmp"),
            ("HOSTNAME", "station"),
            ("REZ_CONFIG_FILE", "/config/rez.py"),
            ("PATH", "/polluted/bin"),
            ("PYTHONPATH", "/polluted/python"),
            ("CUSTOM_MACHINE", "private"),
        ]);
        for platform in [Platform::Linux, Platform::MacOS] {
            let env = clean_environ(platform, &parent, &[]);
            assert_eq!(
                system_paths(platform, &parent),
                [
                    "/usr/local/sbin",
                    "/usr/local/bin",
                    "/usr/sbin",
                    "/usr/bin",
                    "/sbin",
                    "/bin"
                ]
            );
            assert_eq!(env["PATH"], system_paths(platform, &parent).join(":"));
            for key in [
                "HOME",
                "USER",
                "LOGNAME",
                "SHELL",
                "TMPDIR",
                "HOSTNAME",
                "REZ_CONFIG_FILE",
            ] {
                assert_eq!(env[key], parent[key]);
            }
            assert!(!env.contains_key("PYTHONPATH"));
            assert!(!env.contains_key("CUSTOM_MACHINE"));
        }
    }

    #[test]
    fn allowlist_preserves_empty_values_and_can_override_path() {
        let parent = parent(&[
            ("USER", "alice"),
            ("HOME", "/users/alice"),
            ("HOSTNAME", "station"),
            ("PATH", ""),
            ("CUSTOM_MACHINE", "private"),
            ("CUSTOM", ""),
            ("PYTHONPATH", "polluted"),
        ]);
        let allowlist = ["PATH", "CUSTOM_MACHINE", "CUSTOM", "ABSENT"].map(str::to_owned);
        for platform in [Platform::Linux, Platform::MacOS, Platform::Windows] {
            let env = clean_environ(platform, &parent, &allowlist);
            assert_eq!(env["PATH"], "");
            assert_eq!(env["CUSTOM"], "");
            assert_eq!(env["CUSTOM_MACHINE"], "private");
            assert!(!env.contains_key("ABSENT"));
            assert!(!env.contains_key("PYTHONPATH"));
        }
    }

    #[test]
    fn windows_keys_are_case_insensitive_and_canonical() {
        let parent = parent(&[
            ("sYsTeMrOoT", r"D:\OS"),
            ("pAtH", r"E:\Tools"),
            ("uSeRnAmE", "alice"),
            ("uSeRpRoFiLe", r"D:\Users\alice"),
            ("cOmPuTeRnAmE", "station"),
            ("rez_config_file", r"E:\rez.py"),
            ("custom_machine", "private"),
            ("PythonPath", "polluted"),
        ]);
        let env = clean_environ(
            Platform::Windows,
            &parent,
            &["Path".to_owned(), "CUSTOM_MACHINE".to_owned()],
        );
        assert_eq!(env["PATH"], r"E:\Tools");
        assert_eq!(env["SYSTEMROOT"], r"D:\OS");
        assert_eq!(env["USERNAME"], "alice");
        assert_eq!(env["REZ_CONFIG_FILE"], r"E:\rez.py");
        assert_eq!(env["CUSTOM_MACHINE"], "private");
        assert!(!env.contains_key("PYTHONPATH"));
        assert!(env.keys().all(|key| *key == key.to_ascii_uppercase()));
        assert_eq!(
            env.keys()
                .filter(|key| key.eq_ignore_ascii_case("PATH"))
                .count(),
            1
        );
        // An exact canonical spelling wins, regardless of HashMap iteration order.
        let parent = super::tests::parent(&[("Path", "alias"), ("PATH", "canonical")]);
        assert_eq!(
            clean_environ(Platform::Windows, &parent, &["PATH".to_owned()])["PATH"],
            "canonical"
        );
    }

    #[test]
    fn baseline_fallbacks_do_not_copy_application_state() {
        let parent = parent(&[("PYTHONPATH", "polluted"), ("CUSTOM_MACHINE", "private")]);
        let windows = platform_environ(Platform::Windows, &parent);
        assert_eq!(windows["SYSTEMROOT"], r"C:\WINDOWS");
        assert_eq!(windows["COMSPEC"], r"C:\WINDOWS\System32\cmd.exe");
        assert_eq!(windows["PROGRAMFILES"], r"C:\Program Files");
        for platform in [Platform::Windows, Platform::Linux, Platform::MacOS] {
            let env = clean_environ(platform, &parent, &[]);
            assert!(!env.contains_key("PYTHONPATH"));
            assert!(!env.contains_key("CUSTOM_MACHINE"));
            if platform != Platform::Windows {
                assert_eq!(env["SHELL"], "/bin/sh");
                assert_eq!(env["TMPDIR"], "/tmp");
            }
        }
    }

    #[test]
    fn native_identity_is_available_without_parent_environment() {
        // This does not mutate process env, so it is safe alongside parallel tests.
        let platform = Platform::current();
        let env = clean_environ(platform, &HashMap::new(), &[]);
        if let Ok(user) = whoami::username() {
            assert_eq!(env["USERNAME"], user);
        }
        if let Some(home) =
            dirs::home_dir().and_then(|path| path.into_os_string().into_string().ok())
        {
            assert_eq!(
                env[if platform == Platform::Windows {
                    "USERPROFILE"
                } else {
                    "HOME"
                }],
                home
            );
        }
    }
}
