//! CSRF checks for cookie-authenticated operator writes.

use axum::{
    body::Body,
    extract::Request,
    http::{Method, header},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CsrfError {
    MissingOrigin,
    OriginMismatch,
    MissingHeader,
    UnsupportedContentType,
}

impl CsrfError {
    #[must_use]
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::MissingOrigin => "csrf_origin_required",
            Self::OriginMismatch => "csrf_origin_mismatch",
            Self::MissingHeader => "csrf_header_required",
            Self::UnsupportedContentType => "csrf_content_type_required",
        }
    }
}

#[must_use]
pub(crate) fn requires_csrf(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

pub(crate) fn verify(req: &Request<Body>, operator_origin: &str) -> Result<(), CsrfError> {
    if !requires_csrf(req.method()) {
        return Ok(());
    }

    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(CsrfError::MissingOrigin)?;
    if origin != operator_origin {
        return Err(CsrfError::OriginMismatch);
    }

    let csrf = req
        .headers()
        .get("x-portunus-csrf")
        .and_then(|value| value.to_str().ok())
        .ok_or(CsrfError::MissingHeader)?;
    if csrf != "1" {
        return Err(CsrfError::MissingHeader);
    }

    if request_has_body(req) {
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .ok_or(CsrfError::UnsupportedContentType)?;
        if !content_type
            .split(';')
            .next()
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
        {
            return Err(CsrfError::UnsupportedContentType);
        }
    }

    Ok(())
}

fn request_has_body(req: &Request<Body>) -> bool {
    req.headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|len| len > 0)
        || req.headers().contains_key(header::TRANSFER_ENCODING)
}
