//! Bearer-token interceptor.
//!
//! Reads the `authorization` metadata, strips the `Bearer ` prefix, calls
//! [`forward_auth::Authenticator::verify`], and injects the resulting
//! [`ClientIdentity`] into request extensions. On failure returns
//! `Status::unauthenticated(<reason>)` with one of the stable reason
//! strings from `AuthFailureReason`.

use std::sync::Arc;

use forward_auth::{AuthError, AuthFailureReason, Authenticator, ClientIdentity};
use tonic::{Request, Status};
use tracing::warn;

#[derive(Clone)]
pub struct AuthInterceptor {
    pub auth: Arc<dyn Authenticator>,
}

impl AuthInterceptor {
    #[must_use]
    pub fn new(auth: Arc<dyn Authenticator>) -> Self {
        Self { auth }
    }

    pub fn intercept<T>(&self, mut req: Request<T>) -> Result<Request<T>, Status> {
        let token = match req.metadata().get("authorization") {
            None => {
                warn!(event = "auth.failure", reason = %AuthFailureReason::Missing);
                return Err(Status::unauthenticated(
                    AuthFailureReason::Missing.to_string(),
                ));
            }
            Some(v) => {
                if let Ok(s) = v.to_str() {
                    s.to_owned()
                } else {
                    warn!(event = "auth.failure", reason = %AuthFailureReason::Malformed);
                    return Err(Status::unauthenticated(
                        AuthFailureReason::Malformed.to_string(),
                    ));
                }
            }
        };
        let token = if let Some(rest) = token.strip_prefix("Bearer ") {
            rest.trim()
        } else {
            warn!(event = "auth.failure", reason = %AuthFailureReason::Malformed);
            return Err(Status::unauthenticated(
                AuthFailureReason::Malformed.to_string(),
            ));
        };
        match self.auth.verify(token) {
            Ok(identity) => {
                inject_identity(&mut req, identity);
                Ok(req)
            }
            Err(AuthError::Failed(reason)) => {
                warn!(event = "auth.failure", reason = %reason);
                Err(Status::unauthenticated(reason.to_string()))
            }
            Err(other) => {
                warn!(event = "auth.failure", reason = %other);
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
