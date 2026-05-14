//! Smoke: spin up an echo TCP server on a random localhost port, write
//! a TOML config that forwards another random port to it, launch the
//! standalone binary as a subprocess, verify bytes echo, then send
//! SIGTERM and verify graceful exit.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn spawn_echo() -> (u16, Arc<AtomicBool>) {
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
                Ok((mut sock, _)) => {
                    thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = sock.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            if sock.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        let _ = sock.shutdown(Shutdown::Both);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
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
fn tcp_echo_loopback() {
    let (backend_port, backend_stop) = spawn_echo();
    let frontend_port = pick_port();
    let cfg = format!(
        r#"
[global]
log_level = "warn"

[[rule]]
name = "echo"
protocol = "tcp"
listen_port = {frontend_port}
target = "127.0.0.1:{backend_port}"
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
        .expect("spawn standalone");

    let listening =
        wait_for_listen(frontend_port, Instant::now() + Duration::from_secs(10));
    assert!(
        listening,
        "frontend port {frontend_port} should be listening"
    );

    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    s.write_all(b"hello, standalone").unwrap();
    let mut buf = [0u8; 17];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello, standalone");
    drop(s);

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SAFETY: we hold the child handle so its pid is valid.
        #[allow(unsafe_code)]
        #[allow(clippy::cast_possible_wrap)]
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let status = child.wait().expect("standalone wait");
        let code = status
            .code()
            .unwrap_or_else(|| status.signal().expect("signaled"));
        assert!(code == 0 || code == 143, "expected clean exit, got {code}");
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    backend_stop.store(true, Ordering::Relaxed);
}
