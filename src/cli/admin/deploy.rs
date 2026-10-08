//! Native, recoverable installation of the CLI and its copied entry points.
//!
//! Ownership and recovery formats are shared with the legacy Python installer.
use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const MARKER: &str = ".rez-rs-cli.json";
const PRIMARY: &[&str] = &["rez", "rez.exe", "rez-rs.zip"];
type Hashes = BTreeMap<String, String>;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// Destination directory; defaults to the running executable's directory.
    #[arg(long)]
    pub bin_dir: Option<PathBuf>,
    /// Install the source archive beside the CLI.
    #[arg(long, conflicts_with = "recover_only")]
    pub source_zip: Option<PathBuf>,
    /// Refresh aliases without installing the primary executable.
    #[arg(long, conflicts_with = "recover_only")]
    pub aliases_only: bool,
    /// Validate ownership and report the proposed files without modifying anything.
    #[arg(long, conflicts_with = "recover_only")]
    pub dry_run: bool,
    /// Restore an interrupted activation without requiring incoming payloads.
    #[arg(long)]
    pub recover_only: bool,
}

#[derive(Serialize, Deserialize)]
struct Ownership {
    schema: u32,
    hashes: Hashes,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    schema: u32,
    previous: Hashes,
    incoming: Hashes,
}

fn safe_name(name: &str) -> bool {
    PRIMARY.contains(&name) || name == MARKER || {
        let stem = name.strip_suffix(".exe").unwrap_or(name);
        matches!(stem, "rezolve" | "_rez-complete" | "_rez_fwd")
            || stem.strip_prefix("rez-").is_some_and(|suffix| {
                suffix
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                    && suffix
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
    }
}

fn inspect(path: &Path, directory: bool) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    #[cfg(windows)]
    let redirected = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let redirected = metadata.file_type().is_symlink();
    if redirected || (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(format!(
            "Refusing redirected or non-regular installation path: {}",
            path.display()
        )
        .into());
    }
    Ok(true)
}

fn hash(path: &Path) -> Result<String> {
    if !inspect(path, false)? {
        return Err(format!("Missing installation file: {}", path.display()).into());
    }
    let mut source = File::open(path)?;
    Ok(foundation::util::hash_reader_hex::<Sha256>(&mut source)?)
}

fn hashes(directory: &Path, names: impl IntoIterator<Item = String>) -> Result<Hashes> {
    names
        .into_iter()
        .map(|name| {
            if !safe_name(&name) {
                return Err(format!("Unsafe installation filename: {name}").into());
            }
            Ok((name.clone(), hash(&directory.join(name))?))
        })
        .collect()
}

fn snapshot(directory: &Path, names: &BTreeSet<String>) -> Result<Hashes> {
    let existing = names
        .iter()
        .filter_map(|name| match inspect(&directory.join(name), false) {
            Ok(true) => Some(Ok(name.clone())),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    hashes(directory, existing)
}

fn validate(hashes: &Hashes) -> Result<()> {
    if hashes.iter().any(|(name, digest)| {
        !safe_name(name)
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err("Unsafe installation filenames or hashes".into());
    }
    Ok(())
}

fn identity(previous: &Hashes) -> Result<String> {
    // Python's json.dumps(..., sort_keys=True) uses spaces after separators.
    // Keep its exact bytes so old activation journals use the same backup.
    let entries = previous
        .iter()
        .map(|(name, digest)| {
            Ok(format!(
                "{}: {}",
                serde_json::to_string(name)?,
                serde_json::to_string(digest)?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(foundation::util::hex_encode(Sha256::digest(
        format!("{{{}}}", entries.join(", ")).as_bytes(),
    )))
}

fn replace(source: &Path, target: &Path, expected: &str) -> Result<()> {
    inspect(target, false)?;
    let parent = target.parent().ok_or("Installation target has no parent")?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    fs::copy(source, temporary.path())?;
    if hash(temporary.path())? != expected {
        return Err(format!("Incoming installation file changed: {}", source.display()).into());
    }
    temporary.as_file().sync_all()?;
    temporary.persist(target)?;
    Ok(())
}

fn restore(directory: &Path, transaction: &Path, backups: &Path, record: &Journal) -> Result<()> {
    validate(&record.previous)?;
    validate(&record.incoming)?;
    let names: BTreeSet<_> = record
        .previous
        .keys()
        .chain(record.incoming.keys())
        .cloned()
        .collect();
    if record.schema == 2 {
        if !record.incoming.contains_key(MARKER) {
            return Err("Missing native alias ownership manifest".into());
        }
    } else {
        let legacy: BTreeSet<_> = ["rez.exe".to_owned(), "rez-rs.zip".to_owned()].into();
        if !record.previous.keys().all(|name| legacy.contains(name))
            || record.incoming.keys().cloned().collect::<BTreeSet<_>>() != legacy
        {
            return Err("Invalid legacy activation recovery filenames".into());
        }
    }
    let backup = backups.join(identity(&record.previous)?);
    if !inspect(&backup, true)?
        || hashes(&backup, record.previous.keys().cloned())? != record.previous
    {
        return Err(format!(
            "Activation originals missing or corrupted: {}",
            backup.display()
        )
        .into());
    }
    // Validate the entire target before restoring anything; never overwrite an independent edit.
    for name in &names {
        let target = directory.join(name);
        if inspect(&target, false)? {
            let current = hash(&target)?;
            if record.previous.get(name) != Some(&current)
                && record.incoming.get(name) != Some(&current)
            {
                return Err(format!(
                    "Activation target changed independently: {}",
                    target.display()
                )
                .into());
            }
        }
    }
    for name in names {
        let target = directory.join(&name);
        if let Some(expected) = record.previous.get(&name) {
            if !inspect(&target, false)? || hash(&target)? != *expected {
                replace(&backup.join(&name), &target, expected)?;
            }
        } else if inspect(&target, false)? {
            fs::remove_file(target)?;
        }
    }
    if hashes(directory, record.previous.keys().cloned())? != record.previous {
        return Err("Activation rollback hash verification failed".into());
    }
    fs::remove_dir_all(transaction)?;
    Ok(())
}

pub fn run(args: &DeployArgs, command: &clap::Command) -> foundation::errors::Result<()> {
    install(args, command).map_err(|error| foundation::errors::RezError::System(error.to_string()))
}

fn install(args: &DeployArgs, command: &clap::Command) -> Result<()> {
    let executable = std::env::current_exe()?;
    let directory = std::path::absolute(
        args.bin_dir.as_deref().unwrap_or(
            executable
                .parent()
                .ok_or("Running executable has no parent")?,
        ),
    )?;
    let transaction = directory.join(".rez-rs-activation");
    let backups = directory.join(".rez-rs-backups");
    if args.dry_run && inspect(&transaction, true)? {
        return Err("An activation is pending; run deploy --recover-only first".into());
    }
    if !inspect(&directory, true)? {
        if args.recover_only {
            println!("{}", directory.display());
            return Ok(());
        }
        if !args.dry_run {
            fs::create_dir_all(&directory)?;
            inspect(&directory, true)?;
        }
    }
    // The persistent lock file is shared with cli_install.py. A dry run never creates it.
    let lock_path = directory.join(".rez-rs-install.lock");
    let lock = if args.dry_run {
        None
    } else {
        inspect(&lock_path, false)?;
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        if lock.metadata()?.len() == 0 {
            lock.write_all(&[0])?;
            lock.flush()?;
        }
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(format!("Another installation owns {}", directory.display()).into());
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(format!(
                    "Cannot lock installation at {}: {error}",
                    directory.display()
                )
                .into());
            }
        }
        Some(lock)
    };
    for state in [&transaction, &backups] {
        inspect(state, true)?;
    }
    let expected_executable = if args.recover_only {
        None
    } else {
        Some(hash(&executable)?)
    };
    if inspect(&transaction, true)? {
        let journal = transaction.join("journal.json");
        inspect(&journal, false)?;
        let record: Journal = serde_json::from_reader(File::open(journal)?)?;
        restore(&directory, &transaction, &backups, &record)?;
    }
    if args.recover_only {
        println!("{}", directory.display());
        return Ok(());
    }
    let executable_hash = hash(&executable)?;
    if expected_executable.as_ref() != Some(&executable_hash) {
        return Err("Incoming executable changed during activation recovery; retry with the external incoming executable".into());
    }
    let primary = if cfg!(windows) { "rez.exe" } else { "rez" };
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let alias_names: BTreeSet<_> = crate::cli::aliases(command)
        .keys()
        .map(|name| format!("{name}{suffix}"))
        .collect();
    let mut sources: BTreeMap<String, PathBuf> = alias_names
        .iter()
        .map(|name| (name.clone(), executable.clone()))
        .collect();
    if !args.aliases_only {
        sources.insert(primary.into(), executable.clone());
    }
    if let Some(zip) = &args.source_zip {
        sources.insert("rez-rs.zip".into(), std::path::absolute(zip)?);
    }
    let mut incoming = Hashes::new();
    for (name, source) in &sources {
        incoming.insert(
            name.clone(),
            if source == &executable {
                executable_hash.clone()
            } else {
                hash(source)?
            },
        );
    }
    let marker = directory.join(MARKER);
    let mut owned = Hashes::new();
    if inspect(&marker, false)? {
        let ownership: Ownership = serde_json::from_reader(File::open(&marker)?)?;
        validate(&ownership.hashes)?;
        if ownership.schema != 1 || ownership.hashes.contains_key(MARKER) {
            return Err("Unsafe native alias ownership manifest".into());
        }
        owned = ownership.hashes;
        for (name, expected) in &owned {
            let actual = hash(&directory.join(name))?;
            // An explicitly incoming primary may already have been manually copied in place.
            // This exception never permits changed aliases or a changed source archive.
            if actual != *expected
                && !(name == primary && sources.contains_key(name) && actual == executable_hash)
            {
                return Err(format!(
                    "Previously installed native CLI file changed independently: {name}"
                )
                .into());
            }
            if PRIMARY.contains(&name.as_str()) && !incoming.contains_key(name) {
                incoming.insert(name.clone(), actual);
            }
        }
    }
    for name in &alias_names {
        if inspect(&directory.join(name), false)? && !owned.contains_key(name) {
            return Err(format!("Refusing to replace a foreign CLI alias: {name}").into());
        }
    }
    let mut marker_bytes = serde_json::to_vec(&Ownership {
        schema: 1,
        hashes: incoming.clone(),
    })?;
    marker_bytes.push(b'\n');
    incoming.insert(
        MARKER.into(),
        foundation::util::hex_encode(Sha256::digest(&marker_bytes)),
    );
    let names: BTreeSet<_> = owned.keys().chain(incoming.keys()).cloned().collect();
    let previous = snapshot(&directory, &names)?;
    if args.dry_run {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "directory": directory, "aliases": alias_names, "hashes": incoming,
                "changed": previous != incoming
            }))?
        );
        return Ok(());
    }
    if previous == incoming {
        println!("{}", directory.display());
        return Ok(());
    }
    if !inspect(&backups, true)? {
        fs::create_dir(&backups)?;
    }
    let backup = backups.join(identity(&previous)?);
    if inspect(&backup, true)? {
        let entries = fs::read_dir(&backup)?
            .map(|entry| {
                Ok(entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| "Invalid backup filename")?)
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if entries != previous.keys().cloned().collect()
            || hashes(&backup, previous.keys().cloned())? != previous
        {
            return Err(format!("Conflicting activation backup: {}", backup.display()).into());
        }
    } else {
        let staging = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(&backups)?;
        for name in previous.keys() {
            fs::copy(directory.join(name), staging.path().join(name))?;
        }
        if hashes(staging.path(), previous.keys().cloned())? != previous {
            return Err("Activation originals changed during backup".into());
        }
        fs::rename(staging.path(), &backup)?;
    }
    let record = Journal {
        schema: 2,
        previous,
        incoming,
    };
    let pending = tempfile::Builder::new()
        .prefix(".rez-rs-activation.staging-")
        .tempdir_in(&directory)?;
    let mut journal = File::create(pending.path().join("journal.json"))?;
    serde_json::to_writer(&mut journal, &record)?;
    journal.sync_all()?;
    drop(journal);
    fs::rename(pending.path(), &transaction)?;
    let activated = (|| -> Result<()> {
        for (name, source) in &sources {
            // Never attempt to overwrite the currently running Windows executable if unchanged.
            if record.previous.get(name) != record.incoming.get(name) {
                fs::copy(source, transaction.join(name))?;
            }
        }
        let mut file = File::create(transaction.join(MARKER))?;
        file.write_all(&marker_bytes)?;
        file.sync_all()?;
        drop(file);
        if snapshot(&directory, &names)? != record.previous {
            return Err("Activation originals changed before commit".into());
        }
        for (name, expected) in record
            .incoming
            .iter()
            .filter(|(name, _)| name.as_str() != MARKER)
        {
            if record.previous.get(name) != Some(expected) {
                replace(&transaction.join(name), &directory.join(name), expected)?;
            }
        }
        // Publish ownership last, only after every executable/source payload is ready.
        replace(&transaction.join(MARKER), &marker, &record.incoming[MARKER])?;
        for name in record
            .previous
            .keys()
            .filter(|name| !record.incoming.contains_key(*name))
        {
            fs::remove_file(directory.join(name))?;
        }
        if hashes(&directory, record.incoming.keys().cloned())? != record.incoming {
            return Err("Activated CLI/source/alias hash verification failed".into());
        }
        Ok(())
    })();
    if let Err(error) = activated {
        if let Err(recovery) = restore(&directory, &transaction, &backups, &record) {
            return Err(format!(
                "Activation failed ({error}); recovery pending at {}: {recovery}",
                transaction.display()
            )
            .into());
        }
        return Err(format!("Activation failed and originals restored: {error}").into());
    }
    fs::remove_dir_all(&transaction)?;
    drop(lock); // OS ownership ends; retain the rendezvous file.
    println!("{}", directory.display());
    Ok(())
}
