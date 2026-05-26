//! Batched UDP I/O helpers (Linux `recvmmsg(2)` / `sendmmsg(2)`).
//!
//! v1.5.1 closes the +11% UDP CPU gap against realm observed in the
//! 2026-05-26 VPS benchmark. `strace` showed portunus issued ~37k
//! `sendto`/`recvfrom` calls in a 5s/1Gbps iperf3 UDP window while
//! realm issued ~2k `sendmmsg`/`recvmmsg` calls — same data plane, 18×
//! fewer syscalls. This module provides the batched-syscall wrappers
//! that the listener hot path uses on Linux. On non-Linux platforms
//! the helpers are stubs that always return `Err(WouldBlock)`, so the
//! caller falls back to the single-packet `recv_from` / `try_send`
//! path that has shipped since v0.4.
//!
//! Design notes:
//! * Buffers are heap-allocated, contiguous, slot-sized at
//!   [`UDP_BUFFER_BYTES`] each. `BatchBufs` owns the arena; the
//!   recvmmsg syscall fills `lens[]` and `addrs[]` in place.
//! * Both helpers run inside `UdpSocket::try_io(Interest::*, …)` so
//!   tokio's reactor properly clears the readiness flag on
//!   `WouldBlock`. The caller awaits `readable()` / `writable()` first
//!   exactly the same way it does for `recv_from` today.
//! * No allocations on the hot path — buffers are reused across loop
//!   iterations.

#![allow(unsafe_code)] // raw recvmmsg / sendmmsg

use std::io;
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
use std::net::SocketAddr;

#[cfg(target_os = "linux")]
use tokio::io::Interest;
use tokio::net::UdpSocket;

/// IP-layer UDP payload ceiling (matches FR-013 in the v0.4 spec).
/// Sizing each slot to this value means `recvmmsg` can never truncate
/// a well-formed datagram.
pub(crate) const UDP_BUFFER_BYTES: usize = 65_535;

/// Max datagrams per batched syscall. 32 mirrors the demux fairness
/// budget — large enough to amortize the syscall cost on a 1 Gbps
/// stream (where realm averages ~18 packets per `recvmmsg`), small
/// enough that the 32 × 64 KiB arena (~2 MiB per listener) stays
/// well below the per-rule recv-buffer budget the v014 spec
/// guarantees (SC-001a: O(1) × 64 KiB *per rule*; this 2 MiB lives
/// inside the same listener task so it still scales O(1) in flow
/// count).
pub(crate) const BATCH_SIZE: usize = 32;

/// Heap arena holding `BATCH_SIZE` contiguous slots of
/// `UDP_BUFFER_BYTES`. One instance per listener loop; reused across
/// every batched `recvmmsg`/`sendmmsg` iteration.
pub(crate) struct BatchBufs {
    /// Flat slot arena: `slots[i * UDP_BUFFER_BYTES ..][.. UDP_BUFFER_BYTES]`
    /// is slot `i`.
    slots: Box<[u8]>,
    /// Filled by recvmmsg with per-datagram byte counts. After
    /// `recv_batch` returns `Ok(n)`, only `lens[..n]` is meaningful.
    lens: [usize; BATCH_SIZE],
    /// Filled by recvmmsg with per-datagram source addresses. Same
    /// validity rule as `lens`.
    addrs: [Option<SocketAddr>; BATCH_SIZE],
}

impl BatchBufs {
    pub(crate) fn new() -> Self {
        // Single contiguous allocation; zero-init is cheaper than
        // MaybeUninit acrobatics and the cost (≈2 MiB once per
        // listener spawn) is below noise.
        let slots = vec![0u8; BATCH_SIZE * UDP_BUFFER_BYTES].into_boxed_slice();
        Self {
            slots,
            lens: [0; BATCH_SIZE],
            addrs: [None; BATCH_SIZE],
        }
    }

    /// Returns the (payload, source) for slot `i`. Panics if `i >=
    /// BATCH_SIZE` or if the slot was not populated by the most
    /// recent `recv_batch` call (i.e. `i >= n_received`).
    pub(crate) fn slot(&self, i: usize) -> (&[u8], SocketAddr) {
        let off = i * UDP_BUFFER_BYTES;
        let len = self.lens[i];
        let payload = &self.slots[off..off + len];
        let src = self.addrs[i].expect("slot populated by recv_batch");
        (payload, src)
    }
}

/// Receive up to `BATCH_SIZE` datagrams in one syscall. Returns the
/// number of datagrams actually received. Caller must have already
/// awaited `socket.readable()`.
///
/// On non-Linux platforms this returns `Err(WouldBlock)` so the
/// caller falls back to the single-packet path. On Linux this calls
/// `recvmmsg(2)` via raw libc (nix v0.29's wrapper has an awkward
/// allocator-tied lifetime that costs us per-call work).
#[cfg(target_os = "linux")]
pub(crate) fn recv_batch(socket: &UdpSocket, bufs: &mut BatchBufs) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    socket.try_io(Interest::READABLE, || {
        // Build BATCH_SIZE iovec + sockaddr_storage + mmsghdr arrays
        // on the stack. The kernel writes lengths into
        // `msg_hdr.msg_len`; we lift those into `bufs.lens`.
        let fd = socket.as_raw_fd();
        let mut iovs: [libc::iovec; BATCH_SIZE] =
            unsafe { MaybeUninit::zeroed().assume_init() };
        let mut addrs: [libc::sockaddr_storage; BATCH_SIZE] =
            unsafe { MaybeUninit::zeroed().assume_init() };
        let mut hdrs: [libc::mmsghdr; BATCH_SIZE] =
            unsafe { MaybeUninit::zeroed().assume_init() };

        let slot_size = UDP_BUFFER_BYTES;
        for i in 0..BATCH_SIZE {
            // SAFETY: slot offsets are within `bufs.slots` (allocated
            // BATCH_SIZE * UDP_BUFFER_BYTES). The kernel writes to
            // these via the iovec; we only read them back after
            // recvmmsg returns and only up to msg_len.
            let slot_ptr = unsafe {
                bufs.slots
                    .as_mut_ptr()
                    .add(i * slot_size)
                    .cast::<libc::c_void>()
            };
            iovs[i] = libc::iovec {
                iov_base: slot_ptr,
                iov_len: slot_size,
            };
            hdrs[i].msg_hdr.msg_name =
                std::ptr::from_mut::<libc::sockaddr_storage>(&mut addrs[i]).cast();
            hdrs[i].msg_hdr.msg_namelen =
                std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            hdrs[i].msg_hdr.msg_iov = std::ptr::from_mut::<libc::iovec>(&mut iovs[i]);
            hdrs[i].msg_hdr.msg_iovlen = 1;
        }

        // MSG_DONTWAIT: the socket is already non-blocking, but
        // setting this defensively makes recvmmsg semantics identical
        // even if the underlying fd's O_NONBLOCK ever drifts.
        let rc = unsafe {
            libc::recvmmsg(
                fd,
                hdrs.as_mut_ptr(),
                BATCH_SIZE as libc::c_uint,
                libc::MSG_DONTWAIT,
                std::ptr::null_mut(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let n = rc as usize;
        for i in 0..n {
            bufs.lens[i] = hdrs[i].msg_len as usize;
            bufs.addrs[i] = sockaddr_to_socketaddr(&addrs[i], hdrs[i].msg_hdr.msg_namelen);
        }
        Ok(n)
    })
}

/// Non-Linux fallback: callers degrade to single-packet `recv_from`.
#[cfg(not(target_os = "linux"))]
pub(crate) fn recv_batch(_socket: &UdpSocket, _bufs: &mut BatchBufs) -> io::Result<usize> {
    Err(io::Error::from(io::ErrorKind::WouldBlock))
}

/// Send up to `payloads.len()` datagrams in one syscall on a
/// `connect()`-ed UDP socket. Returns the number of datagrams accepted
/// by the kernel. Caller must have already awaited `socket.writable()`.
///
/// Partial success is possible: the kernel may accept some prefix and
/// then return WouldBlock for the rest; we report the prefix length so
/// the caller can retry the tail on the next wakeup. Errors after a
/// partial success are reported as `Ok(n)` with the successful prefix.
#[cfg(target_os = "linux")]
pub(crate) fn send_batch_connected(
    socket: &UdpSocket,
    payloads: &[&[u8]],
) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    if payloads.is_empty() {
        return Ok(0);
    }
    socket.try_io(Interest::WRITABLE, || {
        let fd = socket.as_raw_fd();
        let len = payloads.len().min(BATCH_SIZE);
        let mut iovs: [libc::iovec; BATCH_SIZE] =
            unsafe { MaybeUninit::zeroed().assume_init() };
        let mut hdrs: [libc::mmsghdr; BATCH_SIZE] =
            unsafe { MaybeUninit::zeroed().assume_init() };

        for i in 0..len {
            iovs[i] = libc::iovec {
                iov_base: payloads[i].as_ptr().cast::<libc::c_void>().cast_mut(),
                iov_len: payloads[i].len(),
            };
            // Connected socket: leave msg_name NULL.
            hdrs[i].msg_hdr.msg_iov = std::ptr::from_mut::<libc::iovec>(&mut iovs[i]);
            hdrs[i].msg_hdr.msg_iovlen = 1;
        }

        let rc = unsafe {
            libc::sendmmsg(
                fd,
                hdrs.as_mut_ptr(),
                len as libc::c_uint,
                libc::MSG_DONTWAIT,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(rc as usize)
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn send_batch_connected(
    _socket: &UdpSocket,
    _payloads: &[&[u8]],
) -> io::Result<usize> {
    Err(io::Error::from(io::ErrorKind::WouldBlock))
}

/// Decode a `sockaddr_storage` of `len` bytes into a Rust
/// [`SocketAddr`]. Returns `None` for unrecognized families (only
/// AF_INET / AF_INET6 are valid on a UDP socket).
#[cfg(target_os = "linux")]
fn sockaddr_to_socketaddr(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> Option<SocketAddr> {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

    let family = i32::from(storage.ss_family);
    if family == libc::AF_INET
        && (len as usize) >= std::mem::size_of::<libc::sockaddr_in>()
    {
        // SAFETY: family + length checked; reinterpreting as
        // sockaddr_in is the standard POSIX pattern.
        let sin: &libc::sockaddr_in =
            unsafe { &*std::ptr::from_ref::<libc::sockaddr_storage>(storage).cast() };
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        let port = u16::from_be(sin.sin_port);
        Some(SocketAddr::V4(SocketAddrV4::new(ip, port)))
    } else if family == libc::AF_INET6
        && (len as usize) >= std::mem::size_of::<libc::sockaddr_in6>()
    {
        // SAFETY: family + length checked.
        let sin6: &libc::sockaddr_in6 =
            unsafe { &*std::ptr::from_ref::<libc::sockaddr_storage>(storage).cast() };
        let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
        let port = u16::from_be(sin6.sin6_port);
        Some(SocketAddr::V6(SocketAddrV6::new(
            ip,
            port,
            sin6.sin6_flowinfo,
            sin6.sin6_scope_id,
        )))
    } else {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::time::Duration;

    async fn bind_loopback() -> (Arc<UdpSocket>, SocketAddr) {
        let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = s.local_addr().unwrap();
        (Arc::new(s), addr)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recv_batch_reads_multiple_datagrams() {
        let (server, server_addr) = bind_loopback().await;
        let (client, _client_addr) = bind_loopback().await;

        // Fire 5 distinct datagrams synchronously.
        for i in 0..5u8 {
            client.send_to(&[i, i, i], server_addr).await.unwrap();
        }
        // Let the kernel deliver them to the server's recv queue.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut bufs = BatchBufs::new();
        server.readable().await.unwrap();
        let n = recv_batch(&server, &mut bufs).expect("recvmmsg succeeds");
        assert!(n >= 1, "should read at least one datagram, got 0");
        for i in 0..n {
            let (payload, _src) = bufs.slot(i);
            assert_eq!(payload.len(), 3);
            assert_eq!(payload[0], payload[1]);
            assert_eq!(payload[1], payload[2]);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_batch_connected_writes_multiple_datagrams() {
        let (server, server_addr) = bind_loopback().await;
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(server_addr).await.unwrap();

        let payloads: Vec<&[u8]> = vec![b"a", b"bb", b"ccc"];
        upstream.writable().await.unwrap();
        let n = send_batch_connected(&upstream, &payloads).expect("sendmmsg succeeds");
        assert_eq!(n, 3);

        let mut buf = [0u8; 16];
        let mut sizes = Vec::new();
        for _ in 0..3 {
            let (n, _) = tokio::time::timeout(
                Duration::from_secs(1),
                server.recv_from(&mut buf),
            )
            .await
            .unwrap()
            .unwrap();
            sizes.push(n);
        }
        sizes.sort_unstable();
        assert_eq!(sizes, vec![1, 2, 3]);
    }
}
