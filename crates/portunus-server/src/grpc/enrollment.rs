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
        let issued = self
            .enrollments
            .redeem(&code, Utc::now())
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
        let server_endpoint = if let Some(ep) = issued.advertised_endpoint.clone() {
            ep
        } else {
            // Legacy pre-V010 row: resolve once, fail closed.
            let override_value = self
                .state
                .settings
                .get_advertised_endpoint()
                .map_err(|e| Status::internal(format!("settings: {e}")))?;
            crate::advertised::resolve_advertised_endpoint(
                &crate::advertised::resolve::ResolveInputs {
                    override_value,
                    seed: self.state.advertised_seed.clone(),
                    req_host: None,
                    control_port: self.state.control_port,
                    san: &self.state.cert_san,
                },
            )
            .map_err(|e| {
                warn!(event = "client.enrollment_failed", error = %e);
                Status::failed_precondition(e.http_code())
            })?
            .endpoint
        };
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
