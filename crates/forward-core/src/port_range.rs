//! Contiguous port range used by both single-port (degenerate, size 1)
//! and range forwarding rules. Spec: 002-port-range-forward
//! `data-model.md` § `PortRange`.
//!
//! All validation lives here so the operator HTTP handler, the CLI,
//! the server rule store, and the client forwarder share one
//! validator (DRY + reduces the chance one path silently allows a
//! shape another rejects).

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PortRange {
    start: u16,
    end: u16,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PortRangeError {
    /// `start > end`, or one side is 0 (port 0 is not a real listening port).
    #[error("range_inverted: start={start} > end={end}")]
    Inverted { start: u16, end: u16 },

    /// `start == 0`. Port 0 isn't a real listening port; a `bind` to
    /// port 0 means "let the OS pick", which is meaningless for a
    /// forwarding rule.
    #[error("range_out_of_bounds: port 0 is not a valid listening port")]
    OutOfBounds,

    /// Listen and target ranges have different sizes.
    #[error("range_length_mismatch: listen_len={listen_len} target_len={target_len}")]
    LengthMismatch { listen_len: u32, target_len: u32 },

    /// Range size > server-configured cap.
    #[error("exceeds_cap: requested={requested} cap={cap}")]
    ExceedsCap { requested: u32, cap: u32 },
}

impl PortRange {
    /// Single-port range — `start == end == port`. Used to lift
    /// v0.1.0 single-port rules into the range path.
    #[must_use]
    pub const fn single(port: u16) -> Self {
        Self {
            start: port,
            end: port,
        }
    }

    /// Validating constructor.
    pub const fn new(start: u16, end: u16) -> Result<Self, PortRangeError> {
        if start == 0 {
            return Err(PortRangeError::OutOfBounds);
        }
        if start > end {
            return Err(PortRangeError::Inverted { start, end });
        }
        Ok(Self { start, end })
    }

    /// Validate listen / target as a coupled pair. Length mismatch is
    /// checked here so callers don't have to re-check.
    pub fn pair(listen: Self, target: Self) -> Result<(Self, Self), PortRangeError> {
        if listen.len() != target.len() {
            return Err(PortRangeError::LengthMismatch {
                listen_len: listen.len(),
                target_len: target.len(),
            });
        }
        Ok((listen, target))
    }

    #[must_use]
    pub const fn start(self) -> u16 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u16 {
        self.end
    }

    /// Number of ports in the range. Returned as `u32` to avoid the
    /// edge-case overflow at `1..=65535` (size 65535 is in-bounds for
    /// `u16` but `(end - start) + 1` could overflow in u16 arithmetic).
    #[must_use]
    pub const fn len(self) -> u32 {
        (self.end as u32) - (self.start as u32) + 1
    }

    /// Always `false` — `new` rejects `start > end`, so an empty range
    /// cannot be constructed. Provided for `clippy::len_without_is_empty`.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn is_empty(self) -> bool {
        false
    }

    #[must_use]
    pub const fn contains(self, port: u16) -> bool {
        port >= self.start && port <= self.end
    }

    /// Intervals `[a, b]` and `[c, d]` overlap iff `a <= d && c <= b`.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    pub fn iter(self) -> impl Iterator<Item = u16> {
        self.start..=self.end
    }

    /// Compute the target-side port for a listen-side port using the
    /// same-offset mapping. Returns `None` if `listen_port` isn't
    /// inside `listen` (caller bug).
    #[must_use]
    pub fn target_for(listen_port: u16, listen: Self, target: Self) -> Option<u16> {
        if !listen.contains(listen_port) {
            return None;
        }
        // listen.len() == target.len() is the caller's invariant; if
        // they pass mismatched ranges the result is "wrong but won't
        // panic" (offset arithmetic stays in u32). Callers SHOULD
        // validate via `pair` first.
        let offset = u32::from(listen_port) - u32::from(listen.start);
        // `offset < listen.len() <= 65535`, so the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        let offset_u16 = offset as u16;
        Some(target.start + offset_u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_start_greater_than_end() {
        assert_eq!(
            PortRange::new(30050, 30000),
            Err(PortRangeError::Inverted {
                start: 30050,
                end: 30000
            })
        );
    }

    #[test]
    fn new_rejects_start_zero() {
        assert_eq!(PortRange::new(0, 100), Err(PortRangeError::OutOfBounds));
    }

    #[test]
    fn new_accepts_single_port() {
        let r = PortRange::new(18080, 18080).unwrap();
        assert_eq!(r.len(), 1);
        assert!(r.contains(18080));
        assert!(!r.contains(18081));
    }

    #[test]
    fn new_accepts_max_range() {
        let r = PortRange::new(1, 65535).unwrap();
        assert_eq!(r.len(), 65535);
    }

    #[test]
    fn new_accepts_top_single() {
        let r = PortRange::new(65535, 65535).unwrap();
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn pair_rejects_length_mismatch() {
        let listen = PortRange::new(30000, 30050).unwrap();
        let target = PortRange::new(40000, 40049).unwrap(); // off by one
        assert_eq!(
            PortRange::pair(listen, target),
            Err(PortRangeError::LengthMismatch {
                listen_len: 51,
                target_len: 50
            })
        );
    }

    #[test]
    fn pair_accepts_equal_lengths() {
        let listen = PortRange::new(30000, 30050).unwrap();
        let target = PortRange::new(40000, 40050).unwrap();
        let (l, t) = PortRange::pair(listen, target).unwrap();
        assert_eq!(l.len(), t.len());
    }

    #[test]
    fn overlaps_symmetric_disjoint_adjacent_nested() {
        let base = PortRange::new(30000, 30010).unwrap();
        let overlapping = PortRange::new(30005, 30015).unwrap();
        let adjacent = PortRange::new(30011, 30020).unwrap();
        let nested = PortRange::new(30002, 30008).unwrap();
        let disjoint = PortRange::new(40000, 40005).unwrap();

        assert!(base.overlaps(overlapping));
        assert!(overlapping.overlaps(base));
        assert!(!base.overlaps(adjacent));
        assert!(!adjacent.overlaps(base));
        assert!(base.overlaps(nested));
        assert!(nested.overlaps(base));
        assert!(!base.overlaps(disjoint));
    }

    #[test]
    fn target_for_full_range() {
        let listen = PortRange::new(30000, 30050).unwrap();
        let target = PortRange::new(40000, 40050).unwrap();
        assert_eq!(PortRange::target_for(30000, listen, target), Some(40000));
        assert_eq!(PortRange::target_for(30025, listen, target), Some(40025));
        assert_eq!(PortRange::target_for(30050, listen, target), Some(40050));
        // Out-of-range listen port → None.
        assert_eq!(PortRange::target_for(29999, listen, target), None);
        assert_eq!(PortRange::target_for(30051, listen, target), None);
    }

    #[test]
    fn target_for_same_offset() {
        // 30000-30050 → 30000-30050 maps each port to itself.
        let listen = PortRange::new(30000, 30050).unwrap();
        let target = listen;
        for p in 30000..=30050 {
            assert_eq!(PortRange::target_for(p, listen, target), Some(p));
        }
    }

    #[test]
    fn iter_covers_full_range() {
        let r = PortRange::new(30000, 30002).unwrap();
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![30000, 30001, 30002]);
    }
}
