//! Structural guard against the v2.0 authorization regression.
//!
//! Every handler mounted on the *protected* operator-HTTP router must
//! take an `Extension<OperatorIdentity>` so it is able to make an
//! authorization decision. A protected handler that omits it is
//! reachable by any authenticated User-role operator with no
//! role/ownership check — the exact shape of the cross-tenant privilege
//! escalation fixed alongside this test (post_revoke / delete_client /
//! put_client / patch_client_name and the owner-cap handlers).
//!
//! axum's `Router` does not expose its route table for runtime
//! introspection, so this is a source-level assertion: it parses the
//! `protected` router block in `http.rs`, resolves each referenced
//! handler to its `async fn` signature across `src/operator/*.rs`, and
//! fails if any protected handler's parameter list lacks
//! `OperatorIdentity` — unless the handler is on an explicit, documented
//! allowlist. Taking the identity does not by itself prove the handler
//! authorizes correctly, but its ABSENCE proves it cannot, which is the
//! failure mode we are guarding against.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// Handlers intentionally mounted on the protected router WITHOUT an
/// `OperatorIdentity`. Each entry must carry a justification and is
/// removed when the handler is hardened — `allowlist_has_no_stale_entries`
/// fails if an entry stops being an actual unguarded protected handler.
const ALLOWLIST: &[&str] = &[
    // GET /v1/clients — the Web UI exposes the client list to User-role
    // operators by design (sidebar nav is unconditional). Correct
    // hardening is grant-scoped read filtering, tracked as a follow-up;
    // a blanket superadmin gate here would break the User clients page.
    "get_clients",
];

fn operator_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/operator")
}

/// `(file_name, contents)` for every `src/operator/*.rs`, sorted by name
/// so handler resolution is deterministic across machines / CI.
fn operator_sources() -> Vec<(String, String)> {
    let mut paths: Vec<PathBuf> = fs::read_dir(operator_dir())
        .expect("read src/operator")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&p).expect("read source file"))
        })
        .collect()
}

/// The `protected` router block: from `let protected = Router::new()`
/// up to its terminating `.route_layer(`.
fn protected_block(http: &str) -> &str {
    let start = http
        .find("let protected = Router::new()")
        .expect("protected router block present");
    let rest = &http[start..];
    let end = rest
        .find(".route_layer(")
        .expect("protected router terminated by .route_layer(");
    &rest[..end]
}

/// Final identifiers of every `method(handler_path)` call in the block,
/// for the five routing verbs matched as word-bounded calls.
fn extract_handlers(block: &str) -> BTreeSet<String> {
    const METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];
    let bytes = block.as_bytes();
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut handlers = BTreeSet::new();
    for verb in METHODS {
        let pat = format!("{verb}(");
        let mut from = 0;
        while let Some(rel) = block[from..].find(&pat) {
            let at = from + rel;
            from = at + pat.len();
            // Word boundary: the char before the verb must not be part of
            // a longer identifier (so `widget(` never matches `get(`).
            if at > 0 && is_ident(bytes[at - 1]) {
                continue;
            }
            let mut end = from;
            while end < bytes.len() {
                let c = bytes[end];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b':' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > from {
                let path = &block[from..end];
                handlers.insert(path.rsplit("::").next().unwrap().to_string());
            }
        }
    }
    handlers
}

/// Every `async fn <name>(...)` declaration in the corpus, as
/// `(file_name, takes_operator_identity)`. Used to detect ambiguous
/// (duplicate) handler names as well as the identity-parameter check.
fn declarations(sources: &[(String, String)], name: &str) -> Vec<(String, bool)> {
    let needle = format!("async fn {name}(");
    let mut out = Vec::new();
    for (file, src) in sources {
        let mut from = 0;
        while let Some(rel) = src[from..].find(&needle) {
            let open = from + rel + needle.len() - 1; // index of the params '('
            from = open + 1;
            let mut depth = 0i32;
            for (j, &c) in src.as_bytes().iter().enumerate().skip(open) {
                match c {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            out.push((file.clone(), src[open..=j].contains("OperatorIdentity")));
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// Whether the handler's `async fn` takes an `OperatorIdentity`. `None`
/// if no such fn exists. Panics if the name is declared in more than one
/// place — an ambiguous resolution would make the guard unsound.
fn takes_identity(sources: &[(String, String)], name: &str) -> Option<bool> {
    let decls = declarations(sources, name);
    assert!(
        decls.len() <= 1,
        "handler `{name}` has {} `async fn` declarations across src/operator ({:?}); \
         the guard cannot resolve it unambiguously — rename or scope the duplicate",
        decls.len(),
        decls.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(),
    );
    decls.first().map(|&(_, takes)| takes)
}

#[test]
fn every_protected_handler_takes_operator_identity() {
    let http = fs::read_to_string(operator_dir().join("http.rs")).expect("read http.rs");
    let sources = operator_sources();
    let handlers = extract_handlers(protected_block(&http));

    // Guard against the parser silently extracting nothing.
    assert!(
        handlers.len() > 20,
        "expected to extract the full protected handler set, got {}",
        handlers.len()
    );

    let allow: BTreeSet<&str> = ALLOWLIST.iter().copied().collect();
    let mut offenders = Vec::new();
    for handler in &handlers {
        if allow.contains(handler.as_str()) {
            continue;
        }
        match takes_identity(&sources, handler) {
            Some(true) => {}
            Some(false) => offenders.push(format!("{handler} (no OperatorIdentity parameter)")),
            None => offenders.push(format!("{handler} (async fn signature not found)")),
        }
    }
    assert!(
        offenders.is_empty(),
        "protected operator-HTTP handlers cannot authorize (no OperatorIdentity):\n  {}\n\n\
         Add `Extension<OperatorIdentity>` plus an rbac check, or — if the route is \
         intentionally reachable by User-role callers — add it to ALLOWLIST with a justification.",
        offenders.join("\n  ")
    );
}

#[test]
fn exactly_one_protected_route_layer() {
    // The guard scans the single `protected` router (the one block
    // terminated by `.route_layer(auth_middleware)`). A second
    // auth-gated sub-router merged into the app would put its handlers
    // outside the scanned block and silently escape the guard. Trip
    // loudly if a second `.route_layer(` appears so the scan is widened.
    let http = fs::read_to_string(operator_dir().join("http.rs")).expect("read http.rs");
    let layers = http.matches(".route_layer(").count();
    assert_eq!(
        layers, 1,
        "expected exactly one `.route_layer(` (the protected router); found {layers}. \
         A new auth-gated sub-router must be wired into this guard's scan."
    );
}

#[test]
fn allowlist_has_no_stale_entries() {
    let http = fs::read_to_string(operator_dir().join("http.rs")).expect("read http.rs");
    let sources = operator_sources();
    let handlers = extract_handlers(protected_block(&http));

    for &allowed in ALLOWLIST {
        assert!(
            handlers.contains(allowed),
            "ALLOWLIST entry `{allowed}` is not a protected route handler — remove it"
        );
        assert_eq!(
            takes_identity(&sources, allowed),
            Some(false),
            "ALLOWLIST entry `{allowed}` now takes an OperatorIdentity — drop it from ALLOWLIST"
        );
    }
}
