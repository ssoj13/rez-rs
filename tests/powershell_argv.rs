//! Actual PowerShell binder, stream, and wrapper contracts use the shared renderer.
#![cfg(windows)]
use rex::{wrapper::WrapperScript, ShellType};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn shells() -> Vec<PathBuf> {
    let mut shells = vec![PathBuf::from("powershell.exe")];
    let modern = PathBuf::from(
        std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
    )
    .join("PowerShell/7/pwsh.exe");
    if modern.is_file() {
        shells.push(modern);
    }
    shells
}

fn run(shell: &Path, command: &str) -> Output {
    let invocation = ShellType::PowerShell.command(&format!(
        "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); {command}"
    ));
    Command::new(shell)
        .args(invocation.get_args())
        .env("REZ_ARG_TEST", "expanded value")
        .output()
        .unwrap()
}

fn success(shell: &Path, command: &str) -> Output {
    let output = run(shell, command);
    assert!(
        output.status.success(),
        "{}: {}",
        shell.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn python(temp: &Path) -> PathBuf {
    let path = temp.join("rez-python.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_rez"), &path).unwrap();
    path
}

fn invocation(python: &Path, code: &str, values: &[String], expand: bool) -> String {
    let mut args = vec![
        python.to_string_lossy().into_owned(),
        "-c".into(),
        code.into(),
    ];
    args.extend_from_slice(values);
    ShellType::PowerShell.join_command(&args, expand, None)
}

fn corpus() -> Vec<String> {
    let mut values: Vec<_> = [
        "",
        "space value",
        "semi;colon",
        "inner\"quote",
        "back\\\"quote",
        "C:\\space path\\trailing\\\\",
        "first\nsecond",
        "'apostrophe'",
        "\u{60}backtick\u{60}",
        "--%",
        "a\\\n",
        "%REZ_ARG_TEST%",
        "snowman \u{2603}",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let chars = ['a', ' ', '\\', '"', ';', '\u{60}', '\'', '\n', '\t'];
    let mut state = 117u64;
    for _ in 0..180 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let len = (state % 25) as usize;
        let mut value = String::new();
        for _ in 0..len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            value.push(chars[(state % chars.len() as u64) as usize]);
        }
        values.push(value);
    }
    values
}

#[test]
fn native_argv_corpus_preserves_modes_expansion_aliases_and_preferences() {
    let temp = tempfile::tempdir().unwrap();
    let python = python(temp.path());
    let values = corpus();
    for shell in shells() {
        let modes: &[&str] = if shell.file_name().unwrap() == "pwsh.exe" {
            &["", "Legacy", "Standard", "Windows"]
        } else {
            &[""]
        };
        for mode in modes {
            for expand in [false, true] {
                for chunk in values.chunks(48) {
                    let mut args = vec![
                        "rez_python_probe".into(),
                        "-c".into(),
                        "import sys,json; print(json.dumps(sys.argv[1:]))".into(),
                    ];
                    args.extend_from_slice(chunk);
                    args.push(concat!("$", "{Env:REZ_ARG_TEST}").into());
                    args.push("$REZ_ARG_SCOPED".into());
                    let prefix = format!(
                        "Set-Alias rez_python_probe '{}'; $REZ_ARG_SCOPED='scoped value'; {} $before=$PSNativeCommandArgumentPassing;",
                        python.to_string_lossy().replace('\'', "''"),
                        if mode.is_empty() {
                            String::new()
                        } else {
                            format!("$PSNativeCommandArgumentPassing='{mode}';")
                        }
                    );
                    let command = format!(
                        "{prefix}{}; if($before -ne $PSNativeCommandArgumentPassing){{throw 'preference leaked'}}; exit $LASTEXITCODE",
                        ShellType::PowerShell.join_command(&args, expand, None)
                    );
                    let output = success(&shell, &command);
                    let mut expected = chunk.to_vec();
                    expected.push(if expand {
                        "expanded value".into()
                    } else {
                        concat!("$", "{Env:REZ_ARG_TEST}").into()
                    });
                    expected.push(if expand {
                        "scoped value".into()
                    } else {
                        "$REZ_ARG_SCOPED".into()
                    });
                    assert_eq!(
                        serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap(),
                        expected,
                        "shell={}, mode={mode}, expand={expand}",
                        shell.display()
                    );
                }
            }
        }
    }
}

#[test]
fn functions_and_expansion_keep_string_types_and_single_evaluation() {
    let temp = tempfile::tempdir().unwrap();
    let python = python(temp.path());
    let values = corpus();
    for shell in shells() {
        for expand in [false, true] {
            let mut args = vec!["rez_function_probe".into()];
            args.extend_from_slice(&values[..13]);
            let command = format!(
                "function rez_function_probe {{ foreach($a in $args){{if($a -isnot [string]){{throw 'type changed'}}}}; ConvertTo-Json -Compress -InputObject @($args) }}; {}",
                ShellType::PowerShell.join_command(&args, expand, None)
            );
            let output = success(&shell, &command);
            assert_eq!(
                serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap(),
                values[..13]
            );
        }
        let command = format!(
            "$global:counter=0; function CountValue {{ $global:counter++; 'once' }}; {}; if($counter -ne 1){{throw 'reevaluated'}}",
            invocation(
                &python,
                "import sys,json;print(json.dumps(sys.argv[1:]))",
                &["$(CountValue)".into()],
                true
            )
        );
        assert_eq!(
            serde_json::from_slice::<Vec<String>>(&success(&shell, &command).stdout).unwrap(),
            ["once"]
        );
    }
}

#[test]
fn native_streams_stdin_filters_exit_and_binary_redirection_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let python = python(temp.path());
    for shell in shells() {
        let command = format!(
            "'pipe-input' | {}",
            invocation(
                &python,
                "import sys,json;print(json.dumps(sys.stdin.read()))",
                &[],
                false
            )
        );
        let input: String = serde_json::from_slice(&success(&shell, &command).stdout).unwrap();
        assert_eq!(input.replace("\r\n", "\n"), "pipe-input\n");
        let command = format!(
            "$a=@({} | Where-Object {{ $_ -eq 'keep' }}); if($a.Count -ne 1 -or $a[0] -ne 'keep'){{throw 'pipeline'}}",
            invocation(&python, "print('keep');print('discard')", &[], false)
        );
        success(&shell, &command);
        let command = format!(
            "{}; exit $LASTEXITCODE",
            invocation(
                &python,
                "import sys;print('out');print('err',file=sys.stderr);sys.exit(7)",
                &[],
                false
            )
        );
        let output = run(&shell, &command);
        assert_eq!(output.status.code(), Some(7));
        assert!(String::from_utf8_lossy(&output.stdout).contains("out"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("err"));
        let binary = temp.path().join("bytes.bin");
        let baseline = temp.path().join("baseline.bin");
        let code = "import sys;sys.stdout.buffer.write(bytes([0,128,255,13,10]))";
        let ordinary = format!(
            "& '{}' '-c' '{}' > '{}'",
            python.to_string_lossy().replace('\'', "''"),
            code,
            baseline.to_string_lossy().replace('\'', "''")
        );
        success(&shell, &ordinary);
        let command = format!(
            "{} > '{}'",
            invocation(&python, code, &[], false),
            binary.to_string_lossy().replace('\'', "''")
        );
        success(&shell, &command);
        assert_eq!(
            std::fs::read(&binary).unwrap(),
            std::fs::read(&baseline).unwrap()
        );
        if shell.file_name().unwrap() == "pwsh.exe" {
            assert_eq!(std::fs::read(&binary).unwrap(), [0, 128, 255, 13, 10]);
        }
    }
}

#[test]
fn both_generated_wrappers_encode_forwarded_arguments_and_source_context_once() {
    let temp = tempfile::tempdir().unwrap();
    let python = python(temp.path());
    let context = temp.path().join("context.ps1");
    std::fs::write(&context, "$global:rezContextLoads++;").unwrap();
    let values = corpus()[..13].to_vec();
    for shell in shells() {
        for forwarding in [false, true] {
            let wrapper = temp.path().join("wrapper.ps1");
            let script = if forwarding {
                WrapperScript::generate_forward(
                    &python.to_string_lossy(),
                    &[
                        "-c".into(),
                        "import sys,json;print(json.dumps(sys.argv[1:]))".into(),
                    ],
                    &std::collections::HashMap::new(),
                    None,
                    ShellType::PowerShell,
                )
                .unwrap()
            } else {
                WrapperScript::generate(&context, &python.to_string_lossy(), ShellType::PowerShell)
            };
            script.write_to(&wrapper).unwrap();
            let mut args = vec![wrapper.to_string_lossy().into_owned()];
            if !forwarding {
                args.extend([
                    "-c".into(),
                    "import sys,json;print(json.dumps(sys.argv[1:]))".into(),
                ]);
            }
            args.extend_from_slice(&values);
            let command = format!(
                "$global:rezContextLoads=0; {}; if($rezContextLoads -ne {}){{throw 'context execution count'}}; exit $LASTEXITCODE",
                ShellType::PowerShell.join_command(&args, false, None),
                usize::from(!forwarding)
            );
            // generate_forward deliberately exits its script, not the invoking host.
            let output = success(&shell, &command);
            assert_eq!(
                serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap(),
                values
            );
        }
    }
}

#[test]
fn script_completion_preserves_native_priority_and_strict_mode_failures() {
    let temp = tempfile::tempdir().unwrap();
    let python = python(temp.path());
    let batch = temp.path().join("owned-exit7.cmd");
    std::fs::write(&batch, "@echo off\r\nexit /b 7\r\n").unwrap();
    let native = invocation(&python, "import sys;sys.exit(7)", &[], false);
    let batch_command =
        ShellType::PowerShell.join_command(&[batch.to_string_lossy().into_owned()], false, None);
    let cases = [
        ("native", native.clone(), 7),
        ("batch", batch_command, 7),
        ("native_then_cmdlet", format!("{native}\nWrite-Output done"), 7),
        ("cmdlet_failure", "Write-Error failed".into(), 1),
        ("strict_failure", "Set-StrictMode -Version Latest\nRemove-Variable LASTEXITCODE -ErrorAction SilentlyContinue\nWrite-Error failed".into(), 1),
        ("strict_success", "Set-StrictMode -Version Latest\nRemove-Variable LASTEXITCODE -ErrorAction SilentlyContinue\nWrite-Output done".into(), 0),
        ("empty", String::new(), 0),
        ("explicit_exit", "exit 7".into(), 7),
        ("function_success", "function own { Write-Output done }; own".into(), 0),
        ("function_native", format!("function own {{ {native} }}; own"), 7),
        ("advanced_function_failure", "function own { [CmdletBinding()]param(); $PSCmdlet.WriteError([Management.Automation.ErrorRecord]::new([Exception]::new('failed'),'Own',[Management.Automation.ErrorCategory]::NotSpecified,$null)) }; own".into(), 1),
    ];
    for shell in shells() {
        for (name, body, expected) in &cases {
            let script = temp.path().join("completion.ps1");
            std::fs::write(
                &script,
                format!("{body}\n{}\n", ShellType::PowerShell.exit_command()),
            )
            .unwrap();
            let output = Command::new(&shell)
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&script)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(*expected),
                "shell={}, case={name}: {}",
                shell.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let wrapper = temp.path().join("forward.ps1");
        WrapperScript::generate_forward(
            &batch.to_string_lossy(),
            &[],
            &std::collections::HashMap::new(),
            None,
            ShellType::PowerShell,
        )
        .unwrap()
        .write_to(&wrapper)
        .unwrap();
        let output = Command::new(&shell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&wrapper)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        WrapperScript::generate_forward(
            "Write-Error",
            &["failed".into()],
            &std::collections::HashMap::new(),
            None,
            ShellType::PowerShell,
        )
        .unwrap()
        .write_to(&wrapper)
        .unwrap();
        let output = Command::new(&shell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&wrapper)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let context = temp.path().join("suite-context.ps1");
        for (tool, context_body, arguments, expected) in [
            (
                batch.to_string_lossy().into_owned(),
                "",
                Vec::<String>::new(),
                7,
            ),
            ("Write-Error".into(), "", vec!["failed".into()], 1),
            (
                "Write-Error".into(),
                "Set-StrictMode -Version Latest\nRemove-Variable LASTEXITCODE -ErrorAction SilentlyContinue",
                vec!["failed".into()],
                1,
            ),
            (
                "Write-Output".into(),
                "cmd.exe /D /C exit 7",
                vec!["done".into()],
                7,
            ),
        ] {
            std::fs::write(
                &context,
                format!("Write-Output source_once\n{context_body}\n"),
            )
            .unwrap();
            WrapperScript::generate(&context, &tool, ShellType::PowerShell)
                .write_to(&wrapper)
                .unwrap();
            let output = Command::new(&shell)
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&wrapper)
                .args(arguments)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "tool={tool}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|line| *line == "source_once")
                    .count(),
                1,
                "suite sources the context exactly once",
            );
        }
    }
}
