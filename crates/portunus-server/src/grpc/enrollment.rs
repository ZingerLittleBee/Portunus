//! Unauthenticated, one-shot client enrollment RPC.

use std::sync::Arc;

use chrono::Utc;
use portunus_proto::v1::{
    CredentialBundle as WireCredentialBundle, EnrollClientRequest,
    client_enrollment_server::ClientEnrollment,
};
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use crate::state::AppState;
use crate::store::enrollment_store::{ClientEnrollmentStore, RedeemEnrollmentError};

pub struct ClientEnrollmentService {
    state: Arc<AppState>,
    enrollments: ClientEnrollmentStore,
}

impl ClientEnrollmentService {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        let enrollments = ClientEnrollmentStore::new(Arc::clone(&state.store));
        Self { state, enrollments }
    }
}

#[tonic::async_trait]
impl ClientEnrollment for ClientEnrollmentService {
    async fn enroll(
        &self,
        request: Request<EnrollClientRequest>,
    ) -> Result<Response<WireCredentialBundle>, Status> {
        let code = request.into_inner().code;
        let state = &self.state;
        // Read the operator override BEFORE entering redeem()'s write
        // transaction. resolve_legacy() runs inside that tx (which holds the
        // only pooled connection on 1-vCPU hosts); doing the settings DB read
        // here — sequentially, before the tx — avoids a nested pool checkout
        // self-deadlock. The resolver itself is pure (no DB), so it stays in
        // the closure. Persisted-endpoint rows never invoke the closure, so a
        // settings-read failure is only surfaced for legacy NULL rows
        // (unchanged behavior).
        let pre_override = state.settings.get_advertised_endpoint();
        let resolve_legacy = move || -> Result<String, RedeemEnrollmentError> {
            let override_value = pre_override.map_err(|e| {
                warn!(event = "client.enrollment_failed", error = %e);
                RedeemEnrollmentError::Store(e)
            })?;
            crate::advertised::resolve_advertised_endpoint(
                &crate::advertised::resolve::ResolveInputs {
                    override_value,
                    seed: state.advertised_seed.clone(),
                    req_host: None,
                    control_port: state.control_port,
                    san: &state.cert_san,
                },
            )
            .map(|r| r.endpoint)
            .map_err(|e| {
                warn!(event = "client.enrollment_failed", error = %e);
                RedeemEnrollmentError::AdvertisedEndpoint(e.http_code().to_string())
            })
        };
        let issued = self
            .enrollments
            .redeem(&code, Utc::now(), resolve_legacy)
            .map_err(map_redeem_error)?;
        info!(
            event = "client.enrollment_redeemed",
            client_name = %issued.client_name,
            rotated_existing = issued.rotated_existing,
        );
        if issued.rotated_existing {
            let disconnected = self.state.clients.disconnect(&issued.client_name).await;
            info!(
                event = "client.enrollment_rotated",
                client_name = %issued.client_name,
                disconnected,
            );
        }
        // `advertised_endpoint` is always `Some` after a successful
        // redeem: persisted rows carry their value; legacy NULL rows
        // were resolved inside the redeem transaction above.
        let server_endpoint = issued.advertised_endpoint.clone().unwrap_or_default();
        Ok(Response::new(WireCredentialBundle {
            version: 1,
            client_name: issued.client_name.to_string(),
            server_endpoint,
            server_cert_sha256: self.state.server_cert_sha256.clone(),
            server_cert_pem: self.state.server_cert_pem.clone(),
            token: issued.token,
        }))
    }
}

fn map_redeem_error(err: RedeemEnrollmentError) -> Status {
    match err {
        RedeemEnrollmentError::InvalidCode => Status::unauthenticated("enrollment_invalid"),
        RedeemEnrollmentError::Expired => Status::unauthenticated("enrollment_expired"),
        RedeemEnrollmentError::AlreadyUsed => Status::unauthenticated("enrollment_used"),
        RedeemEnrollmentError::AdvertisedEndpoint(code) => Status::failed_precondition(code),
        RedeemEnrollmentError::Store(e) => {
            warn!(event = "client.enrollment_failed", error = %e);
            Status::internal("enrollment_store_error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::ConnectedClients;
    use crate::store::Store;
    use crate::store::operator_store::SqliteOperatorStore;
    use crate::store::token_store::SqliteTokenStore;
    use tempfile::tempdir;

    fn test_state_with_fixture_cert() -> Arc<AppState> {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
        operator_store
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        Arc::new(
            AppState::new(
                tokens,
                operator_store,
                ConnectedClients::default(),
                None,
                7443,
                "deadbeef",
                include_str!("../advertised/testdata/san_fixture.pem"),
                16,
                store,
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn redeemed_bundle_endpoint_matches_creation_resolution() {
        let state = test_state_with_fixture_cert();
        let cmd = crate::operator::cli::enroll_client(
            &state,
            "edge-x",
            None,
            300,
            Some("public.example:443"),
        )
        .expect("create enrollment");
        // uri: portunus://public.example:7443/enroll?...&code=CODE&cert=...
        let code = cmd
            .uri
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string();

        let svc = ClientEnrollmentService::new(Arc::clone(&state));
        let resp = svc
            .enroll(tonic::Request::new(EnrollClientRequest { code }))
            .await
            .expect("enroll ok");
        assert_eq!(resp.into_inner().server_endpoint, "public.example:7443");
    }
}
