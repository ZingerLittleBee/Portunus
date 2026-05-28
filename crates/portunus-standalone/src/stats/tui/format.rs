//! Human-readable byte / rate formatters.

#[must_use]
pub fn fmt_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    if b < 1024 { return format!("{b}   B"); }
    // cast_precision_loss: u64 → f64 is intentional here; display
    // precision of ±1 LSB is acceptable for a human-readable size label.
    #[allow(clippy::cast_precision_loss)]
    let mut f = b as f64;
    let mut i = 0;
    while f >= 1024.0 && i < UNITS.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    format!("{f:6.1} {}", UNITS[i])
}

#[must_use]
pub fn fmt_rate(bps: u64) -> String {
    format!("{}/s", fmt_bytes(bps).trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_zero() { assert_eq!(fmt_bytes(0).trim(), "0   B"); }

    #[test]
    fn bytes_kb() {
        let s = fmt_bytes(1500);
        assert!(s.contains("KB"), "got {s}");
    }

    #[test]
    fn rate_appends_per_second() {
        let s = fmt_rate(2048);
        assert!(s.ends_with("/s"));
        assert!(s.contains("KB"));
    }
}
