//! T030 (005-multi-user-rbac, US2) — `identity.json` round-trip across
//! a `FileOperatorStore` drop + reopen. Validates that every entity
//! persists, that revoked credentials remain on disk for audit, and
//! that the reloaded store rejects revoked tokens with `credential_invalid`.

use chrono::Utc;
use forward_auth::{
    ClientScope, Grant, GrantId, OperatorAuthenticator, OperatorRole, ProtocolSet, RbacError, User,
    UserId,
};
use forward_core::ClientName;
use std::str::FromStr;
use tempfile::TempDir;

#[test]
fn full_round_trip_users_credentials_grants() {
    let dir = TempDir::new().expect("tempdir");

    let bootstrap_token = "T030-bootstrap-token";
    let alice_label = "laptop";

    let alice = UserId::from_str("alice").unwrap();
    let alice_grant_id;
    let alice_token;

    // ---- Phase 1: build state, drop the store ----
    {
        let sqlite_store =
            std::sync::Arc::new(forward_server::store::Store::open(dir.path()).unwrap());
        let store = forward_server::store::operator_store::SqliteOperatorStore::new(
            std::sync::Arc::clone(&sqlite_store),
        );
        store
            .bootstrap_legacy_superadmin(bootstrap_token)
            .expect("bootstrap");
        store
            .add_user(User {
                id: alice.clone(),
                display_name: "Alice".to_string(),
                role: OperatorRole::User,
                created_at: Utc::now(),
                disabled: false,
            })
            .expect("add alice");
        let (_cred, raw) = store
            .issue_credential(&alice, Some(alice_label.to_string()))
            .expect("issue cred");
        alice_token = raw;
        let grant = Grant {
            id: GrantId::new(),
            user_id: alice.clone(),
            client: ClientScope::Named(ClientName::new("client-a".to_string()).unwrap()),
            listen_port_start: 30000,
            listen_port_end: 30005,
            protocols: ProtocolSet::non_empty(ProtocolSet::TCP).unwrap(),
            note: None,
            created_at: Utc::now(),
        };
        alice_grant_id = grant.id;
        store.add_grant(grant).expect("add grant");
        // store dropped here at end of scope.
    }

    // ---- Phase 2: re-open, assert state survived ----
    {
        let sqlite_store =
            std::sync::Arc::new(forward_server::store::Store::open(dir.path()).unwrap());
        let store = forward_server::store::operator_store::SqliteOperatorStore::new(
            std::sync::Arc::clone(&sqlite_store),
        );
        // Bootstrap superadmin + alice.
        let users = store.list_users();
        assert_eq!(users.len(), 2);

        // Both tokens still verifiable.
        let id = store
            .verify(bootstrap_token)
            .expect("bootstrap token verify");
        assert_eq!(id.role, OperatorRole::Superadmin);
        let id = store.verify(&alice_token).expect("alice token verify");
        assert_eq!(id.user_id, alice);
        assert_eq!(id.role, OperatorRole::User);

        // Alice's grant is intact.
        let grants = store.list_grants(Some(&alice));
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].id, alice_grant_id);
        assert_eq!(grants[0].listen_port_start, 30000);

        // Now revoke alice's credential, drop, reload, assert it's still on
        // disk but verify yields credential_invalid.
        let creds = store.list_credentials(&alice);
        assert_eq!(creds.len(), 1);
        store
            .revoke_credential(&alice, &creds[0].id)
            .expect("revoke");
        // store dropped.
    }

    // ---- Phase 3: revoked credential still on disk, fails verify ----
    {
        let sqlite_store =
            std::sync::Arc::new(forward_server::store::Store::open(dir.path()).unwrap());
        let store = forward_server::store::operator_store::SqliteOperatorStore::new(
            std::sync::Arc::clone(&sqlite_store),
        );
        let creds = store.list_credentials(&alice);
        assert_eq!(creds.len(), 1, "revoked credential must persist for audit");
        match store.verify(&alice_token) {
            Err(RbacError::CredentialInvalid) => {}
            other => panic!("expected CredentialInvalid; got {other:?}"),
        }
    }
}
