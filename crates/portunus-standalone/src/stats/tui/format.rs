//! Human-readable byte / rate formatters.

use ratatui::style::Color;

use super::probe::ProbeSample;

#[must_use]
pub fn fmt_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    if b < 1024 {
        return format!("{b}   B");
    }
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

/// Format a probe sample for the Targets panel: the text to display and
/// the colour to display it in. Green `< 50 ms`, yellow `< 200 ms`, red
/// otherwise; timeouts and failures are red.
#[must_use]
pub fn fmt_rtt(sample: ProbeSample) -> (String, Color) {
    match sample {
        ProbeSample::Ok(d) => {
            let ms = d.as_millis();
            let color = if ms < 50 {
                Color::Green
            } else if ms < 200 {
                Color::Yellow
            } else {
                Color::Red
            };
            (format!("{ms}ms"), color)
        }
        ProbeSample::Timeout => ("timeout".to_string(), Color::Red),
        ProbeSample::Failed => ("down".to_string(), Color::Red),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_zero() {
        assert_eq!(fmt_bytes(0).trim(), "0   B");
    }

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

    #[test]
    fn rtt_ok_thresholds() {
        use std::time::Duration;
        assert_eq!(
            fmt_rtt(ProbeSample::Ok(Duration::from_millis(10))),
            ("10ms".to_string(), Color::Green)
        );
        assert_eq!(
            fmt_rtt(ProbeSample::Ok(Duration::from_millis(120))).1,
            Color::Yellow
        );
        assert_eq!(
            fmt_rtt(ProbeSample::Ok(Duration::from_millis(500))).1,
            Color::Red
        );
    }

    #[test]
    fn rtt_timeout_and_failed_are_red() {
        assert_eq!(
            fmt_rtt(ProbeSample::Timeout),
            ("timeout".to_string(), Color::Red)
        );
        assert_eq!(
            fmt_rtt(ProbeSample::Failed),
            ("down".to_string(), Color::Red)
        );
    }
}
