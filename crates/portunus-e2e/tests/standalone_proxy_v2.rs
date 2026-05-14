//! E2E: PROXY v2 prelude reaches backend.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PROXY_V2_SIG: [u8; 12] =
    [0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A];

fn wait_for_listen(port: u16, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn proxy_v2_prelude_reaches_backend() {
    // The backend verifies the PROXY v2 signature then sends it back
    // over the mpsc channel so we can assert it in the test body.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let backend_port = listener.local_addr().unwrap().port();

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut sig = [0u8; 12];
            if s.read_exact(&mut sig).is_ok() {
                let mut rest = [0u8; 4];
                let _ = s.read_exact(&mut rest);
                let len = u16::from_be_bytes([rest[2], rest[3]]) as usize;
                let mut addr_block = vec![0u8; len];
                let _ = s.read_exact(&mut addr_block);
                let _ = tx.send(sig.to_vec());
                // Echo any payload sent after the prelude.
                let mut payload = [0u8; 32];
                if let Ok(n) = s.read(&mut payload) {
                    let _ = s.write_all(&payload[..n]);
                }
            }
        }
    });

    let frontend_port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = format!(
        r#"
[[rule]]
name = "proxyv2"
protocol = "tcp"
listen_port = {frontend_port}
targets = [{{ host = "127.0.0.1", port = {backend_port}, priority = 0, proxy_protocol = "v2" }}]
"#
    );
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &cfg).unwrap();

    let bin = assert_cmd::cargo::cargo_bin("portunus-standalone");
    let mut child = Command::new(&bin)
        .arg("--config")
        .arg(tmp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    assert!(
        wait_for_listen(frontend_port, Instant::now() + Duration::from_secs(10)),
        "frontend port {frontend_port} should be listening"
    );

    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    s.write_all(b"data after prelude").unwrap();

    let sig = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("backend should have received PROXY v2 prelude within 5s");
    assert_eq!(
        sig, PROXY_V2_SIG,
        "first 12 bytes must be the PROXY v2 signature"
    );
    drop(s);

    #[cfg(unix)]
    {
        // SAFETY: own child handle.
        #[allow(unsafe_code, clippy::cast_possible_wrap)]
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}
