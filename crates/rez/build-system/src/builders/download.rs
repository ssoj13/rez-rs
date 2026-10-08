// SPDX-License-Identifier: Apache-2.0

//! HTTP downloads with URL-scoped caching, validated resume, and atomic publication.

use std::fs::{self, File, OpenOptions};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::errors::{Result, RezError};
use crate::util::{hash_reader_hex, open_file, FileLock};

/// Parsed checksum: algorithm (sha256/sha1) and expected hex hash.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChecksumSpec {
    pub algorithm: String,
    pub hash: String,
}

impl ChecksumSpec {
    pub(super) fn validate(&self) -> Result<()> {
        validate_checksum(&self.algorithm, &self.hash)
    }
}

fn validate_checksum(algorithm: &str, checksum: &str) -> Result<()> {
    let length = match algorithm.to_ascii_lowercase().as_str() {
        "sha256" => 64,
        "sha1" => 40,
        _ => {
            return Err(RezError::BuildSystem(format!(
                "Unsupported checksum algorithm: {algorithm} (use sha256 or sha1)"
            )))
        }
    };
    let checksum = checksum.trim();
    if checksum.len() != length || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RezError::BuildSystem(format!(
            "Invalid {algorithm} checksum: expected {length} hexadecimal digits"
        )));
    }
    Ok(())
}

/// Ensure a cache filename is one safe OS path component.
pub(super) fn validate_cache_file_name(file_name: &str) -> Result<()> {
    if !foundation::path::is_safe_rez_path_component(file_name, false) || file_name.contains('\0') {
        return Err(RezError::BuildSystem(format!(
            "Download filename must be a safe single path component: '{file_name}'"
        )));
    }
    Ok(())
}

/// Verify a file checksum; local distribution links may be explicitly followed.
pub fn verify_checksum(
    path: &Path,
    algorithm: &str,
    checksum: &str,
    follow_symlinks: bool,
) -> Result<bool> {
    validate_checksum(algorithm, checksum)?;
    let file = if follow_symlinks {
        File::open(path)?
    } else {
        open_file(path, OpenOptions::new().read(true))?
    };
    let mut file = BufReader::new(file);
    let actual = match algorithm.to_ascii_lowercase().as_str() {
        "sha256" => hash_reader_hex::<Sha256>(&mut file),
        "sha1" => hash_reader_hex::<Sha1>(&mut file),
        _ => unreachable!("checksum algorithm was validated"),
    }?;
    Ok(actual.eq_ignore_ascii_case(checksum.trim()))
}

/// Only generated regular files are permitted inside a download cache entry.
fn cache_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !crate::util::is_redirect(&metadata) => {
            Ok(Some(metadata))
        }
        Ok(_) => Err(RezError::BuildSystem(format!(
            "Download cache entry is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeMetadata {
    etag: Option<String>,
}

/// Download URL to its cache entry under one persistent OS lock.
///
/// Resume without a strong validator is allowed only when a configured checksum
/// will validate the complete representation. Otherwise an old partial restarts.
pub fn download_to_cache(
    url: &str,
    cache_root: &Path,
    file_name: &str,
    checksum: Option<&ChecksumSpec>,
    agent: Option<&ureq::Agent>,
) -> Result<PathBuf> {
    validate_cache_file_name(file_name)?;
    if let Some(spec) = checksum {
        spec.validate()?;
    }
    let url_hash = Sha256::digest(url.as_bytes());
    let cache_dir = crate::util::directory(
        cache_root,
        Path::new(&crate::util::hex_encode(url_hash)),
        true,
    )?;
    let payload_dir = crate::util::directory(&cache_dir, Path::new("payload"), true)?;
    let dest = payload_dir.join(file_name);
    let partial = control_path(&cache_dir, file_name, "part");
    let metadata_path = control_path(&cache_dir, file_name, "resume.json");
    let lock_path = control_path(&cache_dir, file_name, "lock");
    cache_metadata(&lock_path)?;
    let mut lock = FileLock::new(&lock_path);
    lock.acquire(Duration::from_secs(120))?;
    // Validate after acquiring the lock: another writer may have finished while waiting.
    for path in [&dest, &partial, &metadata_path] {
        cache_metadata(path)?;
    }
    if cache_metadata(&dest)?.is_some() {
        let valid = match checksum {
            Some(spec) => verify_checksum(&dest, &spec.algorithm, &spec.hash, false)?,
            None => true,
        };
        if valid {
            return Ok(dest);
        }
        fs::remove_file(&dest)?;
    }
    let mut metadata = if cache_metadata(&metadata_path)?.is_some() {
        let file = open_file(&metadata_path, OpenOptions::new().read(true))?;
        serde_json::from_reader::<_, ResumeMetadata>(file).map_err(|error| {
            RezError::BuildSystem(format!(
                "Invalid download resume metadata {}: {error}",
                metadata_path.display()
            ))
        })?
    } else {
        ResumeMetadata::default()
    };
    // Sidecars must not inject a weak/malformed validator into If-Range.
    if metadata
        .etag
        .as_ref()
        .is_some_and(|value| !strong_etag(value))
    {
        return Err(RezError::BuildSystem(format!(
            "Invalid strong ETag in {}",
            metadata_path.display()
        )));
    }
    let default_agent;
    let agent = match agent {
        Some(agent) => agent,
        None => {
            default_agent = ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(30)))
                .timeout_recv_response(Some(Duration::from_secs(60)))
                .timeout_recv_body(Some(Duration::from_secs(60)))
                .build()
                .into();
            &default_agent
        }
    };
    for attempt in 0..2 {
        let mut start = cache_metadata(&partial)?.map_or(0, |value| value.len());
        if start > 0 && metadata.etag.is_none() && checksum.is_none() {
            start = 0;
        }
        let mut request = agent.get(url).header("Accept-Encoding", "identity");
        if start > 0 {
            request = request.header("Range", &format!("bytes={start}-"));
            if let Some(etag) = &metadata.etag {
                request = request.header("If-Range", etag);
            }
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(416)) if start > 0 && attempt == 0 => {
                fs::remove_file(&partial)?;
                if cache_metadata(&metadata_path)?.is_some() {
                    fs::remove_file(&metadata_path)?;
                }
                metadata = ResumeMetadata::default();
                continue;
            }
            Err(error) => {
                return Err(RezError::BuildSystem(format!(
                    "HTTP GET {url} failed: {error}"
                )))
            }
        };
        let status = response.status().as_u16();
        if status == 416 && start > 0 && attempt == 0 {
            fs::remove_file(&partial)?;
            if cache_metadata(&metadata_path)?.is_some() {
                fs::remove_file(&metadata_path)?;
            }
            metadata = ResumeMetadata::default();
            continue;
        }
        if status != 200 && !(status == 206 && start > 0) {
            return Err(RezError::BuildSystem(format!(
                "Unexpected HTTP status {status} downloading {url}"
            )));
        }
        if response
            .headers()
            .get("Content-Encoding")
            .is_some_and(|value| {
                value
                    .to_str()
                    .map_or(true, |value| !value.eq_ignore_ascii_case("identity"))
            })
        {
            return Err(RezError::BuildSystem(format!(
                "Encoded HTTP representation cannot be cached by byte range: {url}"
            )));
        }
        let response_etag = response
            .headers()
            .get("ETag")
            .and_then(|value| value.to_str().ok())
            .filter(|value| strong_etag(value))
            .map(str::to_owned);
        let range = if status == 206 {
            let range = response
                .headers()
                .get("Content-Range")
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range)
                .filter(|(first, last, total)| {
                    *first == start && last >= first && total.is_none_or(|total| total > *last)
                })
                .ok_or_else(|| {
                    RezError::BuildSystem(format!(
                        "Invalid Content-Range downloading {url} from byte {start}"
                    ))
                })?;
            if metadata
                .etag
                .as_ref()
                .is_some_and(|etag| response_etag.as_ref() != Some(etag))
            {
                return Err(RezError::BuildSystem(format!(
                    "HTTP representation validator changed while resuming {url}"
                )));
            }
            Some(range)
        } else {
            start = 0; // Reuse the full response when Range/If-Range was ignored.
            None
        };
        let expected_size = response
            .headers()
            .get("Content-Length")
            .map(|value| {
                value
                    .to_str()
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or_else(|| {
                        RezError::BuildSystem(format!("Invalid Content-Length for {url}"))
                    })
            })
            .transpose()?;
        if let (Some((first, last, _)), Some(length)) = (range, expected_size) {
            if last.checked_sub(first).and_then(|size| size.checked_add(1)) != Some(length) {
                return Err(RezError::BuildSystem(format!(
                    "Content-Length does not match Content-Range for {url}"
                )));
            }
        }
        if status == 200 {
            metadata.etag = response_etag;
            crate::serialise::atomic_write(&metadata_path, serde_json::to_vec(&metadata)?)?;
        }
        let mut file = open_file(
            &partial,
            OpenOptions::new().write(true).create(true).truncate(false),
        )?;
        if start == 0 {
            file.set_len(0)?;
        }
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(start))?;
        let mut body = response.into_body();
        let downloaded = std::io::copy(&mut body.as_reader(), &mut file)?;
        file.sync_all()?;
        drop(file);
        if expected_size.is_some_and(|expected| expected != downloaded) {
            return Err(RezError::BuildSystem(format!(
                "Incomplete HTTP body for {url}"
            )));
        }
        let actual_total = start
            .checked_add(downloaded)
            .ok_or_else(|| RezError::BuildSystem(format!("Downloaded size overflow for {url}")))?;
        if let Some((_, end, total)) = range {
            if actual_total.checked_sub(1) != Some(end)
                || total.is_some_and(|total| total != actual_total)
                || (total.is_none() && checksum.is_none())
            {
                return Err(RezError::BuildSystem(format!(
                    "Incomplete resumed representation for {url}"
                )));
            }
        }
        if let Some(spec) = checksum {
            if !verify_checksum(&partial, &spec.algorithm, &spec.hash, false)? {
                fs::remove_file(&partial)?;
                fs::remove_file(&metadata_path).or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })?;
                return Err(RezError::BuildSystem(format!(
                    "Checksum mismatch for {url}"
                )));
            }
        }
        fs::rename(&partial, &dest)?;
        if cache_metadata(&metadata_path)?.is_some() {
            fs::remove_file(&metadata_path)?;
        }
        return Ok(dest);
    }
    Err(RezError::BuildSystem(format!(
        "Cannot restart rejected HTTP range for {url}"
    )))
}

fn control_path(directory: &Path, file_name: &str, suffix: &str) -> PathBuf {
    let key = crate::util::hex_encode(Sha256::digest(file_name.as_bytes()));
    directory.join(format!(".{key}.{suffix}"))
}

fn strong_etag(value: &str) -> bool {
    value.starts_with('"')
        && value.ends_with('"')
        && value.len() >= 2
        && value[1..value.len() - 1]
            .bytes()
            .all(|byte| byte == 0x21 || (0x23..=0x7e).contains(&byte) || byte >= 0x80)
}

fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let (unit, content) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = content.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let number = |value: &str| {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            None
        } else {
            value.parse::<u64>().ok()
        }
    };
    Some((
        number(start)?,
        number(end)?,
        if total == "*" {
            None
        } else {
            Some(number(total)?)
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::{download_to_cache, validate_cache_file_name};
    use crate::errors::Result;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;

    fn loopback_agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(std::time::Duration::from_secs(5)))
            .build()
            .into()
    }

    fn serve_once(response: &'static [u8]) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]).to_string();
            stream.write_all(response).unwrap();
            stream.flush().unwrap();
            request
        });
        (format!("http://{address}"), handle)
    }

    // Local CA and loopback servers exercise the actual cache downloader without
    // disabling certificate verification or relying on public HTTPS services.
    struct HttpsIdentity {
        ca_pem: String,
        server: std::sync::Arc<rustls::ServerConfig>,
    }

    impl HttpsIdentity {
        fn new(host: &str) -> Self {
            let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
            ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            ca_params.key_usages = vec![
                rcgen::KeyUsagePurpose::KeyCertSign,
                rcgen::KeyUsagePurpose::DigitalSignature,
            ];
            let ca_key = rcgen::KeyPair::generate().unwrap();
            let ca = ca_params.self_signed(&ca_key).unwrap();
            let issuer = rcgen::Issuer::new(ca_params, ca_key);
            let mut leaf_params = rcgen::CertificateParams::new(vec![host.to_owned()]).unwrap();
            leaf_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
            let leaf_key = rcgen::KeyPair::generate().unwrap();
            let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
            let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
            let server = rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf.der().clone(), ca.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
                )
                .unwrap();
            Self {
                ca_pem: ca.pem(),
                server: std::sync::Arc::new(server),
            }
        }

        fn agent(&self) -> ureq::Agent {
            let certificate = ureq::tls::Certificate::from_pem(self.ca_pem.as_bytes()).unwrap();
            ureq::Agent::config_builder()
                .proxy(None)
                .timeout_global(Some(std::time::Duration::from_secs(5)))
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .root_certs(ureq::tls::RootCerts::new_with_certs(&[certificate]))
                        .build(),
                )
                .build()
                .new_agent()
        }
    }

    struct LocalServer {
        url: String,
        worker: Option<thread::JoinHandle<std::io::Result<()>>>,
    }

    impl LocalServer {
        fn new(tls: Option<std::sync::Arc<rustls::ServerConfig>>, response: Vec<u8>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let scheme = if tls.is_some() { "https" } else { "http" };
            let worker = thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
                let (socket, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if std::time::Instant::now() >= deadline {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "client did not connect",
                                ));
                            }
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => return Err(error),
                    }
                };
                // Windows accepts inherit the listener's nonblocking mode.
                // TLS performs blocking I/O guarded by the socket deadlines.
                socket.set_nonblocking(false)?;
                socket.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
                socket.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
                let mut stream: Box<dyn ReadWrite> = if let Some(tls) = tls {
                    Box::new(rustls::StreamOwned::new(
                        rustls::ServerConnection::new(tls).unwrap(),
                        socket,
                    ))
                } else {
                    Box::new(socket)
                };
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte)?;
                    request.push(byte[0]);
                    if request.len() > 16 * 1024 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "oversized request",
                        ));
                    }
                }
                stream.write_all(&response)?;
                stream.flush()
            });
            Self {
                url: format!("{scheme}://{address}/payload.bin"),
                worker: Some(worker),
            }
        }

        fn finish(&mut self) -> std::io::Result<()> {
            self.worker.take().unwrap().join().unwrap()
        }
    }

    trait ReadWrite: Read + Write {}
    impl<T: Read + Write> ReadWrite for T {}

    impl Drop for LocalServer {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn download_https_fixture(
        identity: &HttpsIdentity,
        trusted: &HttpsIdentity,
        redirect: bool,
    ) -> (crate::errors::Result<std::path::PathBuf>, tempfile::TempDir) {
        let payload = b"verified HTTPS payload";
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        )
        .into_bytes();
        response.extend_from_slice(payload);
        let mut server = LocalServer::new(Some(identity.server.clone()), response);
        let mut redirect_server = redirect.then(|| LocalServer::new(None, format!(
            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", server.url
        ).into_bytes()));
        let url = redirect_server
            .as_ref()
            .map_or_else(|| server.url.clone(), |server| server.url.clone());
        let cache = tempfile::tempdir().unwrap();
        let checksum = super::ChecksumSpec {
            algorithm: "sha256".into(),
            hash: crate::util::hex_encode(Sha256::digest(payload)),
        };
        let result = download_to_cache(
            &url,
            cache.path(),
            "payload.bin",
            Some(&checksum),
            Some(&trusted.agent()),
        );
        if let Some(redirect) = redirect_server.as_mut() {
            redirect.finish().unwrap();
        }
        let served = server.finish();
        if result.is_ok() {
            served.unwrap();
        } else {
            eprintln!("HTTPS fixture client: {result:?}; server: {served:?}");
            assert!(
                served.is_err(),
                "rejected TLS peer unexpectedly received an HTTP request"
            );
        }
        let destination = cache
            .path()
            .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())))
            .join("payload")
            .join("payload.bin");
        if let Ok(path) = &result {
            assert_eq!(path, &std::fs::canonicalize(&destination).unwrap());
            assert_eq!(std::fs::read(path).unwrap(), payload);
            assert!(!super::control_path(
                destination.parent().unwrap().parent().unwrap(),
                "payload.bin",
                "part"
            )
            .exists());
            // Cache hit must need no second server connection.
            assert_eq!(
                download_to_cache(
                    &url,
                    cache.path(),
                    "payload.bin",
                    Some(&checksum),
                    Some(&trusted.agent())
                )
                .unwrap(),
                *path
            );
        } else {
            assert!(!destination.exists());
        }
        (result, cache)
    }

    #[test]
    fn https_trusted_ca_and_redirect_publish_verified_cache() {
        let identity = HttpsIdentity::new("127.0.0.1");
        for redirect in [false, true] {
            let (result, _cache) = download_https_fixture(&identity, &identity, redirect);
            result.unwrap();
        }
    }

    #[test]
    fn https_rejects_untrusted_ca_direct_and_redirected() {
        let identity = HttpsIdentity::new("127.0.0.1");
        let unrelated = HttpsIdentity::new("127.0.0.1");
        for redirect in [false, true] {
            let (result, _cache) = download_https_fixture(&identity, &unrelated, redirect);
            let error = result.unwrap_err().to_string();
            assert!(
                error.to_ascii_lowercase().contains("certificate"),
                "{error}"
            );
        }
    }

    #[test]
    fn https_rejects_hostname_mismatch_direct_and_redirected() {
        let identity = HttpsIdentity::new("wrong-host.example");
        for redirect in [false, true] {
            let (result, _cache) = download_https_fixture(&identity, &identity, redirect);
            let error = result.unwrap_err().to_string();
            assert!(
                error.to_ascii_lowercase().contains("certificate"),
                "{error}"
            );
        }
    }

    #[test]
    fn download_cache_names_are_single_path_components() {
        for name in [
            "../escape.tar",
            "folder/file.tar",
            r"folder\file.tar",
            "",
            ".",
            "..",
        ] {
            assert!(validate_cache_file_name(name).is_err(), "accepted {name:?}");
        }
        assert!(validate_cache_file_name("archive.tar.gz").is_ok());
    }

    #[test]
    fn incomplete_http_response_never_becomes_a_cached_file() {
        let (url, server) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc");
        let cache = tempfile::tempdir().unwrap();
        let result = download_to_cache(
            &url,
            cache.path(),
            "archive.tar",
            None,
            Some(&loopback_agent()),
        );
        let _ = server.join().unwrap();
        assert!(result.is_err());

        let hash = crate::util::hex_encode(Sha256::digest(url.as_bytes()));
        let directory = cache.path().join(hash);
        assert!(!directory.join("payload/archive.tar").exists());
        assert!(super::control_path(&directory, "archive.tar", "part").exists());
    }

    #[test]
    fn resume_validates_content_range_and_publishes_complete_file() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/archive.tar");
        let hash = crate::util::hex_encode(Sha256::digest(url.as_bytes()));
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join(hash);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            super::control_path(&directory, "archive.tar", "part"),
            b"abc",
        )
        .unwrap();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]).to_string();
            stream
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef",
                )
                .unwrap();
            stream.flush().unwrap();
            request
        });

        let spec = super::ChecksumSpec {
            algorithm: "sha256".into(),
            hash: crate::util::hex_encode(Sha256::digest(b"abcdef")),
        };
        let path = download_to_cache(
            &url,
            cache.path(),
            "archive.tar",
            Some(&spec),
            Some(&loopback_agent()),
        )
        .unwrap();
        let request = server.join().unwrap();
        assert!(request.to_ascii_lowercase().contains("range: bytes=3-"));
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        assert!(!super::control_path(&directory, "archive.tar", "part").exists());
    }

    #[test]
    fn invalid_checksums_fail_before_cache_io() {
        let owner = tempfile::tempdir().unwrap();
        let cache = owner.path().join("absent");
        for (algorithm, hash) in [("md5", "00"), ("sha256", "not-hex"), ("sha1", "00")] {
            let spec = super::ChecksumSpec {
                algorithm: algorithm.into(),
                hash: hash.into(),
            };
            assert!(download_to_cache(
                "http://127.0.0.1:1/no-network",
                &cache,
                "file",
                Some(&spec),
                None
            )
            .is_err());
            assert!(!cache.exists());
        }
    }

    #[test]
    fn non_payload_statuses_never_publish_cache_files() {
        for response in [
            b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".as_slice(),
            b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".as_slice(),
        ] {
            let (url, server) = serve_once(response);
            let cache = tempfile::tempdir().unwrap();
            assert!(download_to_cache(
                &url,
                cache.path(),
                "archive",
                None,
                Some(&loopback_agent())
            )
            .is_err());
            server.join().unwrap();
            let directory = cache
                .path()
                .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())));
            assert!(!directory.join("payload/archive").exists());
            assert!(!super::control_path(&directory, "archive", "part").exists());
        }
    }

    #[test]
    fn unverifiable_legacy_partial_restarts_without_a_range() {
        let (url, server) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew");
        let cache = tempfile::tempdir().unwrap();
        let directory = cache
            .path()
            .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(super::control_path(&directory, "archive", "part"), b"old").unwrap();
        let path = download_to_cache(&url, cache.path(), "archive", None, Some(&loopback_agent()))
            .unwrap();
        let request = server.join().unwrap();
        assert!(!request.to_ascii_lowercase().contains("range:"));
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }

    fn validated_partial_fixture(
        response: &'static [u8],
    ) -> (Result<PathBuf>, tempfile::TempDir, String) {
        let (url, server) = serve_once(response);
        let cache = tempfile::tempdir().unwrap();
        let directory = cache
            .path()
            .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(super::control_path(&directory, "archive", "part"), b"abc").unwrap();
        std::fs::write(
            super::control_path(&directory, "archive", "resume.json"),
            br#"{"etag":"\"v1\""}"#,
        )
        .unwrap();
        let result =
            download_to_cache(&url, cache.path(), "archive", None, Some(&loopback_agent()));
        let request = server.join().unwrap();
        (result, cache, request)
    }

    #[test]
    fn resumed_full_response_is_reused_without_a_second_request() {
        let (result, _cache, request) = validated_partial_fixture(
            b"HTTP/1.1 200 OK\r\nETag: \"v2\"\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew",
        );
        assert!(request.to_ascii_lowercase().contains("if-range: \"v1\""));
        assert_eq!(std::fs::read(result.unwrap()).unwrap(), b"new");
    }

    #[test]
    fn invalid_ranges_and_changed_validators_do_not_append() {
        for response in [
            b"HTTP/1.1 206 Partial Content\r\nETag: \"v1\"\r\nContent-Range: bytes 3-2/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef".as_slice(),
            b"HTTP/1.1 206 Partial Content\r\nETag: \"v1\"\r\nContent-Range: bytes 3-5/5\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef".as_slice(),
            b"HTTP/1.1 206 Partial Content\r\nETag: \"v2\"\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef".as_slice(),
        ] {
            let (result, cache, _) = validated_partial_fixture(response);
            assert!(result.is_err());
            let entry = std::fs::read_dir(cache.path()).unwrap().next().unwrap().unwrap().path();
            assert_eq!(std::fs::read(super::control_path(&entry, "archive", "part")).unwrap(), b"abc");
            assert!(!entry.join("payload/archive").exists());
        }
    }

    #[test]
    fn unknown_complete_length_does_not_publish_without_checksum() {
        let (result, cache, _) = validated_partial_fixture(
            b"HTTP/1.1 206 Partial Content\r\nETag: \"v1\"\r\nContent-Range: bytes 3-5/*\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef");
        assert!(result.is_err());
        let entry = std::fs::read_dir(cache.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read(super::control_path(&entry, "archive", "part")).unwrap(),
            b"abcdef"
        );
        assert!(!entry.join("payload/archive").exists());
    }

    #[test]
    fn concurrent_downloads_use_one_complete_publication() {
        let (url, server) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef");
        let cache = tempfile::tempdir().unwrap();
        let writers = (0..2)
            .map(|_| {
                let url = url.clone();
                let root = cache.path().to_path_buf();
                thread::spawn(move || {
                    download_to_cache(&url, &root, "archive", None, Some(&loopback_agent()))
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            assert_eq!(std::fs::read(writer.join().unwrap()).unwrap(), b"abcdef");
        }
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn partial_symlink_is_rejected_without_touching_its_target() {
        let cache = tempfile::tempdir().unwrap();
        let url = "http://127.0.0.1:1/no-network";
        let directory = cache
            .path()
            .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())));
        std::fs::create_dir_all(&directory).unwrap();
        let foreign = cache.path().join("valuable");
        std::fs::write(&foreign, b"keep").unwrap();
        std::os::unix::fs::symlink(&foreign, super::control_path(&directory, "archive", "part"))
            .unwrap();
        assert!(
            download_to_cache(url, cache.path(), "archive", None, Some(&loopback_agent())).is_err()
        );
        assert_eq!(std::fs::read(foreign).unwrap(), b"keep");
    }

    #[test]
    fn rejected_range_restarts_once_and_publishes_the_full_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let cache = tempfile::tempdir().unwrap();
        let entry = cache
            .path()
            .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())));
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(super::control_path(&entry, "archive", "part"), b"old").unwrap();
        std::fs::write(
            super::control_path(&entry, "archive", "resume.json"),
            br#"{"etag":"\"v1\""}"#,
        )
        .unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */3\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew".as_slice(),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let count = stream.read(&mut request).unwrap();
                requests.push(String::from_utf8_lossy(&request[..count]).to_ascii_lowercase());
                stream.write_all(response).unwrap();
                stream.flush().unwrap();
            }
            requests
        });
        let result =
            download_to_cache(&url, cache.path(), "archive", None, Some(&loopback_agent()))
                .unwrap();
        let requests = server.join().unwrap();
        assert!(requests[0].contains("range: bytes=3-"));
        assert!(!requests[1].contains("range:"));
        assert_eq!(std::fs::read(result).unwrap(), b"new");
    }

    #[test]
    fn custom_agent_cannot_publish_http_error_bodies() {
        let (url, server) = serve_once(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad",
        );
        let cache = tempfile::tempdir().unwrap();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(std::time::Duration::from_secs(5)))
            .http_status_as_error(false)
            .build()
            .into();
        assert!(download_to_cache(&url, cache.path(), "archive", None, Some(&agent)).is_err());
        server.join().unwrap();
        let entry = cache
            .path()
            .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())));
        assert!(!entry.join("payload/archive").exists());
        assert!(!super::control_path(&entry, "archive", "part").exists());
    }

    #[test]
    fn payload_names_cannot_alias_another_downloads_control_files() {
        let (url, server) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew");
        let cache = tempfile::tempdir().unwrap();
        let entry = cache
            .path()
            .join(crate::util::hex_encode(Sha256::digest(url.as_bytes())));
        std::fs::create_dir_all(&entry).unwrap();
        let control = super::control_path(&entry, "archive", "part");
        std::fs::write(&control, b"keep").unwrap();
        let collision = control.file_name().unwrap().to_str().unwrap();
        let result =
            download_to_cache(&url, cache.path(), collision, None, Some(&loopback_agent()))
                .unwrap();
        server.join().unwrap();
        assert_eq!(std::fs::read(result).unwrap(), b"new");
        assert_eq!(
            std::fs::read(super::control_path(&entry, "archive", "part")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn valid_long_payload_names_use_bounded_control_names() {
        let name = "a".repeat(250);
        let (url, server) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew");
        let cache = tempfile::tempdir().unwrap();
        let result =
            download_to_cache(&url, cache.path(), &name, None, Some(&loopback_agent())).unwrap();
        server.join().unwrap();
        assert_eq!(result.file_name().unwrap(), name.as_str());
        assert_eq!(std::fs::read(result).unwrap(), b"new");
    }
}
