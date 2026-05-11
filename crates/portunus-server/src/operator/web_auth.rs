//! Browser-oriented authentication endpoints.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header},
};
use chrono::Utc;
use portunus_auth::{OperatorAuthenticator, OperatorRole, RbacError, User, UserId};
use serde::{Deserialize, Serialize};

use crate::operator::http::ApiError;
use crate::operator::passwords::{PasswordError, hash_password, verify_password};
use crate::operator::sessions;
use crate::operator::throttle::{AuthThrottleAction, UNKNOWN_AUTH_SUBJECT};
use crate::operator::{auth_layer, csrf};
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

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub user_id: String,
    pub password: String,
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
    reject_if_throttled(
        &state,
        throttle_subject,
        &remote_addr,
        AuthThrottleAction::Onboarding,
        now,
    )?;

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

pub async fn post_auth_login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<(StatusCode, [(header::HeaderName, String); 1]), ApiError> {
    let now = Utc::now();
    let remote_ip = remote_addr.ip().to_string();
    let throttle_subject = login_throttle_subject(&body.user_id);
    reject_if_throttled(
        &state,
        &throttle_subject,
        &remote_ip,
        AuthThrottleAction::Login,
        now,
    )?;

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);

    let outcome = authenticate_login(
        &state,
        &body,
        now,
        remote_ip.clone(),
        user_agent,
        state.operator_http_cookie_secure,
    );

    match outcome {
        Ok(cookie) => {
            let _ = state.operator_store.clear_login_attempts(
                &throttle_subject,
                &remote_ip,
                AuthThrottleAction::Login,
            );
            Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]))
        }
        Err(err) => {
            let _ = state.operator_store.record_login_attempt_failure(
                &throttle_subject,
                &remote_ip,
                AuthThrottleAction::Login,
                now,
            );
            Err(err)
        }
    }
}

pub async fn post_auth_logout(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
) -> Result<(StatusCode, [(header::HeaderName, String); 1]), ApiError> {
    let secret =
        sessions::cookie_value(req.headers(), sessions::SESSION_COOKIE).ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "missing session cookie",
            )
        })?;
    auth_layer::verify_session_secret(&state, secret).map_err(|_| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "credential_invalid",
            "invalid or revoked session",
        )
    })?;
    csrf::verify(&req, &state.operator_http_public_origin).map_err(csrf_error)?;
    let session_hash = sessions::hash_session_secret(secret);
    state
        .operator_store
        .revoke_web_session(&session_hash)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            expired_session_cookie(state.operator_http_cookie_secure),
        )],
    ))
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

fn authenticate_login(
    state: &AppState,
    body: &LoginRequest,
    now: chrono::DateTime<chrono::Utc>,
    remote_addr: String,
    user_agent: Option<String>,
    secure_cookie: bool,
) -> Result<String, ApiError> {
    let user_id = UserId::from_str(&body.user_id).map_err(|_| invalid_login())?;
    let user = state
        .operator_store
        .get_user(&user_id)
        .ok_or_else(invalid_login)?;
    if user.disabled {
        return Err(invalid_login());
    }
    let password_state = state
        .operator_store
        .password_state(&user_id)
        .map_err(|_| invalid_login())?
        .ok_or_else(invalid_login)?;
    verify_password(&body.password, &password_state.hash).map_err(|_| invalid_login())?;

    let secret = sessions::generate_session_secret();
    let session_hash = sessions::hash_session_secret(&secret);
    state
        .operator_store
        .create_web_session(
            &session_hash,
            &user_id,
            now,
            now + sessions::ABSOLUTE_TIMEOUT,
            Some(remote_addr),
            user_agent,
        )
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    Ok(session_cookie(&secret, secure_cookie))
}

fn reject_if_throttled(
    state: &AppState,
    subject: &str,
    remote_addr: &str,
    action: AuthThrottleAction,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), ApiError> {
    let state = state
        .operator_store
        .login_attempt_state(subject, remote_addr, action, now)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    if state.is_locked(now) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many authentication attempts",
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

fn login_throttle_subject(user_id: &str) -> String {
    UserId::from_str(user_id)
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|_| UNKNOWN_AUTH_SUBJECT.to_string())
}

fn session_cookie(secret: &str, secure: bool) -> String {
    let mut cookie = format!(
        "{}={secret}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        sessions::SESSION_COOKIE,
        sessions::ABSOLUTE_TIMEOUT.num_seconds(),
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn expired_session_cookie(secure: bool) -> String {
    let mut cookie = format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        sessions::SESSION_COOKIE,
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn invalid_login() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_login",
        "invalid username or password",
    )
}

fn csrf_error(error: csrf::CsrfError) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        error.code(),
        "csrf verification failed",
    )
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
