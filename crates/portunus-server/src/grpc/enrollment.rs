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
        Ok(Response::new(WireCredentialBundle {
            version: 1,
            client_name: issued.client_name.to_string(),
            server_endpoint: self.state.server_endpoint.clone(),
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
