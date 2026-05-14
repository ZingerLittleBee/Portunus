//! In-memory SNI routing table. Spec 009-tls-sni-routing data-model.md §2.2.
//!
//! Built from a snapshot of `ClientRule`s sharing the same `(client,
//! listen_port)` SNI listener. Hot-path lookup; rebuilds happen in the
//! control task and are swapped into the listener via
//! `tokio::sync::watch::Sender::send_replace` (R-002 / R-007 in
//! `research.md`).
//!
//! Lookup precedence (data-model.md §2.2):
//!   1. exact match on the lowercased SNI hostname.
//!   2. longest-suffix wildcard match (single-label remainder, no
//!      inner dots in the remainder).
//!   3. fallback (the rule with `sni_pattern = None`), if present.
//!
//! Phase 3 (T037) implements steps 1 and 3. Phases 4 (T052/T053) and
//! 5 (T060) layer the wildcard slot and the fallback slot. The
//! current implementation already populates all three slots so US2
//! and US3 only have to add the lookup arms.

use std::collections::HashMap;
use std::sync::Arc;

use portunus_core::RuleId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SniMatchKind {
    Exact,
    Wildcard,
    Fallback,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SniMatch {
    Hit { rule_id: RuleId, kind: SniMatchKind },
    Miss,
}

/// Build error for `from_members`. The server-side overlap matrix
/// (data-model.md §Overlap) prevents these in normal flow; the
/// in-memory table panics in debug and reports the error in release
/// so a developer footgun (e.g. two None fallbacks slipping past the
/// store overlap check) gets caught early. The shared `Duplicate*`
/// prefix mirrors the operator-API codes (`conflict.sni_route_duplicate`
/// / `conflict.sni_fallback_duplicate`); renaming would lose that link.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, PartialEq, Eq)]
pub enum BuildError {
    DuplicateExact(String),
    DuplicateWildcard(String),
    DuplicateFallback,
}

#[derive(Debug, Default)]
pub struct SniRoutingTable {
    /// Exact hostname → rule_id. O(1) lookup.
    pub(crate) exact: HashMap<String, RuleId>,
    /// Wildcard suffixes (the part *after* `*.`), sorted longest-first.
    /// Populated in T052 (Phase 4 / US2).
    pub(crate) wildcards: Vec<(String, RuleId)>,
    /// At most one fallback (sni_pattern = NULL).
    /// Populated in T060 (Phase 5 / US3).
    pub(crate) fallback: Option<RuleId>,
}

impl SniRoutingTable {
    /// Build a table from a snapshot of group members.
    ///
    /// Each member is `(sni_pattern, rule_id)`:
    /// - `Some("api.example.com")` → exact slot.
    /// - `Some("*.example.com")` → wildcard slot (suffix is "example.com").
    /// - `None` → fallback slot.
    ///
    /// Patterns are assumed to be already lowercased + grammar-validated
    /// by the server (see `operator/http.rs::validate_sni_pattern`).
    /// We re-lowercase defensively for safety.
    pub fn from_members(members: &[(Option<&str>, RuleId)]) -> Result<Arc<Self>, BuildError> {
        let mut exact: HashMap<String, RuleId> = HashMap::with_capacity(members.len());
        let mut wildcards: Vec<(String, RuleId)> = Vec::new();
        let mut fallback: Option<RuleId> = None;

        for (pat, id) in members {
            match pat {
                None => {
                    if fallback.is_some() {
                        return Err(BuildError::DuplicateFallback);
                    }
                    fallback = Some(*id);
                }
                Some(p) => {
                    let lowered = p.to_ascii_lowercase();
                    if let Some(suffix) = lowered.strip_prefix("*.") {
                        if wildcards.iter().any(|(s, _)| s.as_str() == suffix) {
                            return Err(BuildError::DuplicateWildcard(suffix.to_string()));
                        }
                        wildcards.push((suffix.to_string(), *id));
                    } else {
                        match exact.entry(lowered) {
                            std::collections::hash_map::Entry::Occupied(e) => {
                                return Err(BuildError::DuplicateExact(e.key().clone()));
                            }
                            std::collections::hash_map::Entry::Vacant(e) => {
                                e.insert(*id);
                            }
                        }
                    }
                }
            }
        }

        // Longest-suffix-first ordering so the lookup walk picks
        // `*.team.example.com` before `*.example.com`.
        wildcards.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()));

        Ok(Arc::new(Self {
            exact,
            wildcards,
            fallback,
        }))
    }

    /// Look up an SNI value (or its absence). Hot-path target.
    ///
    /// `sni`:
    /// - `Some(host)` — peer sent a `server_name` extension.
    /// - `None` — peer sent a valid ClientHello with no SNI.
    ///
    /// Precedence: exact → wildcard (longest first) → fallback.
    /// `None` skips exact and wildcard (those require a host); a
    /// no-SNI peer can only land on the fallback.
    #[must_use] 
    pub fn lookup(&self, sni: Option<&str>) -> SniMatch {
        if let Some(host) = sni {
            // Lowercased + dot-stripped by `client_hello::parse`,
            // but we re-lowercase here defensively in case a future
            // caller hands us a raw host. This stays cheap: the
            // typical hostname is ≤ 64 chars.
            let host_lc = host.to_ascii_lowercase();
            if let Some(&rule_id) = self.exact.get(&host_lc) {
                return SniMatch::Hit {
                    rule_id,
                    kind: SniMatchKind::Exact,
                };
            }
            // Phase 4 (T053) walks `self.wildcards`. We populate it
            // here in Phase 3 already; the walk is added in T053 so
            // the unit test `exact_beats_fallback` (T034) passes
            // without depending on US2 work.
            for (suffix, rule_id) in &self.wildcards {
                if wildcard_matches(&host_lc, suffix) {
                    return SniMatch::Hit {
                        rule_id: *rule_id,
                        kind: SniMatchKind::Wildcard,
                    };
                }
            }
        }
        if let Some(rule_id) = self.fallback {
            return SniMatch::Hit {
                rule_id,
                kind: SniMatchKind::Fallback,
            };
        }
        SniMatch::Miss
    }
}

/// Single-label wildcard match (data-model.md §2.2).
///
/// `*.example.com` matches `foo.example.com` but not `example.com`
/// (no left label) and not `a.b.example.com` (extra label).
///
/// `host_lc` is assumed lowercased; `suffix` is the part after `*.`
/// (e.g. `"example.com"`).
fn wildcard_matches(host_lc: &str, suffix: &str) -> bool {
    let needle = format!(".{suffix}");
    let Some(prefix) = host_lc.strip_suffix(&needle) else {
        return false;
    };
    if prefix.is_empty() {
        // host == ".example.com" — bogus, but stripping leaves "".
        return false;
    }
    // Single-label remainder check: prefix must contain no dots.
    !prefix.contains('.')
}

#[cfg(test)]
#[allow(clippy::match_wildcard_for_single_variants)] // tests pattern-match on `SniMatch::Hit { .. } | other`
mod tests {
    use super::*;

    fn rid(n: u64) -> RuleId {
        RuleId(n)
    }

    #[test]
    fn exact_beats_fallback() {
        // T034: exact match priority over the fallback slot.
        let table =
            SniRoutingTable::from_members(&[(Some("api.example.com"), rid(1)), (None, rid(2))])
                .expect("build");
        assert_eq!(
            table.lookup(Some("api.example.com")),
            SniMatch::Hit {
                rule_id: rid(1),
                kind: SniMatchKind::Exact,
            }
        );
    }

    #[test]
    fn unmatched_falls_back() {
        let table =
            SniRoutingTable::from_members(&[(Some("api.example.com"), rid(1)), (None, rid(2))])
                .expect("build");
        assert_eq!(
            table.lookup(Some("admin.example.com")),
            SniMatch::Hit {
                rule_id: rid(2),
                kind: SniMatchKind::Fallback,
            }
        );
    }

    #[test]
    fn no_sni_lands_on_fallback() {
        let table =
            SniRoutingTable::from_members(&[(Some("api.example.com"), rid(1)), (None, rid(2))])
                .expect("build");
        assert_eq!(
            table.lookup(None),
            SniMatch::Hit {
                rule_id: rid(2),
                kind: SniMatchKind::Fallback,
            }
        );
    }

    #[test]
    fn no_sni_no_fallback_misses() {
        let table =
            SniRoutingTable::from_members(&[(Some("api.example.com"), rid(1))]).expect("build");
        assert_eq!(table.lookup(None), SniMatch::Miss);
    }

    #[test]
    fn duplicate_fallback_panics_or_errors() {
        // T057: in-memory backstop for INV-1 (server's overlap check
        // is the authoritative gate, but the table refuses to silently
        // drop a fallback if a developer footgun bypasses it).
        let err = SniRoutingTable::from_members(&[(None, rid(1)), (None, rid(2))])
            .expect_err("two fallbacks must error");
        assert_eq!(err, BuildError::DuplicateFallback);
    }

    #[test]
    fn duplicate_exact_errors() {
        let err = SniRoutingTable::from_members(&[
            (Some("api.example.com"), rid(1)),
            (Some("API.example.com"), rid(2)),
        ])
        .expect_err("duplicate exact must error");
        match err {
            BuildError::DuplicateExact(h) => assert_eq!(h, "api.example.com"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn wildcard_single_label_only() {
        // T047: *.example.com matches foo.example.com; rejects
        // example.com (no left label) and a.b.example.com (extra label).
        let table =
            SniRoutingTable::from_members(&[(Some("*.example.com"), rid(7))]).expect("build");
        assert_eq!(
            table.lookup(Some("foo.example.com")),
            SniMatch::Hit {
                rule_id: rid(7),
                kind: SniMatchKind::Wildcard,
            }
        );
        assert_eq!(table.lookup(Some("example.com")), SniMatch::Miss);
        assert_eq!(table.lookup(Some("a.b.example.com")), SniMatch::Miss);
    }

    #[test]
    fn longest_wildcard_wins() {
        // T048: more specific suffix beats less specific.
        let table = SniRoutingTable::from_members(&[
            (Some("*.example.com"), rid(1)),
            (Some("*.team.example.com"), rid(2)),
        ])
        .expect("build");
        assert_eq!(
            table.lookup(Some("x.team.example.com")),
            SniMatch::Hit {
                rule_id: rid(2),
                kind: SniMatchKind::Wildcard,
            }
        );
    }

    #[test]
    fn exact_beats_wildcard() {
        // T049: exact match wins over a covering wildcard.
        let table = SniRoutingTable::from_members(&[
            (Some("api.example.com"), rid(1)),
            (Some("*.example.com"), rid(2)),
        ])
        .expect("build");
        assert_eq!(
            table.lookup(Some("api.example.com")),
            SniMatch::Hit {
                rule_id: rid(1),
                kind: SniMatchKind::Exact,
            }
        );
        assert_eq!(
            table.lookup(Some("other.example.com")),
            SniMatch::Hit {
                rule_id: rid(2),
                kind: SniMatchKind::Wildcard,
            }
        );
    }

    #[test]
    fn fallback_only_on_miss() {
        // T056: fallback fires only when both exact and wildcard miss.
        let table = SniRoutingTable::from_members(&[
            (Some("api.example.com"), rid(1)),
            (Some("*.example.com"), rid(2)),
            (None, rid(99)),
        ])
        .expect("build");
        // Exact wins.
        match table.lookup(Some("api.example.com")) {
            SniMatch::Hit { rule_id, .. } => assert_eq!(rule_id, rid(1)),
            other => panic!("{other:?}"),
        }
        // Wildcard wins.
        match table.lookup(Some("foo.example.com")) {
            SniMatch::Hit { rule_id, .. } => assert_eq!(rule_id, rid(2)),
            other => panic!("{other:?}"),
        }
        // Both miss → fallback.
        match table.lookup(Some("totally.unrelated.host")) {
            SniMatch::Hit { rule_id, kind } => {
                assert_eq!(rule_id, rid(99));
                assert_eq!(kind, SniMatchKind::Fallback);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lookup_lowercases_host() {
        let table =
            SniRoutingTable::from_members(&[(Some("api.example.com"), rid(1))]).expect("build");
        assert_eq!(
            table.lookup(Some("API.EXAMPLE.COM")),
            SniMatch::Hit {
                rule_id: rid(1),
                kind: SniMatchKind::Exact,
            }
        );
    }
}
