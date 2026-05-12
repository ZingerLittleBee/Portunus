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

/// Verify the CSRF posture of a cookie-authenticated mutating request.
///
/// `expected_origin = Some(s)` — strict string match against `s` (used when
/// the operator explicitly configured `operator_http_public_origin`, e.g.
/// behind a reverse proxy or a public hostname).
///
/// `expected_origin = None` — same-origin check: the `Origin` header's
/// authority (host\[:port\]) must equal the request's `Host` header. This is
/// the zero-config default and mirrors Grafana / Caddy / Gitea: it works for
/// `localhost`, `127.0.0.1`, LAN IPs, and any reverse-proxy hostname without
/// asking the operator to pre-declare which one they'll use. DNS rebinding
/// is not a concern because the session cookie is origin-scoped — an
/// attacker controlled host can pass the same-origin check but cannot
/// produce a valid session.
pub(crate) fn verify(
    req: &Request<Body>,
    expected_origin: Option<&str>,
) -> Result<(), CsrfError> {
    if !requires_csrf(req.method()) {
        return Ok(());
    }

    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(CsrfError::MissingOrigin)?;

    let origin_ok = match expected_origin {
        Some(expected) => origin == expected,
        None => same_origin(origin, req),
    };
    if !origin_ok {
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

fn same_origin(origin: &str, req: &Request<Body>) -> bool {
    let Some(authority) = strip_scheme(origin) else {
        return false;
    };
    let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    authority == host
}

fn strip_scheme(origin: &str) -> Option<&str> {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
}

fn request_has_body(req: &Request<Body>) -> bool {
    req.headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|len| len > 0)
        || req.headers().contains_key(header::TRANSFER_ENCODING)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: Method, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri("/");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Body::empty()).expect("request")
    }

    #[test]
    fn get_skips_csrf() {
        let r = req(Method::GET, &[]);
        assert!(verify(&r, None).is_ok());
        assert!(verify(&r, Some("http://example.test")).is_ok());
    }

    #[test]
    fn same_origin_matches_localhost_with_host_header() {
        let r = req(
            Method::POST,
            &[
                ("origin", "http://localhost:7080"),
                ("host", "localhost:7080"),
                ("x-portunus-csrf", "1"),
            ],
        );
        assert!(verify(&r, None).is_ok());
    }

    #[test]
    fn same_origin_matches_loopback_ip_with_host_header() {
        let r = req(
            Method::POST,
            &[
                ("origin", "http://127.0.0.1:7080"),
                ("host", "127.0.0.1:7080"),
                ("x-portunus-csrf", "1"),
            ],
        );
        assert!(verify(&r, None).is_ok());
    }

    #[test]
    fn same_origin_rejects_cross_origin_request() {
        let r = req(
            Method::POST,
            &[
                ("origin", "http://evil.example"),
                ("host", "127.0.0.1:7080"),
                ("x-portunus-csrf", "1"),
            ],
        );
        assert_eq!(verify(&r, None), Err(CsrfError::OriginMismatch));
    }

    #[test]
    fn same_origin_requires_host_header() {
        let r = req(
            Method::POST,
            &[
                ("origin", "http://127.0.0.1:7080"),
                ("x-portunus-csrf", "1"),
            ],
        );
        assert_eq!(verify(&r, None), Err(CsrfError::OriginMismatch));
    }

    #[test]
    fn explicit_origin_overrides_same_origin() {
        let r = req(
            Method::POST,
            &[
                ("origin", "https://ops.example.com"),
                ("host", "ops.example.com"),
                ("x-portunus-csrf", "1"),
            ],
        );
        assert!(verify(&r, Some("https://ops.example.com")).is_ok());
        assert_eq!(
            verify(&r, Some("https://other.example.com")),
            Err(CsrfError::OriginMismatch)
        );
    }

    #[test]
    fn missing_origin_header_rejected() {
        let r = req(Method::POST, &[("x-portunus-csrf", "1")]);
        assert_eq!(verify(&r, None), Err(CsrfError::MissingOrigin));
    }
}
