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

    /// Returns just the payload slice for slot `i`. Used by
    /// [`recv_batch_connected`], which fills a `connect()`-ed socket's
    /// datagrams whose source address is implied by the connection and
    /// therefore never written into `addrs`. Panics if `i >= BATCH_SIZE`;
    /// the caller must only read slots `i < n_received` from the most
    /// recent `recv_batch_connected` return.
    pub(crate) fn payload(&self, i: usize) -> &[u8] {
        let off = i * UDP_BUFFER_BYTES;
        let len = self.lens[i];
        &self.slots[off..off + len]
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
        let mut iovs: [libc::iovec; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };
        let mut addrs: [libc::sockaddr_storage; BATCH_SIZE] =
            unsafe { MaybeUninit::zeroed().assume_init() };
        let mut hdrs: [libc::mmsghdr; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };

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
            // sockaddr_storage is 128 B on every supported target;
            // `try_from` keeps clippy::cast_possible_truncation quiet
            // without losing the invariant.
            hdrs[i].msg_hdr.msg_namelen =
                libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>())
                    .unwrap_or(libc::socklen_t::MAX);
            hdrs[i].msg_hdr.msg_iov = std::ptr::from_mut::<libc::iovec>(&mut iovs[i]);
            hdrs[i].msg_hdr.msg_iovlen = 1;
        }

        // MSG_DONTWAIT: the socket is already non-blocking, but
        // setting this defensively makes recvmmsg semantics identical
        // even if the underlying fd's O_NONBLOCK ever drifts.
        // `BATCH_SIZE` is 32, fits in c_uint trivially; try_from
        // keeps clippy::cast_possible_truncation quiet.
        let batch_count = libc::c_uint::try_from(BATCH_SIZE).unwrap_or(libc::c_uint::MAX);
        let rc = unsafe {
            libc::recvmmsg(
                fd,
                hdrs.as_mut_ptr(),
                batch_count,
                libc::MSG_DONTWAIT as _,
                std::ptr::null_mut(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        // rc ≥ 0 checked above; on 64-bit usize is wider than c_int.
        let n = usize::try_from(rc).unwrap_or(0);
        for i in 0..n {
            // msg_len ≤ iov_len (which is itself usize); no truncation
            // possible on any supported target.
            bufs.lens[i] = usize::try_from(hdrs[i].msg_len).unwrap_or(0);
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
pub(crate) fn send_batch_connected(socket: &UdpSocket, payloads: &[&[u8]]) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    if payloads.is_empty() {
        return Ok(0);
    }
    socket.try_io(Interest::WRITABLE, || {
        let fd = socket.as_raw_fd();
        let len = payloads.len().min(BATCH_SIZE);
        let mut iovs: [libc::iovec; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };
        let mut hdrs: [libc::mmsghdr; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };

        for i in 0..len {
            iovs[i] = libc::iovec {
                iov_base: payloads[i].as_ptr().cast::<libc::c_void>().cast_mut(),
                iov_len: payloads[i].len(),
            };
            // Connected socket: leave msg_name NULL.
            hdrs[i].msg_hdr.msg_iov = std::ptr::from_mut::<libc::iovec>(&mut iovs[i]);
            hdrs[i].msg_hdr.msg_iovlen = 1;
        }

        // `len ≤ BATCH_SIZE` (32) by the `.min()` above; fits in c_uint.
        let send_count = libc::c_uint::try_from(len).unwrap_or(libc::c_uint::MAX);
        let rc =
            unsafe { libc::sendmmsg(fd, hdrs.as_mut_ptr(), send_count, libc::MSG_DONTWAIT as _) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        // rc ≥ 0 checked above; usize ≥ c_int on every supported target.
        Ok(usize::try_from(rc).unwrap_or(0))
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn send_batch_connected(_socket: &UdpSocket, _payloads: &[&[u8]]) -> io::Result<usize> {
    Err(io::Error::from(io::ErrorKind::WouldBlock))
}

/// Receive up to `BATCH_SIZE` datagrams in one syscall from a
/// `connect()`-ed UDP socket (the per-flow upstream socket on the
/// reply path). Returns the number of datagrams received; afterwards
/// only `bufs.payload(i)` for `i < n` is meaningful — `bufs.addrs` is
/// NOT written because the peer is fixed by the connection, so
/// `msg_name` stays NULL and the kernel skips the address copy-out.
/// Caller must have already awaited `socket.readable()`.
///
/// On non-Linux platforms this returns `Err(WouldBlock)` so the caller
/// falls back to the single-packet `try_recv` path.
#[cfg(target_os = "linux")]
pub(crate) fn recv_batch_connected(socket: &UdpSocket, bufs: &mut BatchBufs) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    socket.try_io(Interest::READABLE, || {
        // Build BATCH_SIZE iovec + mmsghdr arrays on the stack. No
        // sockaddr_storage array: the socket is connected, so the
        // source is implied and msg_name stays NULL.
        let fd = socket.as_raw_fd();
        let mut iovs: [libc::iovec; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };
        let mut hdrs: [libc::mmsghdr; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };

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
            hdrs[i].msg_hdr.msg_iov = std::ptr::from_mut::<libc::iovec>(&mut iovs[i]);
            hdrs[i].msg_hdr.msg_iovlen = 1;
        }

        // MSG_DONTWAIT: defensive, mirrors `recv_batch` — the fd is
        // already non-blocking. The `as _` cast is REQUIRED: the flags
        // parameter is `c_int` on glibc but `u32` on musl.
        let batch_count = libc::c_uint::try_from(BATCH_SIZE).unwrap_or(libc::c_uint::MAX);
        let rc = unsafe {
            libc::recvmmsg(
                fd,
                hdrs.as_mut_ptr(),
                batch_count,
                libc::MSG_DONTWAIT as _,
                std::ptr::null_mut(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        // rc ≥ 0 checked above; on 64-bit usize is wider than c_int.
        let n = usize::try_from(rc).unwrap_or(0);
        for (len_slot, hdr) in bufs.lens.iter_mut().zip(hdrs.iter()).take(n) {
            // msg_len ≤ iov_len (which is itself usize); no truncation
            // possible on any supported target.
            *len_slot = usize::try_from(hdr.msg_len).unwrap_or(0);
        }
        Ok(n)
    })
}

/// Non-Linux fallback: callers degrade to single-packet `try_recv`.
#[cfg(not(target_os = "linux"))]
pub(crate) fn recv_batch_connected(
    _socket: &UdpSocket,
    _bufs: &mut BatchBufs,
) -> io::Result<usize> {
    Err(io::Error::from(io::ErrorKind::WouldBlock))
}

/// Send up to `payloads.len()` datagrams in one syscall on an
/// UNconnected UDP socket (the shared listener socket on the reply
/// path), addressing message `i` to `dests[i]` via `msg_name`.
/// `payloads` and `dests` must be the same length; at most
/// `BATCH_SIZE` messages go out per call. Returns the number of
/// datagrams accepted by the kernel. Caller must have already awaited
/// `socket.writable()` (or, on the demux reply path, be prepared to
/// treat `WouldBlock` as a drop).
///
/// Partial success is possible: the kernel may accept a prefix and
/// then hit SO_SNDBUF pressure; we report the prefix length and the
/// caller decides what to do with the tail (the demux reply path drops
/// it — same semantics as the single-packet WouldBlock drop, FR-008
/// step e).
#[cfg(target_os = "linux")]
pub(crate) fn send_batch_to(
    socket: &UdpSocket,
    payloads: &[&[u8]],
    dests: &[SocketAddr],
) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    debug_assert_eq!(payloads.len(), dests.len());
    if payloads.is_empty() {
        return Ok(0);
    }
    socket.try_io(Interest::WRITABLE, || {
        let fd = socket.as_raw_fd();
        let len = payloads.len().min(dests.len()).min(BATCH_SIZE);
        let mut iovs: [libc::iovec; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };
        let mut addrs: [libc::sockaddr_storage; BATCH_SIZE] =
            unsafe { MaybeUninit::zeroed().assume_init() };
        let mut hdrs: [libc::mmsghdr; BATCH_SIZE] = unsafe { MaybeUninit::zeroed().assume_init() };

        for i in 0..len {
            iovs[i] = libc::iovec {
                iov_base: payloads[i].as_ptr().cast::<libc::c_void>().cast_mut(),
                iov_len: payloads[i].len(),
            };
            // Unconnected socket: encode the per-message destination
            // into msg_name (inverse of `sockaddr_to_socketaddr`).
            let namelen = socketaddr_to_sockaddr(dests[i], &mut addrs[i]);
            hdrs[i].msg_hdr.msg_name =
                std::ptr::from_mut::<libc::sockaddr_storage>(&mut addrs[i]).cast();
            hdrs[i].msg_hdr.msg_namelen = namelen;
            hdrs[i].msg_hdr.msg_iov = std::ptr::from_mut::<libc::iovec>(&mut iovs[i]);
            hdrs[i].msg_hdr.msg_iovlen = 1;
        }

        // `len ≤ BATCH_SIZE` (32) by the `.min()` above; fits in
        // c_uint. The `as _` cast on MSG_DONTWAIT is REQUIRED: the
        // flags parameter is `c_int` on glibc but `u32` on musl.
        let send_count = libc::c_uint::try_from(len).unwrap_or(libc::c_uint::MAX);
        let rc =
            unsafe { libc::sendmmsg(fd, hdrs.as_mut_ptr(), send_count, libc::MSG_DONTWAIT as _) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        // rc ≥ 0 checked above; usize ≥ c_int on every supported target.
        Ok(usize::try_from(rc).unwrap_or(0))
    })
}

/// Non-Linux fallback: callers degrade to single-packet `try_send_to`.
#[cfg(not(target_os = "linux"))]
pub(crate) fn send_batch_to(
    _socket: &UdpSocket,
    _payloads: &[&[u8]],
    _dests: &[SocketAddr],
) -> io::Result<usize> {
    Err(io::Error::from(io::ErrorKind::WouldBlock))
}

/// Encode a Rust [`SocketAddr`] into a zeroed `sockaddr_storage`
/// (inverse of [`sockaddr_to_socketaddr`]). Returns the number of
/// meaningful bytes (`sizeof(sockaddr_in)` / `sizeof(sockaddr_in6)`)
/// for use as `msg_namelen`.
#[cfg(target_os = "linux")]
fn socketaddr_to_sockaddr(
    addr: SocketAddr,
    storage: &mut libc::sockaddr_storage,
) -> libc::socklen_t {
    match addr {
        SocketAddr::V4(v4) => {
            // SAFETY: sockaddr_in fits inside sockaddr_storage by
            // definition; reinterpreting the storage as sockaddr_in is
            // the standard POSIX pattern (mirror of the decoder).
            let sin: &mut libc::sockaddr_in =
                unsafe { &mut *std::ptr::from_mut::<libc::sockaddr_storage>(storage).cast() };
            sin.sin_family =
                libc::sa_family_t::try_from(libc::AF_INET).unwrap_or(libc::sa_family_t::MAX);
            sin.sin_port = v4.port().to_be();
            sin.sin_addr.s_addr = u32::from(*v4.ip()).to_be();
            libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in>())
                .unwrap_or(libc::socklen_t::MAX)
        }
        SocketAddr::V6(v6) => {
            // SAFETY: sockaddr_in6 fits inside sockaddr_storage.
            let sin6: &mut libc::sockaddr_in6 =
                unsafe { &mut *std::ptr::from_mut::<libc::sockaddr_storage>(storage).cast() };
            sin6.sin6_family =
                libc::sa_family_t::try_from(libc::AF_INET6).unwrap_or(libc::sa_family_t::MAX);
            sin6.sin6_port = v6.port().to_be();
            sin6.sin6_flowinfo = v6.flowinfo();
            sin6.sin6_addr.s6_addr = v6.ip().octets();
            sin6.sin6_scope_id = v6.scope_id();
            libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in6>())
                .unwrap_or(libc::socklen_t::MAX)
        }
    }
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
    if family == libc::AF_INET && (len as usize) >= std::mem::size_of::<libc::sockaddr_in>() {
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

// Portable tests that compile and run on every platform. They exercise
// the target-agnostic `BatchBufs::slot` accessor and the non-Linux
// `recv_batch` / `send_batch_connected` fallbacks, which the Linux-only
// `tests` module below cannot reach.
#[cfg(test)]
mod tests_portable {
    use super::*;
    use std::net::Ipv4Addr;

    // Manually populate one slot the way `recv_batch` would on Linux,
    // then assert `slot()` lifts the payload and source back out. This
    // covers the target-agnostic accessor without a real `recvmmsg`.
    #[test]
    fn slot_returns_payload_and_source() {
        let mut bufs = BatchBufs::new();
        let src: SocketAddr = (Ipv4Addr::new(10, 0, 0, 7), 4321).into();
        let payload = [1u8, 2, 3, 4, 5];

        // Write into the second slot to prove the offset math is used.
        let i = 1usize;
        let off = i * UDP_BUFFER_BYTES;
        bufs.slots[off..off + payload.len()].copy_from_slice(&payload);
        bufs.lens[i] = payload.len();
        bufs.addrs[i] = Some(src);

        let (got_payload, got_src) = bufs.slot(i);
        assert_eq!(got_payload, &payload);
        assert_eq!(got_src, src);
    }

    // A freshly populated slot of length zero yields an empty payload
    // slice (the `off..off + 0` branch), still returning the source.
    #[test]
    fn slot_with_zero_length_payload_is_empty() {
        let mut bufs = BatchBufs::new();
        let src: SocketAddr = (Ipv4Addr::LOCALHOST, 9).into();
        bufs.lens[0] = 0;
        bufs.addrs[0] = Some(src);

        let (payload, got_src) = bufs.slot(0);
        assert!(payload.is_empty());
        assert_eq!(got_src, src);
    }

    // The fresh arena is fully zeroed and sized BATCH_SIZE * slot bytes.
    #[test]
    fn new_allocates_zeroed_arena() {
        let bufs = BatchBufs::new();
        assert_eq!(bufs.slots.len(), BATCH_SIZE * UDP_BUFFER_BYTES);
        assert!(bufs.slots.iter().all(|&b| b == 0));
        assert_eq!(bufs.lens, [0usize; BATCH_SIZE]);
        assert!(bufs.addrs.iter().all(Option::is_none));
    }

    // `slot()` panics when the source address was never populated.
    #[test]
    #[should_panic(expected = "slot populated by recv_batch")]
    fn slot_panics_when_source_missing() {
        let bufs = BatchBufs::new();
        let _ = bufs.slot(0);
    }

    // Manually populate a slot the way `recv_batch_connected` would on
    // Linux (lens only, no addrs) and assert `payload()` lifts the
    // bytes back out without touching the address array.
    #[test]
    fn payload_returns_slice_without_address() {
        let mut bufs = BatchBufs::new();
        let payload = [9u8, 8, 7];

        // Use the third slot to prove the offset math is applied.
        let i = 2usize;
        let off = i * UDP_BUFFER_BYTES;
        bufs.slots[off..off + payload.len()].copy_from_slice(&payload);
        bufs.lens[i] = payload.len();
        // addrs deliberately stays None — connected recv never sets it.

        assert_eq!(bufs.payload(i), &payload);
        assert!(bufs.addrs[i].is_none());
    }

    // `payload()` on a zero-length slot yields an empty slice.
    #[test]
    fn payload_with_zero_length_is_empty() {
        let mut bufs = BatchBufs::new();
        bufs.lens[0] = 0;
        assert!(bufs.payload(0).is_empty());
    }

    // On non-Linux platforms both batch helpers are stubs that always
    // report WouldBlock so the caller falls back to single-packet I/O.
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn fallbacks_return_would_block_on_non_linux() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();

        let mut bufs = BatchBufs::new();
        let recv_err = recv_batch(&socket, &mut bufs).expect_err("non-Linux recv stub");
        assert_eq!(recv_err.kind(), io::ErrorKind::WouldBlock);

        let payloads: [&[u8]; 1] = [b"x"];
        let send_err = send_batch_connected(&socket, &payloads).expect_err("non-Linux send stub");
        assert_eq!(send_err.kind(), io::ErrorKind::WouldBlock);
    }

    // The v1.6+ reply-path helpers are also WouldBlock stubs on
    // non-Linux platforms so the demux keeps its per-packet fallback.
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn reply_path_fallbacks_return_would_block_on_non_linux() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();

        let mut bufs = BatchBufs::new();
        let recv_err =
            recv_batch_connected(&socket, &mut bufs).expect_err("non-Linux connected recv stub");
        assert_eq!(recv_err.kind(), io::ErrorKind::WouldBlock);

        let payloads: [&[u8]; 1] = [b"x"];
        let dests = [SocketAddr::from((Ipv4Addr::LOCALHOST, 9))];
        let send_err =
            send_batch_to(&socket, &payloads, &dests).expect_err("non-Linux send_to stub");
        assert_eq!(send_err.kind(), io::ErrorKind::WouldBlock);
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
            let (n, _) = tokio::time::timeout(Duration::from_secs(1), server.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
            sizes.push(n);
        }
        sizes.sort_unstable();
        assert_eq!(sizes, vec![1, 2, 3]);
    }

    // The SocketAddr → sockaddr_storage encoder round-trips through the
    // existing decoder for both address families, ports, and the v6
    // flowinfo / scope_id extras.
    #[test]
    fn socketaddr_encoder_round_trips_through_decoder() {
        use std::net::{Ipv6Addr, SocketAddrV6};

        let v4: SocketAddr = (Ipv4Addr::new(192, 168, 1, 42), 4242).into();
        let mut storage: libc::sockaddr_storage = unsafe { MaybeUninit::zeroed().assume_init() };
        let len = socketaddr_to_sockaddr(v4, &mut storage);
        assert_eq!(
            len as usize,
            std::mem::size_of::<libc::sockaddr_in>(),
            "v4 namelen"
        );
        assert_eq!(sockaddr_to_socketaddr(&storage, len), Some(v4));

        let v6 = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::new(0xfe80, 0, 0, 0, 0x1, 0x2, 0x3, 0x4),
            5353,
            7,
            3,
        ));
        let mut storage6: libc::sockaddr_storage = unsafe { MaybeUninit::zeroed().assume_init() };
        let len6 = socketaddr_to_sockaddr(v6, &mut storage6);
        assert_eq!(
            len6 as usize,
            std::mem::size_of::<libc::sockaddr_in6>(),
            "v6 namelen"
        );
        assert_eq!(sockaddr_to_socketaddr(&storage6, len6), Some(v6));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recv_batch_connected_reads_multiple_datagrams() {
        let (peer, peer_addr) = bind_loopback().await;
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(peer_addr).await.unwrap();
        let upstream_local = upstream.local_addr().unwrap();

        for i in 0..4u8 {
            peer.send_to(&[i; 2], upstream_local).await.unwrap();
        }
        // Let the kernel deliver them to the connected socket's queue.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut bufs = BatchBufs::new();
        upstream.readable().await.unwrap();
        let n = recv_batch_connected(&upstream, &mut bufs).expect("recvmmsg succeeds");
        assert!(n >= 1, "should read at least one datagram, got 0");
        for i in 0..n {
            let payload = bufs.payload(i);
            assert_eq!(payload.len(), 2);
            assert_eq!(payload[0], payload[1]);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_batch_to_delivers_to_per_message_destinations() {
        let (listener, _listener_addr) = bind_loopback().await;
        let (primary, primary_addr) = bind_loopback().await;
        let (secondary, secondary_addr) = bind_loopback().await;

        let payloads: Vec<&[u8]> = vec![b"aa", b"bbbb", b"c"];
        let dests = [primary_addr, secondary_addr, primary_addr];
        listener.writable().await.unwrap();
        let n = send_batch_to(&listener, &payloads, &dests).expect("sendmmsg succeeds");
        assert_eq!(n, 3);

        let mut buf = [0u8; 16];
        let mut primary_sizes = Vec::new();
        for _ in 0..2 {
            let (n, _) = tokio::time::timeout(Duration::from_secs(1), primary.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
            primary_sizes.push(n);
        }
        primary_sizes.sort_unstable();
        assert_eq!(primary_sizes, vec![1, 2]);

        let (n_secondary, _) =
            tokio::time::timeout(Duration::from_secs(1), secondary.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(n_secondary, 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_batch_to_empty_is_ok_zero() {
        let (listener, _addr) = bind_loopback().await;
        let payloads: [&[u8]; 0] = [];
        let dests: [SocketAddr; 0] = [];
        assert_eq!(send_batch_to(&listener, &payloads, &dests).unwrap(), 0);
    }
}
