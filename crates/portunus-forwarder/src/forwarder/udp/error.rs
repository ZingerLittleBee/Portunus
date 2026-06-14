//! Classification of UDP socket errors into eviction-or-drop actions
//! (FR-006 / FR-007). Pure function so unit tests can feed synthetic
//! `io::Error`s without provoking real ICMP / PMTU events.

use std::io;

/// What the caller should do with a connection after observing this error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpAction {
    /// `WouldBlock`: drop datagram, keep flow.
    WouldBlock,
    /// `EMSGSIZE`: drop datagram, keep flow (no PMTU bisection here).
    MessageTooLarge,
    /// Terminal/ICMP-class errors: evict flow.
    Evict,
    /// Other transient (`EINTR` etc.): drop datagram, keep flow.
    Transient,
}

#[must_use]
pub fn classify_udp_error(e: &io::Error) -> UdpAction {
    use io::ErrorKind;
    match e.kind() {
        ErrorKind::WouldBlock => UdpAction::WouldBlock,
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionAborted
        | ErrorKind::ConnectionReset
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable => UdpAction::Evict,
        ErrorKind::Interrupted => UdpAction::Transient,
        _ => {
            // EMSGSIZE has no stable ErrorKind on stable Rust; check os
            // errno when available.
            if let Some(raw) = e.raw_os_error() {
                if raw == libc::EMSGSIZE {
                    return UdpAction::MessageTooLarge;
                }
                if raw == libc::EHOSTUNREACH
                    || raw == libc::ENETUNREACH
                    || raw == libc::ECONNREFUSED
                {
                    return UdpAction::Evict;
                }
            }
            UdpAction::Transient
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_from_raw(raw: i32) -> io::Error {
        io::Error::from_raw_os_error(raw)
    }

    #[test]
    fn wouldblock_is_would_block() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "x");
        assert_eq!(classify_udp_error(&e), UdpAction::WouldBlock);
    }

    #[test]
    fn econnrefused_evicts() {
        let e = err_from_raw(libc::ECONNREFUSED);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn ehostunreach_evicts() {
        let e = err_from_raw(libc::EHOSTUNREACH);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn enetunreach_evicts() {
        let e = err_from_raw(libc::ENETUNREACH);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn econnaborted_evicts() {
        // ConnectionAborted shares the connected-UDP "peer gone" eviction
        // arm with ECONNREFUSED/ECONNRESET — assert it explicitly so a
        // future refactor of the match can't silently drop the arm.
        let e = err_from_raw(libc::ECONNABORTED);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn econnreset_evicts() {
        let e = err_from_raw(libc::ECONNRESET);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn emsgsize_message_too_large() {
        let e = err_from_raw(libc::EMSGSIZE);
        assert_eq!(classify_udp_error(&e), UdpAction::MessageTooLarge);
    }

    #[test]
    fn eintr_transient() {
        let e = err_from_raw(libc::EINTR);
        assert_eq!(classify_udp_error(&e), UdpAction::Transient);
    }
}
