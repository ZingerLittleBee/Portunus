//! 006-management-web-ui T063: serve the embedded SPA at the operator
//! HTTP listener's root path.
//!
//! `webui/dist/` is compile-time-embedded via `rust-embed`. Requests
//! for an existing static file (hashed asset, image, font) get the
//! file with the right `Content-Type`; everything else falls back to
//! `index.html` so the SPA's history-API router (React Router) can
//! resolve client-side routes like `/users/alice`.
//!
//! The fallback handler is mounted via `Router::fallback`, so all
//! the `/v1/*` and `/metrics` routes registered in
//! `operator::http::router` and `serve.rs` take precedence — the SPA
//! never shadows an API route.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../webui/dist"]
// Sourcemaps and the rollup-visualizer report are dev-only artefacts;
// excluding them shaves ~600 KB from the release binary.
#[exclude = "*.map"]
#[exclude = "stats.html"]
struct Assets;

const INDEX: &str = "index.html";

/// Serve an embedded asset. Path is taken from the request URI's
/// `path()` (leading `/` stripped). Unknown paths fall back to
/// `index.html` for the SPA history fallback. Hashed assets (anything
/// in `assets/`) get an immutable cache hint; `index.html` is set to
/// always-revalidate so updates ship immediately on the next page load.
pub async fn serve_webui(uri: Uri) -> Response {
    let raw_path = uri.path().trim_start_matches('/');
    let path = if raw_path.is_empty() { INDEX } else { raw_path };

    // 1. Try the requested asset.
    if let Some(file) = Assets::get(path) {
        return build_response(path, file.data.as_ref());
    }

    // 2. SPA history fallback — anything that isn't a real file under
    //    the dist tree gets the SPA shell so client-side routing can
    //    resolve.
    if let Some(index) = Assets::get(INDEX) {
        return build_response(INDEX, index.data.as_ref());
    }

    // 3. No assets at all (PORTUNUS_SKIP_WEBUI=1 build with stub
    //    missing). Return a friendly 404 instead of a panic.
    (
        StatusCode::NOT_FOUND,
        "operator Web UI is not bundled in this binary",
    )
        .into_response()
}

fn build_response(path: &str, body: &[u8]) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path == INDEX {
        // SPA shell: always revalidate so stale shells don't pin the
        // SPA at an old asset bundle.
        "public, max-age=0, must-revalidate"
    } else if path.starts_with("assets/") {
        // Hashed file names (vite emits `assets/index-<hash>.js`).
        // Safe to cache aggressively.
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=300"
    };

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.essence_str())
        .header(header::CACHE_CONTROL, HeaderValue::from_static(cache))
        .body(Body::from(body.to_vec()))
        .expect("response builds");
    // Defense in depth: never let the SPA be embedded inside a
    // third-party page.
    response
        .headers_mut()
        .insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    response
}
