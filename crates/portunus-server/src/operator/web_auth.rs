//! Browser-oriented authentication endpoints.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::StatusCode,
};
use chrono::Utc;
use portunus_auth::{OperatorAuthenticator, OperatorRole, RbacError, User, UserId};
use serde::{Deserialize, Serialize};

use crate::operator::http::ApiError;
use crate::operator::passwords::{PasswordError, hash_password};
use crate::operator::throttle::AuthThrottleAction;
use crate::state::AppState;
use crate::store::operator_store::OnboardingError;

const ONBOARDING_THROTTLE_SUBJECT_PRESENT: &str = "_onboarding_setup_token_present";
const ONBOARDING_THROTTLE_SUBJECT_MISSING: &str = "_onboarding_setup_token_missing";

#[derive(Debug, Serialize)]
pub struct AuthStatusResponse {
    pub onboarding_required: bool,
}

#[derive(Debug, Deserialize)]
pub struct OnboardingRequest {
    pub user_id: String,
    pub display_name: String,
    pub password: String,
    pub password_confirm: String,
    #[serde(default)]
    pub setup_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OnboardingResponse {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
}

pub async fn get_auth_status(State(state): State<Arc<AppState>>) -> Json<AuthStatusResponse> {
    Json(AuthStatusResponse {
        onboarding_required: !state.operator_store.has_any_superadmin(),
    })
}

pub async fn post_auth_onboarding(
    State(state): State<Arc<AppState>>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(body): Json<OnboardingRequest>,
) -> Result<(StatusCode, Json<OnboardingResponse>), ApiError> {
    let now = Utc::now();
    if state.operator_store.has_any_superadmin() {
        return Err(already_bootstrapped());
    }
    let remote_addr = remote_addr.ip().to_string();
    let throttle_subject = onboarding_throttle_subject(body.setup_token.as_deref());
    reject_if_throttled(&state, throttle_subject, &remote_addr, now)?;

    let outcome = verify_setup_token_for_hashing(&state, body.setup_token.as_deref(), now)
        .and_then(|setup_token| {
            build_onboarding_user(&body).and_then(|(user, password_hash)| {
                state
                    .operator_store
                    .onboard_first_superadmin(user.clone(), &password_hash, setup_token, now)
                    .map_err(onboarding_error)?;
                Ok((user, password_hash))
            })
        });

    match outcome {
        Ok((user, _password_hash)) => {
            let _ = state.operator_store.clear_login_attempts(
                throttle_subject,
                &remote_addr,
                AuthThrottleAction::Onboarding,
            );
            Ok((
                StatusCode::CREATED,
                Json(OnboardingResponse {
                    user_id: user.id.as_str().to_string(),
                    display_name: user.display_name,
                    role: "superadmin".to_string(),
                }),
            ))
        }
        Err(err) => {
            let _ = state.operator_store.record_login_attempt_failure(
                throttle_subject,
                &remote_addr,
                AuthThrottleAction::Onboarding,
                now,
            );
            Err(err)
        }
    }
}

fn verify_setup_token_for_hashing<'a>(
    state: &AppState,
    setup_token: Option<&'a str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<&'a str, ApiError> {
    let setup_token = setup_token
        .filter(|token| !token.is_empty())
        .ok_or_else(setup_token_required)?;
    let valid = state
        .operator_store
        .verify_onboarding_setup_token(setup_token, now)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    if !valid {
        return Err(setup_token_invalid());
    }
    Ok(setup_token)
}

fn build_onboarding_user(body: &OnboardingRequest) -> Result<(User, String), ApiError> {
    let id = UserId::from_str(&body.user_id).map_err(ApiError::from)?;
    let display_name = body.display_name.trim();
    if display_name.is_empty() || display_name.len() > 64 {
        return Err(ApiError::from(RbacError::InvalidDisplayName));
    }
    if body.password != body.password_confirm {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "password_mismatch",
            "password confirmation does not match",
        ));
    }
    let password_hash = hash_password(&body.password).map_err(password_error)?;
    let user = User {
        id,
        display_name: display_name.to_string(),
        role: OperatorRole::Superadmin,
        created_at: chrono::Utc::now(),
        disabled: false,
    };
    Ok((user, password_hash))
}

fn reject_if_throttled(
    state: &AppState,
    subject: &str,
    remote_addr: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), ApiError> {
    let state = state
        .operator_store
        .login_attempt_state(subject, remote_addr, AuthThrottleAction::Onboarding, now)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    if state.is_locked(now) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many onboarding attempts",
        ));
    }
    Ok(())
}

fn onboarding_throttle_subject(setup_token: Option<&str>) -> &'static str {
    if setup_token.is_some_and(|token| !token.is_empty()) {
        ONBOARDING_THROTTLE_SUBJECT_PRESENT
    } else {
        ONBOARDING_THROTTLE_SUBJECT_MISSING
    }
}

fn setup_token_required() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "setup_token_required",
        "setup token is required",
    )
}

fn setup_token_invalid() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "setup_token_invalid",
        "setup token is invalid or expired",
    )
}

fn already_bootstrapped() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "already_bootstrapped",
        "onboarding is already complete",
    )
}

fn onboarding_error(error: OnboardingError) -> ApiError {
    match error {
        OnboardingError::AlreadyBootstrapped => already_bootstrapped(),
        OnboardingError::InvalidSetupToken => setup_token_invalid(),
        OnboardingError::UserAlreadyExists(user_id) => ApiError::new(
            StatusCode::CONFLICT,
            "user_already_exists",
            format!("user `{user_id}` already exists"),
        ),
        OnboardingError::Store(message) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
        }
    }
}

fn password_error(error: PasswordError) -> ApiError {
    match error {
        PasswordError::TooShort | PasswordError::TooLong | PasswordError::Invalid => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.to_string(),
            error.to_string(),
        ),
        PasswordError::HashFailed => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "password_hash_failed",
            "password hashing failed",
        ),
    }
}
