// SPDX-License-Identifier: Apache-2.0

//! Pip post-processing utilities (rez_builder port).
//!
//! Reorganizes pip --prefix output to rez layout: site-packages, bin, shebang fix.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::errors::{Result, RezError};

/// Resolve the tool using the same PATH overlay used by the build subprocess.
pub(crate) fn command(program: &str, ctx: &super::BuildContext) -> Result<std::process::Command> {
    let executable =
        crate::shell::types::find_executable(program, Some((&ctx.env_vars, &ctx.source_path)))
            .ok_or_else(|| {
                RezError::BuildSystem(format!(
                    "Cannot resolve build tool {program} on its effective PATH"
                ))
            })?;
    let mut command = std::process::Command::new(executable);
    let mut environment = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    environment.extend(ctx.env_vars.clone());
    let environment = crate::config::CONFIG.pip_environment(&environment);
    command.envs(environment);
    if crate::config::CONFIG.offline {
        command
            .env_remove("PIP_INDEX_URL")
            .env_remove("PIP_EXTRA_INDEX_URL");
    }
    Ok(command)
}

/// Sparse source staging keeps absolute lexical coordinates without copying parents.
/// Only mutable projects and explicitly declared inputs are copied.
pub(crate) struct SourceMap {
    root: PathBuf,
    build: Option<PathBuf>,
    volumes: Vec<std::ffi::OsString>,
    pub(crate) paths: std::collections::BTreeMap<PathBuf, PathBuf>,
}

impl SourceMap {
    pub(crate) fn new(work: &Path, build: Option<&Path>) -> Self {
        Self {
            root: work.to_path_buf(),
            build: build.map(Path::to_path_buf),
            volumes: Vec::new(),
            paths: std::collections::BTreeMap::new(),
        }
    }

    fn path(source: &Path) -> Result<PathBuf> {
        use std::path::Component;
        let absolute = crate::util::path_key(&std::path::absolute(source)?);
        let mut original = PathBuf::new();
        for component in absolute.components() {
            if component == Component::ParentDir {
                if !original.pop() {
                    return Err(RezError::BuildSystem(
                        "Source path escapes its filesystem root".into(),
                    ));
                }
            } else if component != Component::CurDir {
                original.push(component.as_os_str());
            }
        }
        Ok(original)
    }

    pub(crate) fn stage(&mut self, source: &Path) -> Result<PathBuf> {
        use std::path::Component;
        let original = Self::path(source)?;
        if let Some(staged) = self.paths.get(&original) {
            return Ok(staged.clone());
        }
        let prefix = original
            .components()
            .find_map(|component| {
                if let Component::Prefix(prefix) = component {
                    Some(prefix.as_os_str().to_os_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let volume = match self.volumes.iter().position(|value| value == &prefix) {
            Some(index) => index,
            None => {
                self.volumes.push(prefix);
                self.volumes.len() - 1
            }
        };
        let mut relative = PathBuf::from("sources").join(format!("volume-{volume}"));
        for component in original.components() {
            if let Component::Normal(value) = component {
                relative.push(value);
            }
        }
        let staged = self.root.join(&relative);
        let metadata = fs::metadata(&original)?;
        if metadata.is_dir() {
            let excluded = self.build.as_ref().and_then(|path| {
                let path = fs::canonicalize(path).ok()?;
                let base = fs::canonicalize(&original).ok()?;
                let relative = path.strip_prefix(base).ok()?;
                (!relative.as_os_str().is_empty()).then(|| vec![relative.to_path_buf()])
            });
            foundation::filesystem::copy_dir_contents(
                &original,
                &staged,
                true,
                true,
                Some(&self.root),
                excluded.as_deref(),
            )?;
        } else if metadata.is_file() {
            crate::util::directory(&self.root, relative.parent().unwrap(), true)?;
            foundation::filesystem::copy_file(&original, &staged, true)?;
        } else {
            return Err(RezError::BuildSystem(format!(
                "Unsupported source input {}",
                original.display()
            )));
        }
        self.paths.insert(original, staged.clone());
        Ok(staged)
    }

    pub(crate) fn inputs(&mut self, ctx: &super::BuildContext, backend: &str) -> Result<()> {
        let Some(inputs) = ctx
            .package_config
            .as_ref()
            .and_then(|config| config.get(backend))
            .and_then(|config| config.get("source_inputs"))
        else {
            return Ok(());
        };
        let inputs: Vec<PathBuf> = serde_json::from_value(inputs.clone()).map_err(|error| {
            RezError::BuildSystem(format!("Invalid config.{backend}.source_inputs: {error}"))
        })?;
        let source = Self::path(&ctx.source_path)?;
        for input in inputs {
            let input = if input.is_absolute() {
                input
            } else {
                source.join(input)
            };
            let input = Self::path(&input)?;
            if source.starts_with(&input) && source != input {
                return Err(RezError::BuildSystem(
                    "Declare exact source inputs instead of a project ancestor".into(),
                ));
            }
            self.stage(&input)?;
        }
        Ok(())
    }

    pub(crate) fn relocate(&self, payload: &Path) -> Result<()> {
        let mut paths = self.paths.iter().collect::<Vec<_>>();
        paths.sort_by_key(|(_, staged)| std::cmp::Reverse(staged.components().count()));
        for (original, staged) in paths {
            relocate(payload, staged, original)?;
        }
        Ok(())
    }
}

/// Preserve the shared source-copy API used by native pip wheel preparation.
pub(crate) fn source(source: &Path, work: &Path, build_path: Option<&Path>) -> Result<PathBuf> {
    SourceMap::new(work, build_path).stage(source)
}

#[derive(serde::Deserialize)]
struct SourceGraph {
    files: std::collections::BTreeMap<String, String>,
    projects: Vec<PathBuf>,
    args: Vec<String>,
    rewritten: std::collections::BTreeMap<String, String>,
}

impl SourceMap {
    /// Use the selected pip's own parsers and session; Rust only copies discovered projects.
    pub(crate) fn pip(
        &mut self,
        program: &str,
        ctx: &super::BuildContext,
        args: &[std::ffi::OsString],
    ) -> Result<std::process::Command> {
        use std::io::Write;
        use std::process::Stdio;
        let selected = command(program, ctx)?;
        let launcher = PathBuf::from(selected.get_program());
        let explicit = ctx
            .package_config
            .as_ref()
            .and_then(|config| config.get("pip"))
            .and_then(|config| config.get("python"))
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    RezError::BuildSystem("config.pip.python must be a string".into())
                })
            })
            .transpose()?;
        let candidates = if let Some(explicit) = explicit {
            vec![explicit]
        } else {
            let content = fs::read(&launcher)?;
            let mut found = None;
            // distlib appends an interpreter shebang before its executable ZIP.
            // Validate the path rather than selecting an ambient same-version Python.
            for start in content
                .windows(2)
                .enumerate()
                .filter_map(|(index, value)| (value == b"#!").then_some(index + 2))
                .rev()
            {
                let end = content[start..]
                    .iter()
                    .position(|byte| *byte == b'\n' || *byte == b'\r')
                    .map_or(content.len(), |end| start + end);
                let Ok(header) = std::str::from_utf8(&content[start..end]) else {
                    continue;
                };
                let header = header.trim();
                let executable = if let Some(header) = header.strip_prefix('"') {
                    header.split_once('"').map(|(path, _)| path)
                } else {
                    header.split_whitespace().next()
                };
                if let Some(executable) = executable {
                    let path = Path::new(executable);
                    if path.is_absolute()
                        && path.is_file()
                        && path.file_stem().is_some_and(|name| {
                            name.to_string_lossy()
                                .to_ascii_lowercase()
                                .starts_with("python")
                        })
                    {
                        found = Some(executable.to_owned());
                        break;
                    }
                }
            }
            found.into_iter().collect()
        };
        let mut interpreter = None;
        for candidate in candidates {
            let Ok(mut probe) = command(&candidate, ctx) else {
                continue;
            };
            probe
                .current_dir(&ctx.source_path)
                .args(["-c", PIP_SOURCE_LAUNCHER])
                .arg(&launcher);
            let output = probe.output()?;
            if !output.status.success() {
                return Err(RezError::BuildSystem(format!(
                    "Cannot safely bridge selected pip launcher {}: {}",
                    launcher.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            let Some(line) = output
                .stdout
                .split(|byte| *byte == b'\n')
                .rev()
                .find(|line| !line.is_empty())
            else {
                continue;
            };
            let Ok(values) = serde_json::from_slice::<Vec<String>>(line) else {
                continue;
            };
            if values.len() == 2 {
                interpreter = Some((values[0].clone(), values[1].clone()));
                break;
            }
        }
        let (interpreter, selected_runner) = interpreter.ok_or_else(|| RezError::BuildSystem(
            "Cannot verify the selected pip interpreter; set config.pip.python to its executable".into()
        ))?;
        if Path::new(&selected_runner).file_name()
            != Some(std::ffi::OsStr::new("__pip-runner__.py"))
            || !Path::new(&selected_runner).is_file()
        {
            return Err(RezError::BuildSystem(
                "Selected pip runner cannot safely host the source parser; a standard installed pip is required".into()));
        }
        let arguments = args
            .iter()
            .map(|arg| {
                arg.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| RezError::BuildSystem("Pip arguments must be UTF-8".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        let operation = arguments
            .first()
            .ok_or_else(|| RezError::BuildSystem("Missing pip command".into()))?;
        // Let the selected pip interpret global/env --python with its own policy.
        // A target environment need not contain pip: retain the selected runner.
        let mut policy = command(&interpreter, ctx)?;
        policy
            .current_dir(&ctx.source_path)
            .args([
                "-c",
                &pip_source_bootstrap(),
                &selected_runner,
                PIP_SOURCE_PYTHON,
            ])
            .args(&arguments);
        let policy = super::run_cmd(&mut policy, "selected pip Python policy")?;
        let policy: serde_json::Value =
            serde_json::from_slice(&policy.stdout).map_err(|error| {
                RezError::BuildSystem(format!("Invalid pip Python policy: {error}"))
            })?;
        let interpreter = policy["executable"]
            .as_str()
            .ok_or_else(|| RezError::BuildSystem("Missing effective pip interpreter".into()))?;
        let runner = policy["runner"]
            .as_str()
            .ok_or_else(|| RezError::BuildSystem("Missing selected pip runner".into()))?;
        if runner != selected_runner {
            return Err(RezError::BuildSystem(
                "Selected pip runner changed during policy discovery".into(),
            ));
        }
        let mut probe = command(interpreter, ctx)?;
        probe.current_dir(&ctx.source_path).env("_PIP_RUNNING_IN_SUBPROCESS", "1")
            .args(["-c", &pip_source_bootstrap(), runner,
                "import json,sys; from pip._internal.utils.misc import get_pip_version; print(json.dumps([sys.executable,get_pip_version()]))"]);
        let output = super::run_cmd(&mut probe, "effective pip interpreter identity")?;
        let values: Vec<String> = serde_json::from_slice(&output.stdout).map_err(|error| {
            RezError::BuildSystem(format!("Invalid effective pip identity: {error}"))
        })?;
        if values.len() != 2 {
            return Err(RezError::BuildSystem(
                "Incomplete effective pip identity".into(),
            ));
        }
        let interpreter = &values[0];
        let identity = &values[1];
        let mut request = serde_json::json!({
            "identity": identity, "cwd": Self::path(&ctx.source_path)?,
            "command": operation, "args": &arguments[1..]
        });
        let invoke = |request: &serde_json::Value| -> Result<SourceGraph> {
            let mut process = command(interpreter, ctx)?;
            process
                .current_dir(&ctx.source_path)
                .env("_PIP_RUNNING_IN_SUBPROCESS", "1")
                .args(["-c", &pip_source_bootstrap(), runner, PIP_SOURCE_GRAPH])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = process.spawn()?;
            let bytes = serde_json::to_vec(request)?;
            child
                .stdin
                .take()
                .ok_or_else(|| RezError::BuildSystem("Missing pip parser input".into()))?
                .write_all(&bytes)?;
            let output = child.wait_with_output()?;
            if !output.status.success() {
                return Err(RezError::BuildSystem(format!(
                    "Selected pip source parser failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            let line = output
                .stdout
                .split(|byte| *byte == b'\n')
                .rev()
                .find(|line| !line.is_empty())
                .ok_or_else(|| RezError::BuildSystem("Empty pip parser response".into()))?;
            serde_json::from_slice(line).map_err(|error| {
                RezError::BuildSystem(format!("Invalid selected pip parser response: {error}"))
            })
        };
        let graph = invoke(&request)?;
        for project in &graph.projects {
            self.stage(project)?;
        }
        self.inputs(ctx, "pip")?;
        let mut mapping = std::collections::BTreeMap::new();
        for (original, staged) in &self.paths {
            mapping.insert(
                original.to_string_lossy().into_owned(),
                staged.to_string_lossy().into_owned(),
            );
        }
        // Python constructors may retain a different drive-letter spelling.
        // Keep their exact operands as aliases of the shared normalized identity.
        for original in &graph.projects {
            let normalized = Self::path(original)?;
            let staged = self
                .paths
                .get(&normalized)
                .ok_or_else(|| RezError::BuildSystem("Unmapped pip project".into()))?;
            mapping.insert(
                original.to_string_lossy().into_owned(),
                staged.to_string_lossy().into_owned(),
            );
        }
        let directory = crate::util::directory(&self.root, Path::new("requirements"), true)?;
        let destinations = graph
            .files
            .keys()
            .enumerate()
            .map(|(index, original)| {
                (
                    original.clone(),
                    directory
                        .join(format!("{index}.txt"))
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        request["files"] = serde_json::to_value(&graph.files)?;
        request["mapping"] = serde_json::to_value(&mapping)?;
        request["destinations"] = serde_json::to_value(&destinations)?;
        let graph = invoke(&request)?;
        for (original, text) in graph.rewritten {
            let path = destinations.get(&original).ok_or_else(|| {
                RezError::BuildSystem("Unknown rewritten requirement file".into())
            })?;
            crate::serialise::atomic_write(Path::new(path), text.as_bytes())?;
        }
        let mut process = command(interpreter, ctx)?;
        process
            .current_dir(&ctx.source_path)
            .env("_PIP_RUNNING_IN_SUBPROCESS", "1")
            .args([
                "-c",
                &pip_source_bootstrap(),
                runner,
                PIP_SOURCE_RUN,
                identity,
                operation,
            ]);
        process.args(graph.args);
        Ok(process)
    }
}

// Recognize semantics before bypassing a launcher. An arbitrary wrapper may
// inject configuration or environment policy that cannot be inferred from --version.
const PIP_SOURCE_LAUNCHER: &str = r###"import ast, io, json, os, pathlib, sys, tokenize, zipfile
path = pathlib.Path(sys.argv[1])
# Match the launcher's script search path; a checkout may itself contain pip/.
sys.path[0] = str(path.parent)
from pip._internal.build_env import get_runnable_pip
from pip._vendor.distlib.scripts import SCRIPT_TEMPLATE
data = path.read_bytes()
def reject(reason):
    raise RuntimeError('Unsupported pip launcher capability: ' + reason)
def interpreter(header):
    header = header.decode('utf-8').strip()
    if header.startswith('"'):
        executable, separator, arguments = header[1:].partition('"')
        if not separator:
            reject('unterminated interpreter quote')
    else:
        values = header.split(None, 1)
        executable = values[0] if values else ''
        arguments = values[1] if len(values) > 1 else ''
    if arguments.strip():
        reject('interpreter flags or shell policy would be bypassed')
    if executable == '/usr/bin/env':
        reject('env launcher requires an explicit interpreter policy')
    if os.path.normcase(os.path.abspath(executable)) != os.path.normcase(os.path.abspath(sys.executable)):
        reject('configured Python differs from the launcher interpreter')
if data.startswith(b'MZ'):
    from pip._vendor.distlib.scripts import WRAPPERS
    prefixes = [value for name, value in WRAPPERS.items() if name.startswith('t') and data.startswith(value)]
    if len(prefixes) != 1:
        reject('PE loader is not an exact selected distlib console launcher')
    tail = data[len(prefixes[0]):]
    header, separator, archive = tail.partition(b'\n')
    if not separator or not header.startswith(b'#!'):
        reject('missing standard distlib interpreter shebang')
    interpreter(header[2:])
    with zipfile.ZipFile(io.BytesIO(archive)) as payload:
        if payload.namelist() != ['__main__.py']:
            reject('unexpected executable ZIP entries')
        data = payload.read('__main__.py')
elif data.startswith(b'#!'):
    header = data.splitlines()[0][2:]
    interpreter(header)
encoding, _ = tokenize.detect_encoding(io.BytesIO(data).readline)
try:
    tree = ast.dump(ast.parse(data.decode(encoding)), include_attributes=False)
except (SyntaxError, UnicodeError) as error:
    reject('entry point is not a standard Python pip script: ' + str(error))
templates = [SCRIPT_TEMPLATE % {'module': 'pip._internal.cli.main', 'import_name': 'main', 'func': 'main'}]
# These observed standard entry points differ only in argv[0] normalization.
for pattern in [r'(-script\.pyw|\.exe)?$', r'(-script\.pyw?|\.exe)?$']:
    templates.append("import re\nimport sys\nfrom pip._internal.cli.main import main\nif __name__ == '__main__':\n    sys.argv[0] = re.sub(" + repr(pattern) + ", '', sys.argv[0])\n    sys.exit(main())\n")
templates.append("import sys\nfrom pip._internal.cli.main import main\nif __name__ == '__main__':\n    if sys.argv[0].endswith('.exe'):\n        sys.argv[0] = sys.argv[0][:-4]\n    sys.exit(main())\n")
if tree not in {ast.dump(ast.parse(value), include_attributes=False) for value in templates}:
    reject('entry point contains nonstandard wrapper behavior')
print(json.dumps([sys.executable, get_runnable_pip()]))
"###;

const PIP_SOURCE_PYTHON: &str = r###"import json, os, sys
from pip._internal.build_env import get_runnable_pip
from pip._internal.cli.main_parser import create_main_parser, identify_python_interpreter
from pip._internal.commands import create_command
general, remaining = create_main_parser().parse_args(sys.argv[1:])
redirect = bool(general.python and '_PIP_RUNNING_IN_SUBPROCESS' not in os.environ)
if redirect:
    executable = identify_python_interpreter(general.python)
    if executable is None:
        raise RuntimeError('Could not locate Python interpreter ' + general.python)
else:
    executable = sys.executable
    command = create_command(remaining[0], isolated=('--isolated' in remaining[1:]))
    options, operands = command.parse_args(remaining[1:])
    if options.python and '_PIP_RUNNING_IN_SUBPROCESS' not in os.environ:
        raise RuntimeError('The --python option must be placed before the pip subcommand name')
print(json.dumps({'executable': executable, 'runner': get_runnable_pip()}))
"###;

// Execute the actual selected runner's compatibility checks and import policy.
// Intercept only its final module entry, after PipImportRedirectingFinder exists.
pub(crate) const PIP_OFFLINE_GUARD: &str = include_str!("pip_offline.py");

fn pip_source_bootstrap() -> String {
    format!("{PIP_OFFLINE_GUARD}\n{PIP_SOURCE_BOOTSTRAP}")
}

const PIP_SOURCE_BOOTSTRAP: &str = r###"import os, runpy, sys
runner = sys.argv.pop(1)
script = sys.argv.pop(1)
original = runpy.run_module
entered = False
def module(name, *args, **kwargs):
    global entered
    if name == 'pip':
        entered = True
        _rez_apply_pip_offline()
        exec(compile(script, '<rez-pip-source>', 'exec'), {'__name__': '__main__'})
        return {}
    return original(name, *args, **kwargs)
runpy.run_module = module
runpy.run_path(runner, run_name='__main__')
if not entered:
    raise RuntimeError('Selected pip runner did not enter its pip module')
"###;

const PIP_SOURCE_GRAPH: &str = r###"import json, locale, os, shlex, sys
from collections import Counter
from urllib.parse import urljoin, urlsplit, urlunsplit
from pip._internal.commands import create_command
from pip._internal.req import req_file
from pip._internal.req.constructors import parse_req_from_line, parse_req_from_editable
from pip._internal.models.link import Link
from pip._internal.utils.misc import get_pip_version
from pip._internal.utils.urls import path_to_url
request = json.load(sys.stdin)
if get_pip_version() != request['identity']:
    raise RuntimeError('Selected pip interpreter identity changed')
os.chdir(request['cwd'])
try:
    locale.setlocale(locale.LC_ALL, '')
except locale.Error:
    pass
cli = create_command(request['command'], isolated=('--isolated' in request['args']))
options, operands = cli.parse_args(request['args'])
session = cli._build_session(options)
finder = cli._build_package_finder(options, session)
files = dict(request.get('files', {}))
projects = set()
mapping = request.get('mapping', {})
destinations = request.get('destinations', {})
def requirement(value, editable=False):
    parts = parse_req_from_editable(value) if editable else parse_req_from_line(value, None)
    link = parts.link
    if link is None and parts.requirement is not None and parts.requirement.url:
        link = Link(parts.requirement.url)
    if os.environ.get('REZ_OFFLINE') == 'true' and link is not None and link.scheme != 'file':
        raise RuntimeError('REZ_OFFLINE=true: remote pip sources are disabled: ' + str(link))
    if link is None or link.scheme != 'file' or not os.path.isdir(link.file_path):
        return value
    original = os.path.abspath(link.file_path)
    projects.add(original)
    if original not in mapping:
        return value
    parsed = urlsplit(link.url)
    staged = urlsplit(path_to_url(mapping[original]))
    url = urlunsplit((staged.scheme, staged.netloc, staged.path, parsed.query, parsed.fragment))
    extras = set(parts.extras or ())
    if parts.requirement is not None:
        extras.update(parts.requirement.extras)
    extra = '[' + ','.join(sorted(extras)) + ']' if extras else ''
    marker = parts.markers or (parts.requirement.marker if parts.requirement is not None else None)
    if editable:
        value = url if parsed.fragment or parsed.query else mapping[original] + extra
    elif parts.requirement is not None:
        value = parts.requirement.name + extra + ' @ ' + url
    else:
        value = url if parsed.fragment or parsed.query else mapping[original] + extra
    if marker is not None:
        value += ' ; ' + str(marker)
    return value
original_get = req_file.get_file_content
def cached_get(filename, active_session):
    if filename not in files:
        location, content = original_get(filename, active_session)
        files[filename] = content
        return location, content
    return filename, files[filename]
req_file.get_file_content = cached_get
parser = req_file.RequirementsFileParser(session, req_file.get_line_parser(finder))
for filename, constraint in [(path, False) for path in options.requirements] + [(path, True) for path in options.constraints]:
    for line in parser.parse(filename, constraint):
        parsed = req_file.handle_line(line, options=options, finder=finder, session=session)
        if parsed is not None:
            requirement(parsed.requirement, parsed.is_editable)
for operand in operands:
    requirement(operand)
for operand in options.editables:
    requirement(operand, True)
def included(value, filename):
    if filename is not None and req_file.SCHEME_RE.search(filename):
        return urljoin(filename, value)
    if not req_file.SCHEME_RE.search(value):
        return os.path.abspath(os.path.join(os.path.dirname(filename), value)) if filename is not None else value
    return value
seen = Counter()
def option_value(option, value, filename, record=True):
    if record and filename is None and option.dest in ('requirements', 'constraints', 'editables'):
        seen[(option.dest, value)] += 1
    if option.dest in ('requirements', 'constraints'):
        key = included(value, filename)
        return destinations.get(key, value)
    if option.dest == 'editables':
        return requirement(value, True)
    if option.dest == 'find_links' and filename is not None:
        candidate = os.path.join(os.path.dirname(os.path.abspath(filename)), value)
        if os.path.exists(candidate):
            return os.path.abspath(candidate)
    return value
def arguments(tokens, option_parser, filename=None, positional=False):
    output = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token == '--':
            output.append(token)
            output.extend(requirement(value) if positional else value for value in tokens[index+1:])
            break
        if token.startswith('--'):
            flag, equal, attached = token.partition('=')
            option = option_parser._long_opt[option_parser._match_long_opt(flag)]
            if option.takes_value():
                if equal:
                    output.append(flag + '=' + option_value(option, attached, filename))
                else:
                    values = tokens[index+1:index+1+option.nargs]
                    if len(values) != option.nargs:
                        raise RuntimeError('Missing validated pip option value')
                    if option.nargs == 1:
                        values = [option_value(option, values[0], filename)]
                    output.extend([token] + values)
                    index += option.nargs
            else:
                output.append(token)
        elif token.startswith('-') and token != '-':
            cursor = 1
            while cursor < len(token):
                flag = '-' + token[cursor]
                option = option_parser._short_opt[flag]
                if option.takes_value():
                    attached = token[cursor+1:]
                    if attached:
                        output.append(token[:cursor+1] + option_value(option, attached, filename))
                    else:
                        values = tokens[index+1:index+1+option.nargs]
                        if len(values) != option.nargs:
                            raise RuntimeError('Missing validated pip short option value')
                        if option.nargs == 1:
                            values = [option_value(option, values[0], filename)]
                        output.extend([token] + values)
                        index += option.nargs
                    break
                cursor += 1
            else:
                output.append(token)
        else:
            output.append(requirement(token) if positional else token)
        index += 1
    return output
rewritten = {}
if mapping or destinations:
    for filename, content in files.items():
        lines = []
        for _, line in req_file.preprocess(content):
            args, opts = req_file.get_line_parser(None)(line)
            _, options_text = req_file.break_args_options(line)
            tokens = arguments(shlex.split(options_text), req_file.build_parser(), filename)
            transformed = requirement(args) if args else ''
            lines.append((transformed + ' ' + shlex.join(tokens)).strip())
        rewritten[filename] = '\n'.join(lines) + '\n'
output = arguments(request['args'], cli.parser, positional=True)
for dest, flag in [('requirements', '-r'), ('constraints', '-c'), ('editables', '-e')]:
    for value in getattr(options, dest):
        if seen[(dest, value)]:
            seen[(dest, value)] -= 1
        else:
            output.extend([flag, option_value(cli.parser._short_opt[flag], value, None, record=False)])
print(json.dumps({'files': files, 'projects': sorted(projects), 'args': output, 'rewritten': rewritten}))
"###;

// Source-bearing configuration defaults are materialized by the parser bridge.
// Keep all other selected interpreter configuration, environment and pip behavior.
const PIP_SOURCE_RUN: &str = r###"import sys
from pip._internal.cli.parser import ConfigOptionParser
from pip._internal.cli.main import main
from pip._internal.utils.misc import get_pip_version
if get_pip_version() != sys.argv[1]:
    raise RuntimeError('Selected pip interpreter identity changed')
original = ConfigOptionParser._get_ordered_configuration_items
def items(self):
    for key, value in original(self):
        if self.name not in ('install', 'wheel') or key not in ('requirement', 'constraint', 'editable'):
            yield key, value
ConfigOptionParser._get_ordered_configuration_items = items
sys.exit(main(sys.argv[2:]))
"###;

/// Restore persistent source references only in generated editable metadata.
/// User modules are never rewritten as part of build staging.
pub(crate) fn relocate(payload: &Path, source: &Path, original: &Path) -> Result<()> {
    let source = source.to_string_lossy().into_owned();
    let original = SourceMap::path(original)?.to_string_lossy().into_owned();
    let normal = |value: &str| {
        crate::util::path_key(Path::new(value))
            .to_string_lossy()
            .into_owned()
    };
    let mut paths = vec![
        (source.clone(), original.clone()),
        (normal(&source), normal(&original)),
        (
            normal(&source).replace('\\', "/"),
            normal(&original).replace('\\', "/"),
        ),
    ];
    if let (Ok(from), Ok(to)) = (
        url::Url::from_directory_path(normal(&source)),
        url::Url::from_directory_path(normal(&original)),
    ) {
        paths.push((
            from.as_str().trim_end_matches('/').into(),
            to.as_str().trim_end_matches('/').into(),
        ));
    }
    // Windows canonicalization expands existing 8.3 components and may add a
    // verbatim prefix. Pip can record either spelling for the same staged tree.
    // Retain the caller's original checkout spelling as the relocation target.
    if let Ok(canonical) = fs::canonicalize(&source) {
        let canonical = canonical.to_string_lossy().into_owned();
        paths.push((canonical.clone(), original.clone()));
        paths.push((normal(&canonical), normal(&original)));
        paths.push((
            normal(&canonical).replace('\\', "/"),
            normal(&original).replace('\\', "/"),
        ));
        if let (Ok(from), Ok(to)) = (
            url::Url::from_directory_path(normal(&canonical)),
            url::Url::from_directory_path(normal(&original)),
        ) {
            paths.push((
                from.as_str().trim_end_matches('/').into(),
                to.as_str().trim_end_matches('/').into(),
            ));
        }
    }
    let mut replacements = Vec::new();
    for (from, to) in paths {
        replacements.push((0, from.as_bytes().to_vec(), to.as_bytes().to_vec()));
        let quoted_to = to
            .replace('\\', r"\\")
            .replace('\'', r"\'")
            .replace('"', r#"\""#);
        for quote in ['\'', '"'] {
            let quoted_from = from
                .replace('\\', r"\\")
                .replace(quote, &format!("\\{quote}"));
            replacements.push((1, quoted_from.into_bytes(), quoted_to.as_bytes().to_vec()));
        }
        let json_from = serde_json::to_string(&from)?;
        let json_to = serde_json::to_string(&to)?;
        replacements.push((
            2,
            json_from.as_bytes()[1..json_from.len() - 1].to_vec(),
            json_to.as_bytes()[1..json_to.len() - 1].to_vec(),
        ));
    }
    replacements.sort_by_key(|(_, from, _)| std::cmp::Reverse(from.len()));
    let mut directories = vec![payload.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() && !crate::util::is_redirect(&metadata) {
                directories.push(entry.path());
            } else if metadata.is_file() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let generated = name.ends_with(".pth")
                    || name.ends_with(".egg-link")
                    || (name.starts_with("__editable__") && name.ends_with(".py"))
                    || (name == "direct_url.json"
                        && directory
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().ends_with(".dist-info")));
                if !generated {
                    continue;
                }
                let original_bytes = fs::read(entry.path())?;
                let mut content = original_bytes.clone();
                let encoding = if name.ends_with(".py") {
                    1
                } else if name.ends_with(".json") {
                    2
                } else {
                    0
                };
                for (kind, from, to) in &replacements {
                    if *kind != encoding || from.is_empty() {
                        continue;
                    }
                    let mut replaced = Vec::new();
                    let mut cursor = 0;
                    while let Some(offset) = content[cursor..]
                        .windows(from.len())
                        .position(|part| part == from)
                    {
                        let end = cursor + offset;
                        replaced.extend_from_slice(&content[cursor..end]);
                        replaced.extend_from_slice(to);
                        cursor = end + from.len();
                    }
                    replaced.extend_from_slice(&content[cursor..]);
                    content = replaced;
                }
                if content != original_bytes {
                    fs::write(entry.path(), content)?;
                }
            }
        }
    }
    Ok(())
}

/// Replace shebang in executables: absolute python path -> #!/usr/bin/env python or #!python.exe
pub fn make_shebang_movable(bin_path: &Path, relocation: Option<(&Path, &Path)>) -> Result<()> {
    if !bin_path.is_dir() {
        return Ok(());
    }

    for e in fs::read_dir(bin_path).map_err(|e| {
        RezError::BuildSystem(format!("Cannot read bin {}: {}", bin_path.display(), e))
    })? {
        let e = e?;
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        let is_script = if cfg!(windows) {
            ext == "py" || ext == "exe"
        } else {
            true
        };
        if !is_script {
            continue;
        }

        let mut content = Vec::new();
        {
            let mut f = fs::File::open(&p).map_err(|e| {
                RezError::BuildSystem(format!("Cannot open {}: {}", p.display(), e))
            })?;
            f.read_to_end(&mut content)?;
        }

        let modified = if let Some((physical, logical)) = relocation {
            relocate_shebang(&content, physical, logical)
        } else if cfg!(windows) {
            replace_windows_shebang(&content)
        } else {
            replace_posix_shebang(&content)
        };

        if let Some(data) = modified {
            fs::File::create(&p)?.write_all(&data).map_err(|e| {
                RezError::BuildSystem(format!("Cannot write {}: {}", p.display(), e))
            })?;
        }
    }
    Ok(())
}

/// Relocate only a launcher's shebang, including distlib's appended Windows
/// launcher shebang. Do not rewrite the selected Python interpreter to ambient Python.
fn relocate_shebang(content: &[u8], physical: &Path, logical: &Path) -> Option<Vec<u8>> {
    let start = if content.starts_with(b"#!") {
        0
    } else if content.starts_with(b"MZ") {
        content.windows(2).position(|bytes| bytes == b"#!")?
    } else {
        return None;
    };
    let end = content[start..]
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
        .map_or(content.len(), |offset| start + offset);
    let header = std::str::from_utf8(&content[start..end]).ok()?;
    let physical = physical.to_str()?;
    let logical = logical.to_str()?;
    if !header.contains(physical) {
        return None;
    }
    let replaced = header.replace(physical, logical);
    let mut output = content[..start].to_vec();
    output.extend_from_slice(replaced.as_bytes());
    output.extend_from_slice(&content[end..]);
    Some(output)
}

/// Windows: replace #!(.*)python.exe -> #!python.exe (padded), same for pythonw.exe
fn replace_windows_shebang(content: &[u8]) -> Option<Vec<u8>> {
    if !content.starts_with(b"#!") {
        return None;
    }
    let line_end = content
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .unwrap_or(content.len());
    let mut line = content[..line_end].to_vec();
    let mut changed = false;

    for (pattern, replacement) in [
        (b"python.exe".as_slice(), b"#!python.exe".as_slice()),
        (b"pythonw.exe".as_slice(), b"#!pythonw.exe".as_slice()),
    ] {
        if let Some(idx) = line.windows(pattern.len()).position(|w| w == pattern) {
            let match_end = idx + pattern.len();
            let to_replace_len = match_end;
            let mut repl = replacement.to_vec();
            repl.resize(to_replace_len, b' ');
            line[..match_end].copy_from_slice(&repl);
            changed = true;
        }
    }
    if changed {
        let mut out = line;
        out.extend_from_slice(&content[line_end..]);
        Some(out)
    } else {
        None
    }
}

/// POSIX: replace #!...python -> #!/usr/bin/env python
fn replace_posix_shebang(content: &[u8]) -> Option<Vec<u8>> {
    if !content.starts_with(b"#!") {
        return None;
    }
    let line_end = content.iter().position(|&b| b == b'\n' || b == b'\r')?;
    let line = &content[..line_end];
    if !line.windows(6).any(|w| w == b"python") || line.starts_with(b"#!/usr/bin/env ") {
        return None;
    }
    let shebang = b"#!/usr/bin/env python";
    let mut out = shebang.to_vec();
    if line.len() > shebang.len() {
        out.extend(std::iter::repeat_n(b' ', line.len() - shebang.len()));
    }
    out.push(b'\n');
    out.extend_from_slice(&content[line_end + 1..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires installed pip; only validates launcher capability without installing packages"]
    fn pip_wrapper_rejection_precedes_source_copy_and_execution() {
        let owned = tempfile::tempdir().unwrap();
        let project = owned.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("setup.py"), "original").unwrap();
        let marker = owned.path().join("wrapper-executed");
        let wrapper = owned.path().join("pip-wrapper.py");
        fs::write(&wrapper, format!(
            "import os, sys\nfrom pathlib import Path\nfrom pip._internal.cli.main import main\nPath({}).write_text('executed')\nos.environ['PIP_INDEX_URL'] = 'https://private.invalid/simple'\nsys.exit(main())\n",
            serde_json::to_string(&marker.to_string_lossy()).unwrap(),
        )).unwrap();
        let work = tempfile::tempdir().unwrap();
        let mut ctx = super::super::BuildContext::new(
            project.clone(),
            project.join("build"),
            owned.path().join("install"),
        );
        let python = std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into());
        ctx.package_config = Some(serde_json::json!({"pip": {"python": python}}));
        let mut map = SourceMap::new(work.path(), None);
        let error = map
            .pip(
                wrapper.to_str().unwrap(),
                &ctx,
                &["install".into(), ".".into()],
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Unsupported pip launcher capability"),
            "{error}"
        );
        assert!(map.paths.is_empty());
        assert!(!work.path().join("sources").exists());
        assert!(!marker.exists());
        assert_eq!(
            fs::read_to_string(project.join("setup.py")).unwrap(),
            "original"
        );
    }

    #[test]
    fn source_map_preserves_sparse_sibling_coordinates() {
        let checkout = tempfile::tempdir().unwrap();
        let project = checkout.path().join("project");
        let shared = checkout.path().join("shared");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&shared).unwrap();
        fs::write(project.join("setup.py"), "owned").unwrap();
        fs::write(shared.join("resource"), "shared").unwrap();
        fs::write(checkout.path().join("unrelated"), "valuable").unwrap();
        let work = tempfile::tempdir().unwrap();
        let mut map = SourceMap::new(work.path(), None);
        let staged = map.stage(&project).unwrap();
        let input = map.stage(&project.join("../shared/resource")).unwrap();
        assert_eq!(input, staged.parent().unwrap().join("shared/resource"));
        assert_eq!(map.stage(&shared.join("resource")).unwrap(), input);
        assert_eq!(map.paths.len(), 2);
        assert!(!staged.parent().unwrap().join("unrelated").exists());
        fs::write(&input, "modified").unwrap();
        assert_eq!(
            fs::read_to_string(shared.join("resource")).unwrap(),
            "shared"
        );
        assert_eq!(
            fs::read_to_string(checkout.path().join("unrelated")).unwrap(),
            "valuable"
        );
    }

    #[cfg(windows)]
    #[test]
    fn source_map_unifies_verbatim_drive_and_declared_siblings() {
        let checkout = tempfile::tempdir().unwrap();
        let project = checkout.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(checkout.path().join("resource"), "shared").unwrap();
        let verbatim = fs::canonicalize(&project).unwrap();
        let normal = crate::util::path_key(&verbatim);
        let text = normal.to_str().unwrap();
        let lower_drive = format!("{}{}", text[..1].to_ascii_lowercase(), &text[1..]);
        let work = tempfile::tempdir().unwrap();
        let mut ctx = super::super::BuildContext::new(
            verbatim.clone(),
            project.join("build"),
            project.join("install"),
        );
        ctx.package_config = Some(serde_json::json!({"pip": {"source_inputs": ["../resource"]}}));
        let mut map = SourceMap::new(work.path(), None);
        let staged = map.stage(&normal).unwrap();
        assert_eq!(map.stage(&verbatim).unwrap(), staged);
        assert_eq!(map.stage(Path::new(&lower_drive)).unwrap(), staged);
        map.inputs(&ctx, "pip").unwrap();
        assert!(staged.parent().unwrap().join("resource").is_file());
        assert_eq!(map.volumes.len(), 1);
        assert_eq!(map.paths.len(), 2);
    }

    #[test]
    fn source_inputs_reject_ancestor_and_keep_exact_sibling() {
        let checkout = tempfile::tempdir().unwrap();
        let project = checkout.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(checkout.path().join("resource"), "shared").unwrap();
        let work = tempfile::tempdir().unwrap();
        let mut ctx = super::super::BuildContext::new(
            project.clone(),
            project.join("build"),
            project.join("install"),
        );
        ctx.package_config =
            Some(serde_json::json!({"python": {"source_inputs": ["../resource"]}}));
        let mut map = SourceMap::new(work.path(), None);
        let staged = map.stage(&project).unwrap();
        map.inputs(&ctx, "python").unwrap();
        assert!(staged.parent().unwrap().join("resource").is_file());
        ctx.package_config = Some(serde_json::json!({"python": {"source_inputs": [".."]}}));
        assert!(map.inputs(&ctx, "python").is_err());
        assert_eq!(map.paths.len(), 2);
    }

    #[test]
    fn source_map_relocates_every_editable_project() {
        let checkout = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let payload = tempfile::tempdir().unwrap();
        let mut map = SourceMap::new(work.path(), None);
        let mut originals = Vec::new();
        for name in ["first", "second"] {
            let original = checkout.path().join(name);
            fs::create_dir(&original).unwrap();
            let staged = map.stage(&original).unwrap();
            fs::write(
                payload.path().join(format!("{name}.pth")),
                staged.to_string_lossy().as_bytes(),
            )
            .unwrap();
            originals.push((name, SourceMap::path(&original).unwrap()));
        }
        map.relocate(payload.path()).unwrap();
        drop(work);
        for (name, original) in originals {
            assert_eq!(
                fs::read_to_string(payload.path().join(format!("{name}.pth"))).unwrap(),
                original.to_string_lossy()
            );
            assert!(original.is_dir());
        }
    }

    #[test]
    fn source_preserves_checkout_and_declared_artifact_ownership() {
        let checkout = tempfile::tempdir().unwrap();
        fs::create_dir(checkout.path().join("build")).unwrap();
        fs::write(checkout.path().join("build/valuable"), "keep").unwrap();
        fs::create_dir(checkout.path().join("example.egg-info")).unwrap();
        fs::write(checkout.path().join("example.egg-info/metadata"), "keep").unwrap();
        let work = tempfile::tempdir().unwrap();
        let copy = source(checkout.path(), work.path(), None).unwrap();
        assert_eq!(
            fs::read_to_string(copy.join("build/valuable")).unwrap(),
            "keep"
        );
        assert!(copy.join("example.egg-info/metadata").is_file());
        fs::write(copy.join("build/valuable"), "changed").unwrap();
        assert_eq!(
            fs::read_to_string(checkout.path().join("build/valuable")).unwrap(),
            "keep"
        );
        let work = tempfile::tempdir().unwrap();
        let copy = source(
            checkout.path(),
            work.path(),
            Some(&checkout.path().join("build")),
        )
        .unwrap();
        assert!(!copy.join("build").exists());
        assert!(checkout.path().join("build/valuable").is_file());
        fs::create_dir(checkout.path().join("build/variant")).unwrap();
        fs::write(checkout.path().join("build/variant/generated"), "omit").unwrap();
        let work = tempfile::tempdir().unwrap();
        let copy = source(
            checkout.path(),
            work.path(),
            Some(&checkout.path().join("build/variant")),
        )
        .unwrap();
        assert!(!copy.join("build/variant").exists());
        assert!(copy.join("build/valuable").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn editable_relocation_normalizes_mixed_native_and_verbatim_paths() {
        let work = tempfile::tempdir().unwrap();
        let native = work.path().join("sources").join("volume-0").join("project");
        fs::create_dir_all(&native).unwrap();
        let mixed = work.path().join("sources/volume-0").join("project");
        let checkout = tempfile::tempdir().unwrap();
        let original = SourceMap::path(checkout.path()).unwrap();
        let variants = [
            native.to_string_lossy().into_owned(),
            mixed.to_string_lossy().into_owned(),
            native.to_string_lossy().replace('\\', "/"),
            fs::canonicalize(&native)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        for variant in variants {
            let payload = tempfile::tempdir().unwrap();
            fs::write(payload.path().join("editable.pth"), &variant).unwrap();
            fs::write(
                payload.path().join("__editable__finder.py"),
                format!("MAPPING = {}", serde_json::to_string(&variant).unwrap()),
            )
            .unwrap();
            let metadata = payload.path().join("fixture.dist-info");
            fs::create_dir(&metadata).unwrap();
            let uri = url::Url::from_directory_path(&native).unwrap();
            fs::write(metadata.join("direct_url.json"), serde_json::to_vec(
                &serde_json::json!({"url": uri.as_str().trim_end_matches('/'), "source_path": variant})
            ).unwrap()).unwrap();
            relocate(payload.path(), &mixed, checkout.path()).unwrap();
            let raw = fs::read_to_string(payload.path().join("editable.pth")).unwrap();
            assert_eq!(crate::util::path_key(Path::new(&raw)), original);
            let finder = fs::read_to_string(payload.path().join("__editable__finder.py")).unwrap();
            let quoted: String =
                serde_json::from_str(finder.strip_prefix("MAPPING = ").unwrap()).unwrap();
            assert_eq!(crate::util::path_key(Path::new(&quoted)), original);
            let record: serde_json::Value =
                serde_json::from_slice(&fs::read(metadata.join("direct_url.json")).unwrap())
                    .unwrap();
            assert_eq!(
                crate::util::path_key(Path::new(record["source_path"].as_str().unwrap())),
                original
            );
            let expected = url::Url::from_directory_path(&original).unwrap();
            assert_eq!(
                record["url"].as_str().unwrap(),
                expected.as_str().trim_end_matches('/')
            );
            assert!(!finder.contains("volume-0"));
        }
    }

    #[test]
    fn editable_relocation_preserves_user_modules_and_quoted_metadata() {
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join("stage's");
        let original = root.path().join("original's");
        fs::create_dir(&staged).unwrap();
        fs::create_dir(&original).unwrap();
        let payload = tempfile::tempdir().unwrap();
        fs::write(
            payload.path().join("editable.pth"),
            staged.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let finder = format!(
            "MAPPING = {{'module': {}}}",
            serde_json::to_string(&staged.to_string_lossy()).unwrap()
        );
        fs::write(
            payload.path().join("__editable__example_finder.py"),
            &finder,
        )
        .unwrap();
        fs::write(payload.path().join("user.py"), &finder).unwrap();
        let metadata = payload.path().join("example.dist-info");
        fs::create_dir(&metadata).unwrap();
        let uri = url::Url::from_directory_path(&staged).unwrap();
        fs::write(
            metadata.join("direct_url.json"),
            serde_json::to_vec(&serde_json::json!({"url": uri.as_str().trim_end_matches('/')}))
                .unwrap(),
        )
        .unwrap();
        relocate(payload.path(), &staged, &original).unwrap();
        assert_eq!(
            fs::read_to_string(payload.path().join("user.py")).unwrap(),
            finder
        );
        let edited =
            fs::read_to_string(payload.path().join("__editable__example_finder.py")).unwrap();
        assert!(edited.contains(r"original\'s"));
        assert!(!edited.contains("stage"));
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(metadata.join("direct_url.json")).unwrap()).unwrap();
        assert!(metadata["url"].as_str().unwrap().contains("original"));
        assert!(!metadata["url"].as_str().unwrap().contains("stage"));
        assert!(!fs::read_to_string(payload.path().join("editable.pth"))
            .unwrap()
            .contains("stage"));
    }

    #[test]
    fn relocation_preserves_interpreter_and_binary_overlay() {
        let header = b"#!/selected/python\r\nprint('ok')";
        assert!(relocate_shebang(header, Path::new("/stage"), Path::new("/final")).is_none());
        assert_eq!(
            relocate_shebang(
                b"#!/stage/python\r\nscript",
                Path::new("/stage"),
                Path::new("/final")
            )
            .unwrap(),
            b"#!/final/python\r\nscript"
        );
        assert_eq!(
            relocate_shebang(
                b"MZlauncher#!/stage/python\nPK\x03\x04overlay",
                Path::new("/stage"),
                Path::new("/final")
            )
            .unwrap(),
            b"MZlauncher#!/final/python\nPK\x03\x04overlay"
        );
        assert_eq!(
            replace_windows_shebang(b"#!C:/python.exe").unwrap(),
            b"#!python.exe   "
        );
    }
}
