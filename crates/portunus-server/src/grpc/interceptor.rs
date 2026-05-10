//! Bearer-token interceptor.
//!
//! Reads the `authorization` metadata, strips the `Bearer ` prefix, calls
//! [`portunus_auth::Authenticator::verify`], and injects the resulting
//! [`ClientIdentity`] into request extensions. On failure returns
//! `Status::unauthenticated(<reason>)` with one of the stable reason
//! strings from `AuthFailureReason`.

use std::sync::Arc;

use portunus_auth::{AuthError, AuthFailureReason, Authenticator, ClientIdentity};
use tonic::{Request, Status};
use tracing::warn;

use crate::metrics::Metrics;

#[derive(Clone)]
pub struct AuthInterceptor {
    pub auth: Arc<dyn Authenticator>,
    pub metrics: Arc<Metrics>,
}

impl AuthInterceptor {
    #[must_use]
    pub fn new(auth: Arc<dyn Authenticator>, metrics: Arc<Metrics>) -> Self {
        Self { auth, metrics }
    }

    fn record_failure(&self, reason: AuthFailureReason) {
        self.metrics
            .auth_failures_total
            .with_label_values(&[&reason.to_string()])
            .inc();
    }

    pub fn intercept<T>(&self, mut req: Request<T>) -> Result<Request<T>, Status> {
        let token = match req.metadata().get("authorization") {
            None => {
                let reason = AuthFailureReason::Missing;
                warn!(event = "auth.failure", reason = %reason);
                self.record_failure(reason);
                return Err(Status::unauthenticated(reason.to_string()));
            }
            Some(v) => {
                if let Ok(s) = v.to_str() {
                    s.to_owned()
                } else {
                    let reason = AuthFailureReason::Malformed;
                    warn!(event = "auth.failure", reason = %reason);
                    self.record_failure(reason);
                    return Err(Status::unauthenticated(reason.to_string()));
                }
            }
        };
        let token = if let Some(rest) = token.strip_prefix("Bearer ") {
            rest.trim()
        } else {
            let reason = AuthFailureReason::Malformed;
            warn!(event = "auth.failure", reason = %reason);
            self.record_failure(reason);
            return Err(Status::unauthenticated(reason.to_string()));
        };
        match self.auth.verify(token) {
            Ok(identity) => {
                inject_identity(&mut req, identity);
                Ok(req)
            }
            Err(AuthError::Failed(reason)) => {
                warn!(event = "auth.failure", reason = %reason);
                self.record_failure(reason);
                Err(Status::unauthenticated(reason.to_string()))
            }
            Err(other) => {
                warn!(event = "auth.failure", reason = %other);
                // Emit under a stable bucket so alerting groups consistently.
                self.metrics
                    .auth_failures_total
                    .with_label_values(&["internal_error"])
                    .inc();
                Err(Status::unauthenticated("token_verify_error"))
            }
        }
    }
}

fn inject_identity<T>(req: &mut Request<T>, identity: ClientIdentity) {
    req.extensions_mut().insert(identity);
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, req: Request<()>) -> Result<Request<()>, Status> {
        self.intercept(req)
    }
}
