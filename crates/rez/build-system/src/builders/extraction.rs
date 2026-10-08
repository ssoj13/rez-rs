// SPDX-License-Identifier: Apache-2.0

//! Extraction build system — extract zip, tar, tar.gz, tgz, tar.xz, msi (Windows).
//!
//! Config from package.config.extraction.downloads or sources.yaml.
//! Each source: url or path, optional file_name and checksum { algorithm, hash }.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use percent_encoding::percent_decode_str;
use tar::Archive;
use url::Url;
use zip::ZipArchive;

use super::{BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::{Result, RezError};

/// One extraction source: local path or URL (URL download optional).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExtractionSource {
    /// URL (http/https) or file path
    pub url: Option<String>,
    /// Local file path (relative to package or absolute)
    pub path: Option<String>,
    /// Optional output file name when downloading
    pub file_name: Option<String>,
    /// Optional checksum, verified for both local files and downloaded archives.
    pub checksum: Option<super::download::ChecksumSpec>,
}

impl ExtractionSource {
    fn download_file_name(&self, source_url: &str) -> Result<String> {
        let file_name = if let Some(file_name) = self.file_name.as_deref() {
            file_name.to_owned()
        } else {
            let parsed = Url::parse(source_url).map_err(|error| {
                RezError::BuildSystem(format!("Invalid download URL '{}': {}", source_url, error))
            })?;
            let segment = parsed
                .path_segments()
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .ok_or_else(|| {
                    RezError::BuildSystem(format!(
                        "Cannot derive a download filename from '{}'; set file_name",
                        source_url
                    ))
                })?;
            percent_decode_str(segment)
                .decode_utf8()
                .map_err(|error| {
                    RezError::BuildSystem(format!(
                        "Download URL '{}' has an invalid encoded filename: {}",
                        source_url, error
                    ))
                })?
                .into_owned()
        };

        super::download::validate_cache_file_name(&file_name)?;
        Ok(file_name)
    }

    /// Resolve to a local file path; downloads URL sources to cache if needed.
    pub fn resolve_path(&self, source_dir: &Path) -> Result<Option<PathBuf>> {
        crate::config::ensure_valid()?;
        self.resolve_path_with_config(source_dir, &crate::config::CONFIG)
    }

    fn resolve_path_with_config(
        &self,
        source_dir: &Path,
        config: &crate::config::RezConfig,
    ) -> Result<Option<PathBuf>> {
        if let Some(checksum) = &self.checksum {
            checksum.validate()?;
        }
        if let Some(path) = self.path.as_deref() {
            let path = source_dir.join(path);
            match fs::metadata(&path) {
                Ok(metadata) => {
                    if !metadata.is_file() {
                        return Err(RezError::BuildSystem(format!(
                            "Extraction source is not a regular file: {}",
                            path.display()
                        )));
                    }
                    if let Some(checksum) = self.checksum.as_ref() {
                        if !super::download::verify_checksum(
                            &path,
                            &checksum.algorithm,
                            &checksum.hash,
                            true,
                        )? {
                            return Err(RezError::BuildSystem(format!(
                                "Checksum mismatch for extraction source {} ({})",
                                path.display(),
                                checksum.algorithm
                            )));
                        }
                    }
                    return Ok(Some(path));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(RezError::BuildSystem(format!(
                        "Cannot inspect extraction source {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        if let Some(ref url) = self.url {
            let file_name = self.download_file_name(url)?;
            if let Some(mirror) = &config.sources_path {
                let path = crate::config::RezConfig::expand_path(mirror)
                    .to_os()
                    .join(&file_name);
                if path.is_file() {
                    if let Some(checksum) = &self.checksum {
                        if !super::download::verify_checksum(
                            &path,
                            &checksum.algorithm,
                            &checksum.hash,
                            true,
                        )? {
                            return Err(RezError::BuildSystem(format!(
                                "Checksum mismatch for source mirror {}",
                                path.display()
                            )));
                        }
                    }
                    return Ok(Some(path));
                }
            }
            let cache_dir = config
                .user_path
                .as_ref()
                .map(|path| {
                    crate::config::RezConfig::expand_path(path)
                        .to_os()
                        .join("cache/downloads")
                })
                .unwrap_or_else(|| std::env::temp_dir().join("rez_build_cache"));
            let path = if config.offline {
                super::download::download_to_cache_with_policy(
                    url,
                    &cache_dir,
                    &file_name,
                    self.checksum.as_ref(),
                    None,
                    true,
                )
            } else {
                super::download::download_to_cache(
                    url,
                    &cache_dir,
                    &file_name,
                    self.checksum.as_ref(),
                    None,
                )
            };
            return path.map(Some);
        }
        Ok(None)
    }
}

fn parse_sources(entries: &[serde_json::Value]) -> Result<Vec<ExtractionSource>> {
    entries
        .iter()
        .enumerate()
        .map(|(index, item)| {
            if !item.is_object() {
                return Err(RezError::BuildSystem(format!(
                    "Extraction source at index {index} must be a mapping"
                )));
            }
            let source: ExtractionSource =
                serde_json::from_value(item.clone()).map_err(|error| {
                    RezError::BuildSystem(format!(
                        "Invalid extraction source at index {index}: {error}"
                    ))
                })?;
            if source
                .url
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
                && source
                    .path
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
            {
                return Err(RezError::BuildSystem(format!(
                    "Extraction source at index {index} requires a non-empty url or path"
                )));
            }
            if let Some(file_name) = source.file_name.as_deref() {
                super::download::validate_cache_file_name(file_name)?;
            }
            if let Some(checksum) = &source.checksum {
                checksum.validate()?;
            }
            Ok(source)
        })
        .collect()
}

/// Extraction build system — extracts archives to install path.
#[derive(Debug, Clone)]
pub struct ExtractionBuildSystem {
    pub working_dir: PathBuf,
}

impl ExtractionBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }

    /// Check if directory has extraction config (sources.yaml or package signals extraction).
    pub fn is_valid_root(path: &Path) -> bool {
        path.join("sources.yaml").exists() || path.join("sources.yml").exists()
    }

    /// Load sources from package config or sources.yaml, preserving invalid-input errors.
    pub fn load_sources(
        working_dir: &Path,
        config: Option<&serde_json::Value>,
    ) -> Result<Vec<ExtractionSource>> {
        if let Some(ext) = config
            .and_then(|config| config.get("extraction"))
            .filter(|value| !value.is_null())
        {
            if !ext.is_object() {
                return Err(RezError::BuildSystem(
                    "config.extraction must be a mapping".into(),
                ));
            }
            if let Some(downloads) = ext.get("downloads").filter(|value| !value.is_null()) {
                let entries = downloads.as_array().ok_or_else(|| {
                    RezError::BuildSystem("config.extraction.downloads must be an array".into())
                })?;
                let sources = parse_sources(entries)?;
                if !sources.is_empty() {
                    return Ok(sources);
                }
            }
        }

        for name in ["sources.yaml", "sources.yml"] {
            let path = working_dir.join(name);
            let content = match fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(RezError::BuildSystem(format!(
                        "Cannot read extraction sources {}: {error}",
                        path.display()
                    )));
                }
            };
            let data: serde_json::Value = serde_yaml::from_str(&content).map_err(|error| {
                RezError::BuildSystem(format!(
                    "Invalid extraction sources {}: {error}",
                    path.display()
                ))
            })?;
            let entries = data
                .get("downloads")
                .or_else(|| data.get("sources"))
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    RezError::BuildSystem(format!(
                        "Extraction sources {} must contain a downloads or sources array",
                        path.display()
                    ))
                })?;
            let sources = parse_sources(entries).map_err(|error| {
                RezError::BuildSystem(format!("Extraction sources {}: {error}", path.display()))
            })?;
            if !sources.is_empty() {
                return Ok(sources);
            }
        }
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarXz,
    Msi,
}

impl ArchiveFormat {
    fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "zip" => Some(Self::Zip),
            "msi" => Some(Self::Msi),
            "tar" => Some(Self::Tar),
            "gz" | "tgz" => Some(Self::TarGz),
            "xz" => Some(Self::TarXz),
            _ => path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| stem.to_ascii_lowercase().ends_with(".tar"))
                .map(|_| Self::Tar),
        }
    }
}

/// Extract zip archive to dest.
fn extract_zip(src: &Path, dest: &Path) -> Result<()> {
    let file = File::open(src).map_err(|error| {
        RezError::BuildSystem(format!("Cannot open zip {}: {}", src.display(), error))
    })?;
    let mut archive = ZipArchive::new(BufReader::new(file)).map_err(|error| {
        RezError::BuildSystem(format!("Invalid zip {}: {}", src.display(), error))
    })?;
    archive.extract(dest).map_err(|error| {
        RezError::BuildSystem(format!(
            "Cannot safely extract zip {}: {}",
            src.display(),
            error
        ))
    })
}

/// Extract tar (possibly compressed) to dest.
fn extract_tar_inner<R: Read>(reader: R, dest: &Path) -> Result<()> {
    let mut archive = Archive::new(reader);
    archive
        .unpack(dest)
        .map_err(|e| RezError::BuildSystem(format!("Tar extract failed: {}", e)))?;
    Ok(())
}

fn extract_tar(src: &Path, dest: &Path, format: ArchiveFormat) -> Result<()> {
    let file = File::open(src).map_err(|error| {
        RezError::BuildSystem(format!("Cannot open tar {}: {}", src.display(), error))
    })?;

    match format {
        ArchiveFormat::Tar => extract_tar_inner(BufReader::new(file), dest),
        ArchiveFormat::TarGz => extract_tar_inner(GzDecoder::new(BufReader::new(file)), dest),
        ArchiveFormat::TarXz => {
            extract_tar_inner(liblzma::read::XzDecoder::new(BufReader::new(file)), dest)
        }
        _ => Err(RezError::BuildSystem(format!(
            "Archive format {:?} is not a tar format",
            format
        ))),
    }
}

/// Merge extraction result into install path. If extracted dir has single subdir, use that.
fn merge_to_install(extracted: &Path, install_path: &Path) -> Result<()> {
    let entries: Vec<_> = fs::read_dir(extracted)
        .map_err(|e| {
            RezError::BuildSystem(format!(
                "Cannot read extracted {}: {}",
                extracted.display(),
                e
            ))
        })?
        .collect();

    if entries.len() == 1 {
        let single = entries
            .into_iter()
            .next()
            .expect("len()==1 but no entries")?
            .path();
        if fs::symlink_metadata(&single)?.file_type().is_dir() {
            foundation::filesystem::copy_dir_contents(
                &single,
                install_path,
                false,
                true,
                Some(install_path),
                None,
            )?;
            return Ok(());
        }
    }

    foundation::filesystem::copy_dir_contents(
        extracted,
        install_path,
        false,
        true,
        Some(install_path),
        None,
    )?;
    Ok(())
}

impl BuildSystem for ExtractionBuildSystem {
    fn name(&self) -> &str {
        "extraction"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Extraction
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = std::time::Instant::now();

        let source_dir = &ctx.source_path;
        let install_path = &ctx.install_path;

        if !ctx.install {
            return Ok(BuildResult::ok(
                ctx.build_path.clone(),
                start.elapsed().as_secs_f64(),
            ));
        }

        let sources = Self::load_sources(source_dir, ctx.package_config.as_ref())?;

        let prepared =
            std::sync::Arc::new(tempfile::tempdir_in(&ctx.build_path).map_err(|error| {
                RezError::BuildSystem(format!("Cannot stage extracted payload: {error}"))
            })?);

        let mut any_extracted = false;

        for src_cfg in &sources {
            let Some(archive_path) = src_cfg.resolve_path(source_dir)? else {
                continue;
            };

            let fmt = ArchiveFormat::from_path(&archive_path).ok_or_else(|| {
                RezError::BuildSystem(format!(
                    "Unknown archive format: {}",
                    archive_path.display()
                ))
            })?;

            let staging = tempfile::tempdir_in(&ctx.build_path)
                .map_err(|e| RezError::BuildSystem(format!("Cannot create temp dir: {}", e)))?;
            let staging_path = staging.path();

            match fmt {
                ArchiveFormat::Zip => extract_zip(&archive_path, staging_path)?,
                ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarXz => {
                    extract_tar(&archive_path, staging_path, fmt)?;
                }
                ArchiveFormat::Msi => {
                    if cfg!(windows) {
                        extract_msi(&archive_path, staging_path)?;
                    } else {
                        return Err(RezError::BuildSystem(
                            "MSI extraction is only supported on Windows".into(),
                        ));
                    }
                }
            }

            merge_to_install(staging_path, prepared.path())?;
            any_extracted = true;
        }

        if !any_extracted && sources.is_empty() {
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                start.elapsed().as_secs_f64(),
                "No extraction sources configured (sources.yaml or config.extraction.downloads)"
                    .into(),
            ));
        }

        if !any_extracted {
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                start.elapsed().as_secs_f64(),
                "No archive paths resolved. Check source URLs and local paths.".into(),
            ));
        }

        let mut result = BuildResult::ok(ctx.build_path.clone(), start.elapsed().as_secs_f64());
        result.install_path = Some(install_path.clone());
        result.prepared_payload = Some(prepared);
        Ok(result)
    }
}

#[cfg(windows)]
fn extract_msi(src: &Path, dest: &Path) -> Result<()> {
    use std::process::Command;

    let status = Command::new("cmd.exe")
        .args([
            "/c",
            "msiexec",
            "/a",
            &src.to_string_lossy(),
            "/qn",
            "/norestart",
            &format!("TARGETDIR={}", dest.display()),
        ])
        .status()
        .map_err(|e| RezError::BuildSystem(format!("msiexec failed: {}", e)))?;

    if !status.success() {
        return Err(RezError::BuildSystem(format!(
            "msiexec exited with {:?}",
            status.code()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn extract_msi(_src: &Path, _dest: &Path) -> Result<()> {
    unreachable!("MSI only on Windows")
}

#[cfg(test)]
mod tests {
    use super::{parse_sources, ArchiveFormat, ExtractionSource};
    use std::io::Write;

    #[test]
    fn offline_extraction_uses_mirror_and_rejects_network_miss() {
        let root = tempfile::tempdir().unwrap();
        let mirror = root.path().join("mirror");
        std::fs::create_dir(&mirror).unwrap();
        let source: ExtractionSource = serde_json::from_value(serde_json::json!({
            "url": "https://example.invalid/package.zip"
        }))
        .unwrap();
        let config = crate::config::RezConfig {
            sources_path: Some(mirror.to_string_lossy().into_owned()),
            user_path: Some(root.path().join("user").to_string_lossy().into_owned()),
            offline: true,
            ..crate::config::RezConfig::default()
        };
        let error = source
            .resolve_path_with_config(root.path(), &config)
            .unwrap_err();
        assert!(error.to_string().contains("REZ_OFFLINE=true"), "{error}");
        let local = mirror.join("package.zip");
        std::fs::write(&local, b"local archive").unwrap();
        assert_eq!(
            source
                .resolve_path_with_config(root.path(), &config)
                .unwrap(),
            Some(local)
        );
    }

    const PAYLOAD: &[u8] = b"archive payload";

    fn tar_bytes() -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(PAYLOAD.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "package/payload.txt", PAYLOAD)
            .unwrap();
        archive.into_inner().unwrap()
    }

    fn zip_bytes() -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            archive
                .start_file("release..old/payload.txt", options)
                .unwrap();
            archive.write_all(PAYLOAD).unwrap();
            archive.start_file("../escape.txt", options).unwrap();
            archive.write_all(b"outside").unwrap();
            archive.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn archive_format_classifies_supported_extensions_from_one_source() {
        for (name, expected) in [
            ("archive.zip", ArchiveFormat::Zip),
            ("archive.msi", ArchiveFormat::Msi),
            ("archive.tar", ArchiveFormat::Tar),
            ("archive.tar.gz", ArchiveFormat::TarGz),
            ("archive.tgz", ArchiveFormat::TarGz),
            ("archive.tar.xz", ArchiveFormat::TarXz),
            ("archive.TGZ", ArchiveFormat::TarGz),
        ] {
            assert_eq!(
                ArchiveFormat::from_path(std::path::Path::new(name)),
                Some(expected)
            );
        }
        assert_eq!(
            ArchiveFormat::from_path(std::path::Path::new("archive.unknown")),
            None
        );
    }

    #[test]
    fn zip_extraction_preserves_dot_names_and_blocks_parent_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("unsafe.zip");
        let destination = temp.path().join("install");
        std::fs::write(&archive_path, zip_bytes()).unwrap();
        std::fs::create_dir_all(&destination).unwrap();

        let _ = super::extract_zip(&archive_path, &destination);

        assert_eq!(
            std::fs::read(destination.join("release..old/payload.txt")).unwrap(),
            PAYLOAD
        );
        assert!(!temp.path().join("escape.txt").exists());
    }

    #[test]
    fn tar_extracts_plain_gzip_tgz_and_xz_archives() {
        let raw_tar = tar_bytes();
        let temp = tempfile::tempdir().unwrap();

        for (name, format, archive_bytes) in [
            ("plain.tar", ArchiveFormat::Tar, raw_tar.clone()),
            ("gzip.tar.gz", ArchiveFormat::TarGz, gzip_bytes(&raw_tar)),
            ("gzip.tgz", ArchiveFormat::TarGz, gzip_bytes(&raw_tar)),
            ("xz.tar.xz", ArchiveFormat::TarXz, xz_bytes(&raw_tar)),
        ] {
            let archive_path = temp.path().join(name);
            let destination = temp.path().join(format!("{}-out", name));
            std::fs::write(&archive_path, archive_bytes).unwrap();
            std::fs::create_dir_all(&destination).unwrap();

            super::extract_tar(&archive_path, &destination, format).unwrap();

            assert_eq!(
                std::fs::read(destination.join("package/payload.txt")).unwrap(),
                PAYLOAD
            );
        }
    }

    fn gzip_bytes(input: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(input).unwrap();
        encoder.finish().unwrap()
    }

    fn xz_bytes(input: &[u8]) -> Vec<u8> {
        let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 0);
        encoder.write_all(input).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn extraction_sources_derive_safe_archive_names_from_url_paths() {
        let source = ExtractionSource {
            url: Some("https://example.test/files/archive.tar.gz?token=secret#download".into()),
            path: None,
            file_name: None,
            checksum: None,
        };
        assert_eq!(
            source
                .download_file_name(source.url.as_deref().unwrap())
                .unwrap(),
            "archive.tar.gz"
        );

        let encoded = ExtractionSource {
            url: Some("https://example.test/files/archive%2Etar.gz?token=secret".into()),
            path: None,
            file_name: None,
            checksum: None,
        };
        assert_eq!(
            encoded
                .download_file_name(encoded.url.as_deref().unwrap())
                .unwrap(),
            "archive.tar.gz"
        );

        let unsafe_name = ExtractionSource {
            url: Some("https://example.test/files/%2E%2E%2Foutside.tar.gz".into()),
            path: None,
            file_name: None,
            checksum: None,
        };
        assert!(unsafe_name
            .download_file_name(unsafe_name.url.as_deref().unwrap())
            .is_err());
    }

    #[test]
    fn extraction_sources_reuse_one_parser_for_url_and_local_paths() {
        let entries = [
            serde_json::json!({"url": "https://example.test/archive.tgz"}),
            serde_json::json!({"path": "archive.tgz"}),
        ];

        let sources = parse_sources(&entries).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(
            sources[0].url.as_deref(),
            Some("https://example.test/archive.tgz")
        );
        assert_eq!(sources[1].path.as_deref(), Some("archive.tgz"));
    }

    #[test]
    fn explicit_download_name_must_be_a_single_path_component() {
        let source = ExtractionSource {
            url: Some("https://example.test/archive.tar.gz".into()),
            path: None,
            file_name: Some("../outside.tar.gz".into()),
            checksum: None,
        };
        assert!(source
            .download_file_name(source.url.as_deref().unwrap())
            .is_err());
    }

    #[test]
    fn extraction_prepares_payload_without_mutating_live_installation() {
        use crate::builders::{BuildContext, BuildSystem};
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let build = owned.path().join("build");
        let install = owned.path().join("install");
        for path in [&source, &build, &install] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(source.join("payload.tar"), tar_bytes()).unwrap();
        std::fs::write(install.join("payload.txt"), b"original").unwrap();
        std::fs::write(
            source.join("sources.yaml"),
            "sources:\n  - path: payload.tar\n",
        )
        .unwrap();
        let mut context = BuildContext::new(source.clone(), build, install.clone());
        context.install = true;
        let result = super::ExtractionBuildSystem::new(source)
            .build(&context)
            .unwrap();
        assert!(result.success);
        let stage = result.prepared_payload.as_ref().unwrap();
        assert_eq!(
            std::fs::read(stage.path().join("payload.txt")).unwrap(),
            PAYLOAD
        );
        assert_eq!(
            std::fs::read(install.join("payload.txt")).unwrap(),
            b"original"
        );
        let stage_path = stage.path().to_path_buf();
        let cloned = result.clone();
        drop(result);
        assert!(stage_path.exists());
        drop(cloned);
        assert!(!stage_path.exists());
    }

    #[test]
    fn a_later_corrupt_archive_preserves_the_entire_live_payload() {
        use crate::builders::{BuildContext, BuildSystem};
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let build = owned.path().join("build");
        let install = owned.path().join("install");
        for path in [&source, &build, &install] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(source.join("first.tar"), tar_bytes()).unwrap();
        std::fs::write(source.join("broken.zip"), b"not a zip").unwrap();
        std::fs::write(
            source.join("sources.yaml"),
            "sources:\n  - path: first.tar\n  - path: broken.zip\n",
        )
        .unwrap();
        std::fs::write(install.join("payload.txt"), b"original").unwrap();
        let mut context = BuildContext::new(source.clone(), build.clone(), install.clone());
        context.install = true;
        let error = super::ExtractionBuildSystem::new(source)
            .build(&context)
            .unwrap_err();
        assert!(error.to_string().contains("Invalid zip"), "{error}");
        assert_eq!(
            std::fs::read(install.join("payload.txt")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(&build).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn extracted_directory_links_remain_opaque_during_merge() {
        let owned = tempfile::tempdir().unwrap();
        let extracted = owned.path().join("extracted");
        let destination = owned.path().join("destination");
        let foreign = owned.path().join("foreign");
        std::fs::create_dir_all(&extracted).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("valuable"), b"keep").unwrap();
        std::os::unix::fs::symlink(&foreign, extracted.join("link")).unwrap();
        super::merge_to_install(&extracted, &destination).unwrap();
        assert!(std::fs::symlink_metadata(destination.join("link"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_link(destination.join("link")).unwrap(),
            foreign
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_second_archive_cannot_write_through_an_earlier_archive_link() {
        use crate::builders::{BuildContext, BuildSystem};
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let build = owned.path().join("build");
        let install = owned.path().join("install");
        let foreign = owned.path().join("foreign");
        for path in [&source, &build, &install, &foreign] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(foreign.join("payload.txt"), b"valuable").unwrap();
        std::fs::write(install.join("installed.txt"), b"original").unwrap();
        let mut first = tar::Builder::new(Vec::new());
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        link.set_cksum();
        first
            .append_link(&mut link, "outer/link", &foreign)
            .unwrap();
        std::fs::write(source.join("first.tar"), first.into_inner().unwrap()).unwrap();
        let mut second = tar::Builder::new(Vec::new());
        for name in ["outer/link/payload.txt", "outer/other.txt"] {
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o644);
            header.set_cksum();
            second
                .append_data(&mut header, name, b"bad".as_slice())
                .unwrap();
        }
        std::fs::write(source.join("second.tar"), second.into_inner().unwrap()).unwrap();
        std::fs::write(
            source.join("sources.yaml"),
            "sources:\n  - path: first.tar\n  - path: second.tar\n",
        )
        .unwrap();
        let mut context = BuildContext::new(source.clone(), build.clone(), install.clone());
        context.install = true;
        let error = super::ExtractionBuildSystem::new(source)
            .build(&context)
            .unwrap_err();
        assert!(error.to_string().contains("regular directory"), "{error}");
        assert_eq!(
            std::fs::read(foreign.join("payload.txt")).unwrap(),
            b"valuable"
        );
        assert_eq!(
            std::fs::read(install.join("installed.txt")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(build).unwrap().count(), 0);
    }
}
