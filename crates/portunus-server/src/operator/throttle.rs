//! Local auth attempt throttling primitives.

use chrono::{DateTime, Duration, Utc};

pub(crate) const LOCK_AFTER_FAILURES: u32 = 5;
pub(crate) const LOCKOUT_SECONDS: i64 = 60;
pub(crate) const MAX_LOCKOUT_SECONDS: i64 = 15 * 60;
#[allow(dead_code)]
pub(crate) const UNKNOWN_AUTH_SUBJECT: &str = "_unknown";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum AuthThrottleAction {
    Login,
    Onboarding,
    PasswordReset,
}

impl AuthThrottleAction {
    #[must_use]
    pub(crate) const fn as_db_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Onboarding => "onboarding",
            Self::PasswordReset => "password_reset",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ThrottleDecision {
    pub(crate) failures: u32,
    pub(crate) first_failed_at: Option<DateTime<Utc>>,
    pub(crate) last_failed_at: Option<DateTime<Utc>>,
    pub(crate) locked_until: Option<DateTime<Utc>>,
}

impl ThrottleDecision {
    pub(crate) fn record_failure(&mut self, now: DateTime<Utc>) {
        if self
            .locked_until
            .is_some_and(|locked_until| locked_until <= now)
        {
            *self = Self::default();
        }

        self.failures = self.failures.saturating_add(1);
        self.first_failed_at.get_or_insert(now);
        self.last_failed_at = Some(now);

        if self.failures < LOCK_AFTER_FAILURES {
            self.locked_until = None;
            return;
        }

        let shift = (self.failures - LOCK_AFTER_FAILURES).min(10);
        let lockout_seconds = LOCKOUT_SECONDS
            .saturating_mul(1_i64 << shift)
            .min(MAX_LOCKOUT_SECONDS);
        self.locked_until = Some(now + Duration::seconds(lockout_seconds));
    }

    #[must_use]
    pub(crate) fn is_locked(&self, now: DateTime<Utc>) -> bool {
        self.locked_until
            .is_some_and(|locked_until| locked_until > now)
    }

    #[must_use]
    pub(crate) fn effective_at(&self, now: DateTime<Utc>) -> Self {
        let mut state = self.clone();
        if !state.is_locked(now) {
            state.locked_until = None;
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn repeated_failures_trigger_bounded_lockout() {
        let now = Utc.with_ymd_and_hms(2026, 5, 11, 9, 30, 0).unwrap();
        let mut state = ThrottleDecision::default();
        for _ in 0..5 {
            state.record_failure(now);
        }
        assert!(state.locked_until.is_some());
    }

    #[test]
    fn repeated_failures_cap_lockout_duration() {
        let now = Utc.with_ymd_and_hms(2026, 5, 11, 9, 30, 0).unwrap();
        let mut state = ThrottleDecision::default();
        for _ in 0..20 {
            state.record_failure(now);
        }

        assert_eq!(
            state.locked_until,
            Some(now + Duration::seconds(MAX_LOCKOUT_SECONDS))
        );
    }

    #[test]
    fn expired_lockout_starts_new_burst() {
        let now = Utc.with_ymd_and_hms(2026, 5, 11, 9, 30, 0).unwrap();
        let mut state = ThrottleDecision::default();
        for _ in 0..LOCK_AFTER_FAILURES {
            state.record_failure(now);
        }

        let after_lockout = state.locked_until.unwrap() + Duration::seconds(1);
        state.record_failure(after_lockout);

        assert_eq!(state.failures, 1);
        assert_eq!(state.first_failed_at, Some(after_lockout));
        assert_eq!(state.last_failed_at, Some(after_lockout));
        assert_eq!(state.locked_until, None);
    }
}
