//! 013-traffic-quotas v1.4.0: per-(user, client) monthly traffic quota
//! data model. Period anniversary computation (calendar month, clamped
//! to last day) is implemented here so it can be unit-tested without
//! the SQLite layer. See design spec §3.3.

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use portunus_proto::v1 as proto;

pub mod aggregator;
pub mod cache;
pub mod rollover;
pub mod rollup;
pub mod samples;
pub mod store;

/// One row of `traffic_quotas`. Mirrors the schema 1:1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrafficQuotaRow {
    pub user_id: String,
    pub client_name: String,
    pub monthly_bytes: i64,
    pub billing_anchor: i64,                 // unix sec UTC
    pub current_period_started_at: i64,      // unix sec UTC
    pub current_period_bytes_used: i64,
    pub exhausted_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TrafficQuotaRow {
    #[must_use]
    pub fn budget_remaining(&self) -> i64 {
        self.monthly_bytes
            .saturating_sub(self.current_period_bytes_used)
    }
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted_at.is_some()
    }
}

/// Compute the start instant of period number `n` (n=0 is the anchor
/// itself) from the original billing anchor. Every period start is
/// computed relative to the original anchor so the day-of-month never
/// drifts (Jan 31 -> Feb 28/29 -> Mar 31, not Jan 31 -> Feb 28 -> Mar
/// 28). All times are UTC.
#[must_use]
pub fn period_start_at(billing_anchor: DateTime<Utc>, n: u32) -> DateTime<Utc> {
    let anchor_day = billing_anchor.day();
    let (h, m, s) = (
        billing_anchor.hour(),
        billing_anchor.minute(),
        billing_anchor.second(),
    );

    let total_months = i64::from(billing_anchor.year()) * 12
        + i64::from(billing_anchor.month())
        - 1
        + i64::from(n);
    let target_year = i32::try_from(total_months / 12).expect("year fits i32");
    let target_month = u32::try_from((total_months % 12) + 1).expect("month 1..=12");
    let max_day = last_day_of_month(target_year, target_month);
    let day = anchor_day.min(max_day);

    Utc.with_ymd_and_hms(target_year, target_month, day, h, m, s)
        .single()
        .expect("period_start_at constructs valid date")
}

/// Compute the period-end timestamp (next period's start). Returns
/// `i64::MAX` if the chain to `started` can't be located within 1000
/// years — preferred over refusing the read.
#[must_use]
pub fn compute_period_end(billing_anchor: i64, started: i64) -> i64 {
    let Some(anchor) = Utc.timestamp_opt(billing_anchor, 0).single() else {
        return i64::MAX;
    };
    let Some(start_dt) = Utc.timestamp_opt(started, 0).single() else {
        return i64::MAX;
    };
    let mut n: u32 = 0;
    while n < 12_000 {
        if period_start_at(anchor, n) == start_dt {
            return period_start_at(anchor, n + 1).timestamp();
        }
        n += 1;
    }
    i64::MAX
}

/// Build a `ServerMessage` carrying a `TrafficQuotaUpdate{SET}` for
/// `row`. Shared by CRUD push (quota_http.rs), period rollover
/// (rollover.rs), aggregator exhaust handling, and reconnect replay
/// (grpc/service.rs).
#[must_use]
pub fn make_traffic_quota_set_msg(row: &TrafficQuotaRow, request_id: String) -> proto::ServerMessage {
    let ends_at = compute_period_end(row.billing_anchor, row.current_period_started_at);
    proto::ServerMessage {
        payload: Some(proto::server_message::Payload::TrafficQuotaUpdate(
            proto::TrafficQuotaUpdate {
                request_id,
                user_id: row.user_id.clone(),
                client_name: row.client_name.clone(),
                action: proto::TrafficQuotaAction::Set as i32,
                state: Some(proto::TrafficQuotaState {
                    monthly_bytes: row.monthly_bytes,
                    budget_remaining_bytes: row.budget_remaining(),
                    period_started_at_unix_sec: row.current_period_started_at,
                    period_ends_at_unix_sec: ends_at,
                    exhausted: row.is_exhausted(),
                }),
            },
        )),
    }
}

/// Build a `ServerMessage` carrying a `TrafficQuotaUpdate{REMOVE}`.
#[must_use]
pub fn make_traffic_quota_remove_msg(
    user_id: String,
    client_name: String,
    request_id: String,
) -> proto::ServerMessage {
    proto::ServerMessage {
        payload: Some(proto::server_message::Payload::TrafficQuotaUpdate(
            proto::TrafficQuotaUpdate {
                request_id,
                user_id,
                client_name,
                action: proto::TrafficQuotaAction::Remove as i32,
                state: None,
            },
        )),
    }
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_of_next = Utc
        .with_ymd_and_hms(ny, nm, 1, 0, 0, 0)
        .single()
        .expect("first-of-month is valid");
    (first_of_next - chrono::Duration::days(1)).day()
}

/// Given the current state, advance `current_period_started_at` to the
/// latest anchor-period start <= `now`. Returns the new period start
/// instant if the row needs to be advanced. The caller is responsible
/// for zeroing `bytes_used`, clearing `exhausted_at`, and persisting.
#[must_use]
pub fn advance_period_if_due(
    billing_anchor: DateTime<Utc>,
    current_period_started_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    // Find n such that period_start(n) == current_period_started_at.
    // Bounded scan from anchor.
    let mut n = 0u32;
    loop {
        let p = period_start_at(billing_anchor, n);
        if p == current_period_started_at {
            break;
        }
        if p > current_period_started_at {
            // Anchor was changed or current_period_started_at was
            // manually edited; rebase to the previous period.
            n = n.saturating_sub(1);
            break;
        }
        n += 1;
        if n > 12_000 {
            // Sanity break — 1000 years of monthly periods.
            break;
        }
    }
    // Advance n while next period start is <= now.
    let mut advanced = false;
    loop {
        let next = period_start_at(billing_anchor, n + 1);
        if next > now {
            break;
        }
        n += 1;
        advanced = true;
        if n > 12_000 {
            break;
        }
    }
    if advanced {
        Some(period_start_at(billing_anchor, n))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(yyyy: i32, mm: u32, dd: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(yyyy, mm, dd, 0, 0, 0).single().unwrap()
    }

    #[test]
    fn period_start_zero_returns_anchor() {
        let a = anchor(2026, 1, 15);
        assert_eq!(period_start_at(a, 0), a);
    }

    #[test]
    fn period_start_jan31_clamps_february() {
        let a = anchor(2026, 1, 31);
        assert_eq!(period_start_at(a, 0), a);
        assert_eq!(period_start_at(a, 1), anchor(2026, 2, 28));
        assert_eq!(period_start_at(a, 2), anchor(2026, 3, 31));
        assert_eq!(period_start_at(a, 3), anchor(2026, 4, 30));
        assert_eq!(period_start_at(a, 12), anchor(2027, 1, 31));
    }

    #[test]
    fn period_start_feb29_leap_year() {
        // 2024 is a leap year. 2025/2026/2027 are not. 2028 is.
        let a = anchor(2024, 2, 29);
        assert_eq!(period_start_at(a, 12), anchor(2025, 2, 28));
        assert_eq!(period_start_at(a, 24), anchor(2026, 2, 28));
        assert_eq!(period_start_at(a, 48), anchor(2028, 2, 29));
    }

    #[test]
    fn advance_period_skips_many_months() {
        let a = anchor(2026, 1, 15);
        let started = period_start_at(a, 0);
        let now = anchor(2026, 5, 20);
        let new_start = advance_period_if_due(a, started, now).unwrap();
        assert_eq!(new_start, anchor(2026, 5, 15));
    }

    #[test]
    fn advance_period_returns_none_when_not_due() {
        let a = anchor(2026, 1, 15);
        let started = period_start_at(a, 3); // Apr 15
        let now = anchor(2026, 5, 14); // Day before period rolls
        assert!(advance_period_if_due(a, started, now).is_none());
    }

    #[test]
    fn advance_period_exactly_at_boundary_rolls() {
        let a = anchor(2026, 1, 15);
        let started = period_start_at(a, 0);
        let now = anchor(2026, 2, 15);
        let new_start = advance_period_if_due(a, started, now).unwrap();
        assert_eq!(new_start, anchor(2026, 2, 15));
    }

    #[test]
    fn budget_remaining_goes_negative_when_overused() {
        // Per design spec §4.2: budget_remaining_bytes "may be negative
        // when exhausted". The wire signal carries the magnitude.
        let r = TrafficQuotaRow {
            user_id: "u".into(),
            client_name: "c".into(),
            monthly_bytes: 100,
            billing_anchor: 0,
            current_period_started_at: 0,
            current_period_bytes_used: 200,
            exhausted_at: Some(1),
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(r.budget_remaining(), -100);
        assert!(r.is_exhausted());
    }
}
