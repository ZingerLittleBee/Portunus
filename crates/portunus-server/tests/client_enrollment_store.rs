//! Contract tests for short-lived client enrollment codes.

use std::sync::Arc;

use chrono::{Duration, Utc};
use portunus_auth::{AuthError, AuthFailureReason, Authenticator};
use portunus_core::ClientName;
use portunus_server::store::Store;
use portunus_server::store::enrollment_store::{
    ClientEnrollmentStore, CreateEnrollment, CreateEnrollmentError, EnrollmentTarget,
    RedeemEnrollmentError,
};
use portunus_server::store::token_store::SqliteTokenStore;
use tempfile::tempdir;

fn fresh() -> (
    tempfile::TempDir,
    Arc<Store>,
    ClientEnrollmentStore,
    SqliteTokenStore,
) {
    let dir = tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    (
        dir,
        Arc::clone(&store),
        ClientEnrollmentStore::new(Arc::clone(&store)),
        SqliteTokenStore::new(store),
    )
}

#[test]
fn created_code_redeems_once_and_issues_client_token() {
    let (_dir, _store, enrollments, tokens) = fresh();
    let now = Utc::now();
    let created = enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: ClientName::new("edge-01").unwrap(),
            target: EnrollmentTarget::New {
                client_address: Some("edge.example.com".into()),
            },
            expires_at: now + Duration::minutes(10),
            now,
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect("create enrollment");

    let issued = enrollments
        .redeem(&created.code, now + Duration::seconds(1), || {
            panic!("resolver must not run for persisted-endpoint rows")
        })
        .expect("redeem enrollment");

    assert_eq!(issued.client_name.as_str(), "edge-01");
    assert_eq!(
        tokens
            .verify(&issued.token)
            .expect("issued token verifies")
            .client_name
            .as_str(),
        "edge-01"
    );
    assert!(matches!(
        enrollments
            .redeem(&created.code, now + Duration::seconds(2), || panic!(
                "resolver must not run for persisted-endpoint rows"
            ))
            .unwrap_err(),
        RedeemEnrollmentError::AlreadyUsed
    ));
}

#[test]
fn expired_code_does_not_issue_client_token() {
    let (_dir, _store, enrollments, tokens) = fresh();
    let now = Utc::now();
    let created = enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: ClientName::new("edge-01").unwrap(),
            target: EnrollmentTarget::New {
                client_address: None,
            },
            expires_at: now + Duration::seconds(1),
            now,
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect("create enrollment");

    let err = enrollments
        .redeem(&created.code, now + Duration::seconds(2), || {
            panic!("resolver must not run for persisted-endpoint rows")
        })
        .unwrap_err();

    assert!(matches!(err, RedeemEnrollmentError::Expired));
    assert!(tokens.list().expect("list tokens").is_empty());
}

#[test]
fn newer_code_for_same_client_invalidates_older_pending_code() {
    let (_dir, _store, enrollments, tokens) = fresh();
    let now = Utc::now();
    let name = ClientName::new("edge-01").unwrap();
    let older = enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: name.clone(),
            target: EnrollmentTarget::New {
                client_address: None,
            },
            expires_at: now + Duration::minutes(5),
            now,
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect("create older enrollment");
    let newer = enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: name,
            target: EnrollmentTarget::New {
                client_address: None,
            },
            expires_at: now + Duration::minutes(5),
            now: now + Duration::seconds(1),
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect("create newer enrollment");

    assert!(matches!(
        enrollments.redeem(&older.code, now + Duration::seconds(2), || panic!(
            "resolver must not run for persisted-endpoint rows"
        )),
        Err(RedeemEnrollmentError::AlreadyUsed)
    ));

    let issued = enrollments
        .redeem(&newer.code, now + Duration::seconds(2), || {
            panic!("resolver must not run for persisted-endpoint rows")
        })
        .expect("newer code redeems");
    assert_eq!(
        tokens
            .verify(&issued.token)
            .expect("issued token verifies")
            .client_name
            .as_str(),
        "edge-01"
    );
}

#[test]
fn existing_client_code_redeems_by_rotating_token_in_place() {
    let (_dir, _store, enrollments, tokens) = fresh();
    let name = ClientName::new("edge-01").unwrap();
    let old_token = tokens
        .issue_with_address(name.clone(), Some("edge.example.com"))
        .expect("seed client");
    let now = Utc::now();
    let created = enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: name.clone(),
            target: EnrollmentTarget::Existing,
            expires_at: now + Duration::minutes(5),
            now,
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect("create enrollment");
    assert_eq!(created.client_address.as_deref(), Some("edge.example.com"));

    let issued = enrollments
        .redeem(&created.code, now, || {
            panic!("resolver must not run for persisted-endpoint rows")
        })
        .expect("redeem");

    assert_eq!(issued.client_name, name);
    assert!(issued.rotated_existing);
    assert_eq!(tokens.list().expect("list").len(), 1);
    assert!(matches!(
        tokens
            .verify(&old_token)
            .expect_err("old token must stop working"),
        AuthError::Failed(AuthFailureReason::NotFound)
    ));
    assert_eq!(
        tokens
            .verify(&issued.token)
            .expect("new token works")
            .client_name,
        name
    );
}

#[test]
fn new_client_enrollment_rejects_existing_client_inside_store_transaction() {
    let (_dir, _store, enrollments, tokens) = fresh();
    let name = ClientName::new("edge-01").unwrap();
    tokens.issue(name.clone()).expect("seed client");
    let now = Utc::now();

    let err = enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: name.clone(),
            target: EnrollmentTarget::New {
                client_address: None,
            },
            expires_at: now + Duration::minutes(5),
            now,
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect_err("new enrollment must reject existing client");

    assert!(matches!(
        err,
        CreateEnrollmentError::ClientAlreadyExists(existing) if existing == name
    ));
}

#[test]
fn existing_client_enrollment_requires_existing_client_inside_store_transaction() {
    let (_dir, _store, enrollments, _tokens) = fresh();
    let name = ClientName::new("edge-01").unwrap();
    let now = Utc::now();

    let err = enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: name.clone(),
            target: EnrollmentTarget::Existing,
            expires_at: now + Duration::minutes(5),
            now,
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect_err("re-enrollment must require existing client");

    assert!(matches!(
        err,
        CreateEnrollmentError::ClientNotFound(missing) if missing == name
    ));
}

#[test]
fn creating_enrollment_prunes_old_consumed_and_expired_rows() {
    let (_dir, store, enrollments, _tokens) = fresh();
    let now = Utc::now();
    let stale = now - Duration::days(2);
    let fresh_expired = now - Duration::minutes(1);

    store
        .with_write_tx(|tx| {
            tx.execute(
                "INSERT INTO client_enrollments \
                 (client_name, client_address, code_hash, issued_at, expires_at, consumed_at) \
                 VALUES \
                 ('stale-consumed', NULL, 'old-consumed', ?1, ?1, ?1), \
                 ('stale-expired', NULL, 'old-expired', ?1, ?1, NULL), \
                 ('fresh-expired', NULL, 'fresh-expired', ?2, ?2, NULL)",
                rusqlite::params![stale.to_rfc3339(), fresh_expired.to_rfc3339()],
            )
            .map_err(portunus_server::store::map_rusqlite)?;
            Ok(())
        })
        .expect("seed stale rows");

    enrollments
        .create(CreateEnrollment {
            client_id: None,
            client_name: ClientName::new("edge-01").unwrap(),
            target: EnrollmentTarget::New {
                client_address: None,
            },
            expires_at: now + Duration::minutes(5),
            now,
            advertised_endpoint: "public.example:7443".to_string(),
        })
        .expect("create enrollment");

    let rows = store
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM client_enrollments", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(portunus_server::store::map_rusqlite)
        })
        .expect("count rows");
    assert_eq!(rows, 2);
}
