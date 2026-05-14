//! E2E: standalone failover — primary drops, next connection lands on secondary.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn spawn_marker_server(marker: &'static [u8]) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        loop {
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            match listener.accept() {
                Ok((mut s, _)) => {
                    thread::spawn(move || {
                        let _ = s.write_all(marker);
                        let _ = s.flush();
                        let mut buf = [0u8; 64];
                        if let Ok(n) = s.read(&mut buf) {
                            let _ = s.write_all(&buf[..n]);
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (port, stop)
}

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
fn failover_routes_to_secondary_after_primary_drops() {
    let (primary_port, primary_stop) = spawn_marker_server(b"PRIMARY\n");
    let (secondary_port, secondary_stop) = spawn_marker_server(b"SECONDARY\n");
    let frontend_port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let cfg = format!(
        r#"
[global]
log_level = "warn"

[[rule]]
name = "failover"
protocol = "tcp"
listen_port = {frontend_port}
targets = [
  {{ host = "127.0.0.1", port = {primary_port},   priority = 0  }},
  {{ host = "127.0.0.1", port = {secondary_port}, priority = 10 }},
]
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

    // First connection: primary is alive → should get PRIMARY marker.
    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = vec![0u8; 8];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"PRIMARY\n", "first connection should reach primary");
    drop(s);

    // Stop the primary — set the stop flag and give the listener thread
    // time to exit its accept loop so the port becomes unbound.
    primary_stop.store(true, Ordering::Relaxed);
    // Give the primary listener time to actually release the port; the
    // failover dial path trips on a refused-connect.
    thread::sleep(Duration::from_millis(500));

    // Second connection: primary is down → forwarder tries primary
    // (ECONNREFUSED), immediately falls through to secondary.
    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = vec![0u8; 10];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(
        &buf,
        b"SECONDARY\n",
        "second connection should reach secondary after primary drops"
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
    secondary_stop.store(true, Ordering::Relaxed);
}
