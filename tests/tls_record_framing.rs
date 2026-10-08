//! Native TLS framing acceptance: real verified loopback TLS and forced short recv.
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
#[cfg(test)]
mod record {
    use rustpython_host_env::ssl::connection::RecordCursor;

    #[test]
    fn every_fragment_preserves_the_next_record() {
        let mut first = vec![23, 3, 3, 64, 1];
        first.extend(vec![0xff; 16385]);
        let next = [21, 3, 3, 0, 2, 1, 0];
        let mut wire = first.clone();
        wire.extend(next);
        for split in 1..first.len() {
            let mut cursor = RecordCursor::default();
            let mut offset = 0;
            while offset < first.len() {
                let available = if offset < split {
                    split - offset
                } else {
                    wire.len() - offset
                };
                let limit = cursor.want().min(available);
                assert!(limit > 0);
                assert!(
                    offset + limit <= first.len(),
                    "split={split}, offset={offset}"
                );
                let received = if offset % 3 == 0 { limit.min(7) } else { limit };
                cursor.consume(&wire[offset..offset + received]);
                offset += received;
            }
            assert!(cursor.in_header());
            assert_eq!(cursor.want(), 5);
            cursor.consume(&next[..5]);
            assert_eq!(cursor.want(), 2);
            cursor.consume(&next[5..]);
            assert!(cursor.in_header());
        }
    }

    #[test]
    fn empty_reads_do_not_change_partial_header_or_body() {
        let first = [23, 3, 3, 0, 3, 0xff, 0xff, 0xff];
        let mut cursor = RecordCursor::default();
        cursor.consume(&first[..2]);
        assert_eq!(cursor.want(), 3);
        cursor.consume(&[]);
        assert_eq!(cursor.want(), 3);
        cursor.consume(&first[2..5]);
        assert_eq!(cursor.want(), 3);
        cursor.consume(&[]);
        assert_eq!(cursor.want(), 3);
        cursor.consume(&first[5..]);
        assert!(cursor.in_header());
        assert_eq!(cursor.want(), 5);
    }

    #[test]
    fn zero_length_record_does_not_consume_following_header() {
        let wire = [23, 3, 3, 0, 0, 21, 3, 3, 0, 0];
        let mut cursor = RecordCursor::default();
        assert_eq!(cursor.want(), 5);
        cursor.consume(&wire[..5]);
        assert!(cursor.in_header());
        assert_eq!(cursor.want(), 5);
        cursor.consume(&wire[5..]);
        assert!(cursor.in_header());
    }
}

const CLIENT: &str = r#"
import sys, time
phase_start=time.monotonic()
print("TLS_PHASE interpreter-script-start", time.monotonic()-phase_start, file=sys.stderr, flush=True)
import socket, ssl, urllib.request, urllib.error
print("TLS_PHASE imports-ready", time.monotonic()-phase_start, file=sys.stderr, flush=True)
original = socket.socket.recv
states = {}
def recv(self, size, flags=0):
    state = states.setdefault(self.fileno(), [bytearray(), 0])
    header, remaining = state
    if flags & socket.MSG_PEEK:
        return original(self, size, flags)
    if remaining:
        assert size <= remaining, ("body boundary crossed", size, remaining)
        maximum = 2048
    elif header:
        assert size <= 5-len(header), ("partial header treated as new", size, len(header))
        maximum = 1
    else:
        maximum = 2
    data = original(self, min(size, maximum), flags)
    if remaining:
        state[1] -= len(data)
    else:
        header.extend(data)
        if len(header) == 5:
            state[1] = int.from_bytes(header[3:5], "big")
            header.clear()
    return data
socket.socket.recv = recv
print("TLS_PHASE context-start", time.monotonic()-phase_start, file=sys.stderr, flush=True)
context = ssl.create_default_context(cafile=sys.argv[2])
print("TLS_PHASE context-ready", time.monotonic()-phase_start, file=sys.stderr, flush=True)
start=time.monotonic()
try:
    with urllib.request.urlopen(sys.argv[1], context=context, timeout=0.5) as response:
        print("TLS_PHASE headers-ready", time.monotonic()-phase_start, file=sys.stderr, flush=True)
        print("TLS_PHASE body-start", time.monotonic()-phase_start, file=sys.stderr, flush=True)
        result=response.read()
        print("TLS_PHASE body-complete", time.monotonic()-phase_start, file=sys.stderr, flush=True)
        assert len(result)==3750000, len(result)
        assert result==b"a"*3750000
    assert sys.argv[3] in ("body", "leaf_only")
    print("TLS_FRAGMENTED_BODY_OK")
except TimeoutError:
    assert sys.argv[3]=="timeout"
    assert time.monotonic()-start<3
    print("TLS_FRAGMENTED_TIMEOUT_OK")
except urllib.error.URLError as error:
    assert sys.argv[3] in ("missing_aki", "self_issued")
    assert isinstance(error.reason, ssl.SSLCertVerificationError), error.reason
    print("TLS_CERT_REJECTED")
"#;

const BIO_CLIENT: &str = r#"import ssl,socket,sys,time
context=ssl.create_default_context(cafile=sys.argv[2])
incoming,outgoing=ssl.MemoryBIO(),ssl.MemoryBIO()
tls=context.wrap_bio(incoming,outgoing,server_side=False,server_hostname="127.0.0.1")
sock=socket.create_connection(("127.0.0.1",int(sys.argv[1].rsplit(":",1)[1])),timeout=5)
def flush():
    data=outgoing.read()
    if data:sock.sendall(data)
def invoke(function):
    while True:
        try:
            result=function();flush();return result
        except ssl.SSLWantReadError:
            flush()
            data=sock.recv(65536)
            if data:incoming.write(data)
            else:incoming.write_eof()
        except ssl.SSLWantWriteError:
            flush()
invoke(tls.do_handshake)
invoke(lambda:tls.write(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"))
received=bytearray();headerend=None;start=time.monotonic()
while headerend is None or len(received)-headerend<3750000:
    data=invoke(lambda:tls.read(65536))
    assert data,"premature EOF"
    received.extend(data)
    if headerend is None:
        at=received.find(b"\r\n\r\n")
        if at>=0:headerend=at+4
assert received[headerend:]==b"a"*3750000
print("MEMORY_BIO_COMPLETE",len(received)-headerend,time.monotonic()-start,flush=True)
sock.close()
"#;

fn run(
    command: &mut Command,
    timeout: Duration,
    phase: &str,
) -> (std::process::Output, bool, Duration) {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + timeout;
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    let elapsed = started.elapsed();
    eprintln!("TLS phase={phase} elapsed={elapsed:?} timed_out={timed_out}");
    (output, timed_out, elapsed)
}

fn exercise(mode: &str) {
    let timeout = mode == "timeout";
    let rejection = matches!(mode, "missing_aki" | "self_issued");
    let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Owned TLS root");
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate().unwrap();
    let ca = params.self_signed(&key).unwrap();
    let issuer = rcgen::Issuer::new(params, key);
    let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        if mode == "self_issued" {
            "Owned TLS root"
        } else {
            "Owned TLS leaf"
        },
    );
    params.use_authority_key_identifier_extension = !rejection;
    let key = rcgen::KeyPair::generate().unwrap();
    let leaf = params.signed_by(&key, &issuer).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        if mode == "leaf_only" {
            vec![leaf.der().clone()]
        } else {
            vec![leaf.der().clone(), ca.der().clone()]
        },
        rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let ca_path = directory.path().join("ca.pem");
    std::fs::write(&ca_path, ca.pem()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("https://{}", listener.local_addr().unwrap());
    let configure = |command: &mut Command| {
        command
            .env("HOME", directory.path())
            .env("USERPROFILE", directory.path())
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .env_remove("REZ_RS_PYTHON_REENTRY");
    };
    // Direct -c dispatch runs in this process, so the deadline owns the entire
    // discovery process. Explicit "rez python" would spawn a child interpreter.
    let mut discover = Command::new(env!("CARGO_BIN_EXE_rez"));
    configure(&mut discover);
    discover.args(["-I", "-c", "import sys; print(sys.executable)"]);
    // Snapshot hashing/copying is a cold-start phase, independent of TLS I/O.
    let (discovered, timed_out, elapsed) = run(&mut discover, Duration::from_secs(45), "discovery");
    assert!(
        !timed_out && discovered.status.success(),
        "TLS mode={mode} phase=discovery elapsed={elapsed:?} timed_out={timed_out} status={} stdout={} stderr={}",
        discovered.status,
        String::from_utf8_lossy(&discovered.stdout),
        String::from_utf8_lossy(&discovered.stderr)
    );
    let alias = String::from_utf8(discovered.stdout).unwrap();
    let server_state = Arc::new(Mutex::new(("accept_wait", Duration::ZERO)));
    let worker_state = Arc::clone(&server_state);
    let worker = thread::spawn(move || {
        let started = Instant::now();
        let progress = |phase: &'static str| {
            *worker_state.lock().unwrap() = (phase, started.elapsed());
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("TLS fixture accept: {error}"),
            }
        };
        progress("accepted");
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut stream = rustls::StreamOwned::new(
            rustls::ServerConnection::new(Arc::new(config)).unwrap(),
            socket,
        );
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if let Err(error) = stream.read_exact(&mut byte) {
                if rejection {
                    progress("rejected");
                    return;
                }
                panic!("TLS fixture request failed: {error}");
            }
            request.push(byte[0]);
            assert!(request.len() < 16384);
        }
        progress("request_complete");
        if timeout {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n")
                .unwrap();
            stream.flush().unwrap();
            progress("timeout_headers_flushed");
            thread::sleep(Duration::from_secs(1));
        } else {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 3750000\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let body = vec![b'a'; 3750000];
            progress("body_write_start");
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
            progress("body_flushed");
            stream.conn.send_close_notify();
            let _ = stream.flush();
        }
        progress("closed");
    });
    let mut command = Command::new(alias.trim());
    configure(&mut command);
    command
        .args([
            "-I",
            "-c",
            if mode == "bio" { BIO_CLIENT } else { CLIENT },
            &url,
        ])
        .arg(ca_path)
        .arg(mode);
    let (output, timed_out, elapsed) = run(&mut command, Duration::from_secs(15), "native-client");
    let server_result = worker.join();
    let server_snapshot = *server_state.lock().unwrap();
    eprintln!(
        "TLS mode={mode} phase=native-client elapsed={elapsed:?} timed_out={timed_out} server_state={server_snapshot:?} markers={}",
        String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(2048)])
    );
    assert!(
        !timed_out && output.status.success(),
        "TLS mode={mode} phase=native-client elapsed={elapsed:?} timed_out={timed_out} status={} stdout={} stderr={} server={server_result:?} server_state={server_snapshot:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        server_result.is_ok(),
        "TLS mode={mode} fixture failed after successful native child: {server_result:?} server_state={server_snapshot:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(if timeout {
            "TLS_FRAGMENTED_TIMEOUT_OK"
        } else if rejection {
            "TLS_CERT_REJECTED"
        } else if mode == "bio" {
            "MEMORY_BIO_COMPLETE"
        } else {
            "TLS_FRAGMENTED_BODY_OK"
        })
    );
}

#[test]
fn native_tls_preserves_record_boundaries_after_short_recv() {
    exercise("body");
}
#[test]
fn native_tls_fragmented_reads_respect_timeout() {
    exercise("timeout");
}

#[test]
fn native_tls_memory_bio_large_body_remains_supported() {
    exercise("bio");
}

#[test]
fn native_tls_trusted_leaf_only_chain_is_supported() {
    exercise("leaf_only");
}
#[test]
fn native_tls_non_self_signed_leaf_without_aki_is_rejected() {
    exercise("missing_aki");
}
#[test]
fn native_tls_self_issued_cross_signed_leaf_without_aki_is_rejected() {
    exercise("self_issued");
}
