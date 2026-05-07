//! RBAC enforcement (005-multi-user-rbac, US1).
//!
//! Pure functions over `&OperatorIdentity` + `&[Grant]` → `Result<(), RbacError>`.
//! No I/O, no async, no logging — those live one layer up in the HTTP
//! handlers and the auth middleware. Keeping this module pure makes
//! it the natural place for the unit-test surface.
//!
//! Authorization predicate (closed-set, R-004 in
//! `specs/005-multi-user-rbac/research.md`): a user is authorised to
//! push a rule iff at least one single grant covers ALL dimensions
//! (client, full listen-port range, protocol). Range rules whose
//! listen range straddles two grants are REJECTED, even if the
//! union of grants would cover them.

use forward_auth::{
    ClientScope, Grant, OperatorIdentity, OperatorRole, ProtocolSet, RbacError, UserId,
};
use forward_core::ClientName;

/// What a push-rule request asks to do, distilled from the validated
/// HTTP body. Lives here (not in `rules.rs`) so the rbac layer doesn't
/// reach into the rule-store internals.
#[derive(Debug, Clone)]
pub struct PushRequest<'a> {
    pub client: &'a ClientName,
    pub listen_port_start: u16,
    pub listen_port_end: u16,
    pub protocol: PushProtocol,
}

/// Mirror of `forward_server::rules::Protocol` projected onto the
/// rbac layer's bitflags. We translate at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushProtocol {
    Tcp,
    Udp,
}

impl PushProtocol {
    #[must_use]
    fn as_set(self) -> ProtocolSet {
        match self {
            Self::Tcp => ProtocolSet::TCP,
            Self::Udp => ProtocolSet::UDP,
        }
    }
}

/// Authorize a push-rule request. Returns `Ok(())` if allowed,
/// otherwise the most specific applicable rejection reason.
///
/// Reason priority on a non-superadmin caller with no covering grant:
///   1. `client_not_granted` — caller has zero grants whose `client`
///      matches the request's client.
///   2. `protocol_not_granted` — caller has client-matching grants
///      but none whose `protocols` include the requested protocol.
///   3. `port_outside_grant` — caller has client+protocol-matching
///      grants but none whose port range fully covers the listen
///      range (the closed-set semantic from R-004).
pub fn enforce_push(
    identity: &OperatorIdentity,
    push: &PushRequest<'_>,
    grants: &[Grant],
) -> Result<(), RbacError> {
    if identity.role == OperatorRole::Superadmin {
        return Ok(());
    }

    let proto_set = push.protocol.as_set();

    let client_match: Vec<&Grant> = grants
        .iter()
        .filter(|g| matches_client(&g.client, push.client))
        .collect();
    if client_match.is_empty() {
        return Err(RbacError::ClientNotGranted);
    }

    let proto_match: Vec<&Grant> = client_match
        .iter()
        .copied()
        .filter(|g| g.protocols.contains(proto_set))
        .collect();
    if proto_match.is_empty() {
        return Err(RbacError::ProtocolNotGranted);
    }

    let covers_range = proto_match.iter().any(|g| {
        g.listen_port_start <= push.listen_port_start && push.listen_port_end <= g.listen_port_end
    });
    if !covers_range {
        return Err(RbacError::PortOutsideGrant);
    }

    Ok(())
}

/// Authorize a read of a single rule (rule-stats, delete-rule).
/// Superadmin always allowed; everyone else only their own.
pub fn enforce_read(identity: &OperatorIdentity, rule_owner: &UserId) -> Result<(), RbacError> {
    if identity.role == OperatorRole::Superadmin {
        return Ok(());
    }
    if &identity.user_id == rule_owner {
        Ok(())
    } else {
        Err(RbacError::NotOwner)
    }
}

/// Filter a rule list to those visible to the caller. Superadmin sees
/// everything; everyone else only their own.
pub fn filter_visible<R, I>(identity: &OperatorIdentity, rules: I) -> Vec<R>
where
    I: IntoIterator<Item = R>,
    R: HasOwner,
{
    if identity.role == OperatorRole::Superadmin {
        return rules.into_iter().collect();
    }
    rules
        .into_iter()
        .filter(|r| r.owner() == &identity.user_id)
        .collect()
}

/// Trait so `filter_visible` works on both `Rule` and references to it
/// without forcing a particular ownership model on the caller.
pub trait HasOwner {
    fn owner(&self) -> &UserId;
}

impl<T: HasOwner> HasOwner for &T {
    fn owner(&self) -> &UserId {
        (*self).owner()
    }
}

/// Require that the caller has the named role (or higher). Used by
/// superadmin-only endpoints (user-add, grant-add, etc.).
pub fn require_role(identity: &OperatorIdentity, required: OperatorRole) -> Result<(), RbacError> {
    if matches!(identity.role, OperatorRole::Superadmin) {
        // superadmin satisfies any required role
        return Ok(());
    }
    if identity.role == required {
        Ok(())
    } else {
        Err(RbacError::RoleRequired)
    }
}

fn matches_client(scope: &ClientScope, want: &ClientName) -> bool {
    match scope {
        ClientScope::Any => true,
        ClientScope::Named(n) => n == want,
    }
}

// ============================================================================
// Tests (T017)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use forward_auth::{Grant, GrantId, OperatorRole, ProtocolSet, UserId};
    use std::str::FromStr;

    fn alice() -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::from_str("alice").unwrap(),
            role: OperatorRole::User,
        }
    }

    fn superadmin() -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::superadmin(),
            role: OperatorRole::Superadmin,
        }
    }

    fn client_a() -> ClientName {
        ClientName::new("client-a").unwrap()
    }

    fn client_b() -> ClientName {
        ClientName::new("client-b").unwrap()
    }

    fn grant(scope: ClientScope, lo: u16, hi: u16, protos: ProtocolSet) -> Grant {
        Grant {
            id: GrantId::new(),
            user_id: UserId::from_str("alice").unwrap(),
            client: scope,
            listen_port_start: lo,
            listen_port_end: hi,
            protocols: protos,
            note: None,
            created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 7, 10, 0, 0).unwrap(),
        }
    }

    fn push(client: &ClientName, lo: u16, hi: u16, p: PushProtocol) -> PushRequest<'_> {
        PushRequest {
            client,
            listen_port_start: lo,
            listen_port_end: hi,
            protocol: p,
        }
    }

    #[test]
    fn superadmin_always_allowed_even_with_no_grants() {
        let ca = client_a();
        let req = push(&ca, 30005, 30005, PushProtocol::Tcp);
        assert!(enforce_push(&superadmin(), &req, &[]).is_ok());
    }

    #[test]
    fn single_grant_covers_single_port_rule() {
        let ca = client_a();
        let g = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::TCP,
        );
        let req = push(&ca, 30005, 30005, PushProtocol::Tcp);
        assert!(enforce_push(&alice(), &req, &[g]).is_ok());
    }

    #[test]
    fn single_grant_covers_full_range_rule() {
        let ca = client_a();
        let g = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::TCP,
        );
        let req = push(&ca, 30000, 30010, PushProtocol::Tcp);
        assert!(enforce_push(&alice(), &req, &[g]).is_ok());
    }

    #[test]
    fn range_straddling_two_grants_is_rejected_closed_set() {
        let ca = client_a();
        let g1 = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::TCP,
        );
        let g2 = grant(
            ClientScope::Named(client_a()),
            30011,
            30020,
            ProtocolSet::TCP,
        );
        let req = push(&ca, 30005, 30015, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req, &[g1, g2]),
            Err(RbacError::PortOutsideGrant)
        );
    }

    #[test]
    fn any_client_scope_matches_any_client_name() {
        let ca = client_a();
        let cb = client_b();
        let g = grant(ClientScope::Any, 30000, 30010, ProtocolSet::TCP);
        let req_a = push(&ca, 30005, 30005, PushProtocol::Tcp);
        let req_b = push(&cb, 30005, 30005, PushProtocol::Tcp);
        assert!(enforce_push(&alice(), &req_a, std::slice::from_ref(&g)).is_ok());
        assert!(enforce_push(&alice(), &req_b, &[g]).is_ok());
    }

    #[test]
    fn named_client_scope_only_matches_that_name() {
        let cb = client_b();
        let g = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::TCP,
        );
        let req_b = push(&cb, 30005, 30005, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req_b, &[g]),
            Err(RbacError::ClientNotGranted)
        );
    }

    #[test]
    fn empty_grants_rejects_everything_for_non_superadmin() {
        let ca = client_a();
        let req = push(&ca, 30005, 30005, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req, &[]),
            Err(RbacError::ClientNotGranted)
        );
    }

    #[test]
    fn protocol_mismatch_returns_protocol_not_granted() {
        let ca = client_a();
        let g = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::TCP,
        );
        let req = push(&ca, 30005, 30005, PushProtocol::Udp);
        assert_eq!(
            enforce_push(&alice(), &req, &[g]),
            Err(RbacError::ProtocolNotGranted)
        );
    }

    #[test]
    fn port_outside_returns_port_outside_grant() {
        let ca = client_a();
        let g = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::TCP,
        );
        let req = push(&ca, 30099, 30099, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req, &[g]),
            Err(RbacError::PortOutsideGrant)
        );
    }

    #[test]
    fn rejection_priority_client_then_protocol_then_port() {
        let ca = client_a();
        let req = push(&ca, 30005, 30005, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req, &[]),
            Err(RbacError::ClientNotGranted)
        );

        let g = grant(
            ClientScope::Named(client_b()),
            30000,
            30010,
            ProtocolSet::UDP,
        );
        let req = push(&ca, 30099, 30099, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req, &[g]),
            Err(RbacError::ClientNotGranted)
        );

        let g = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::UDP,
        );
        let req = push(&ca, 30099, 30099, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req, &[g]),
            Err(RbacError::ProtocolNotGranted)
        );

        let g = grant(
            ClientScope::Named(client_a()),
            30000,
            30010,
            ProtocolSet::TCP,
        );
        let req = push(&ca, 30099, 30099, PushProtocol::Tcp);
        assert_eq!(
            enforce_push(&alice(), &req, &[g]),
            Err(RbacError::PortOutsideGrant)
        );
    }

    #[test]
    fn enforce_read_superadmin_sees_all() {
        let other = UserId::from_str("bob").unwrap();
        assert!(enforce_read(&superadmin(), &other).is_ok());
    }

    #[test]
    fn enforce_read_user_sees_only_own() {
        let alice_id = UserId::from_str("alice").unwrap();
        assert!(enforce_read(&alice(), &alice_id).is_ok());
        let other = UserId::from_str("bob").unwrap();
        assert_eq!(enforce_read(&alice(), &other), Err(RbacError::NotOwner));
    }

    #[test]
    fn require_role_superadmin_satisfies_user_too() {
        assert!(require_role(&superadmin(), OperatorRole::User).is_ok());
        assert!(require_role(&superadmin(), OperatorRole::Superadmin).is_ok());
    }

    #[test]
    fn require_role_user_cannot_satisfy_superadmin() {
        assert_eq!(
            require_role(&alice(), OperatorRole::Superadmin),
            Err(RbacError::RoleRequired)
        );
    }

    struct TestRule {
        owner: UserId,
        id: u64,
    }

    impl HasOwner for TestRule {
        fn owner(&self) -> &UserId {
            &self.owner
        }
    }

    #[test]
    fn filter_visible_user_sees_only_own() {
        let rules = [
            TestRule {
                owner: UserId::from_str("alice").unwrap(),
                id: 1,
            },
            TestRule {
                owner: UserId::from_str("bob").unwrap(),
                id: 2,
            },
        ];
        let visible: Vec<&TestRule> = filter_visible(&alice(), rules.iter());
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, 1);

        let all: Vec<&TestRule> = filter_visible(&superadmin(), rules.iter());
        assert_eq!(all.len(), 2);
    }
}
