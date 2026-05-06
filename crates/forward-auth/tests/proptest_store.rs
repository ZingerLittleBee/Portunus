//! Property: any sequence of `issue` and `revoke` operations against a
//! `FileTokenStore` must be observable as the same in-memory state after
//! reopening the store from disk.

use std::collections::{HashMap, HashSet};

use forward_auth::file_store::FileTokenStore;
use forward_auth::{AuthError, AuthFailureReason, Authenticator};
use forward_core::ClientName;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Issue(ClientName),
    Revoke(ClientName),
}

fn name_strategy() -> impl Strategy<Value = ClientName> {
    // Small pool so that Issue/Revoke conflicts are likely.
    prop::sample::select(vec![
        ClientName::new("edge-01").unwrap(),
        ClientName::new("edge-02").unwrap(),
        ClientName::new("edge-03").unwrap(),
        ClientName::new("a").unwrap(),
        ClientName::new("z9").unwrap(),
    ])
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        name_strategy().prop_map(Op::Issue),
        name_strategy().prop_map(Op::Revoke),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    fn store_round_trip(ops in prop::collection::vec(op_strategy(), 1..30)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        // Track the model: name -> (live_token_or_none, revoked)
        let mut live: HashMap<ClientName, String> = HashMap::new();
        let mut revoked: HashSet<ClientName> = HashSet::new();
        let mut existing: HashSet<ClientName> = HashSet::new();

        let store = FileTokenStore::open(&path).unwrap();
        for op in ops {
            match op {
                Op::Issue(name) => {
                    let result = store.issue(name.clone());
                    if existing.contains(&name) {
                        prop_assert!(matches!(result, Err(AuthError::ClientAlreadyExists(_))));
                    } else {
                        let token = result.unwrap();
                        existing.insert(name.clone());
                        live.insert(name, token);
                    }
                }
                Op::Revoke(name) => {
                    store.revoke(&name).unwrap();
                    if existing.contains(&name) {
                        revoked.insert(name);
                    }
                }
            }
        }

        // Reopen from disk and assert observable state matches the model.
        drop(store);
        let reopened = FileTokenStore::open(&path).unwrap();
        for (name, token) in &live {
            let got = reopened.verify(token);
            if revoked.contains(name) {
                prop_assert!(
                    matches!(got, Err(AuthError::Failed(AuthFailureReason::Revoked))),
                    "expected Revoked for {name}, got {got:?}"
                );
            } else {
                let id = got.unwrap();
                prop_assert_eq!(id.client_name.as_str(), name.as_str());
            }
        }
    }
}
