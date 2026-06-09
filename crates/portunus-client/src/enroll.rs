use std::path::PathBuf;

use portunus_core::ClientName;
use portunus_proto::v1::{EnrollClientRequest, client_enrollment_client::ClientEnrollmentClient};
use thiserror::Error;
use tonic::Request;

use crate::bundle::CredentialBundle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentUri {
    pub endpoint: String,
    pub pin_sha256: String,
    pub code: String,
}

impl EnrollmentUri {
    pub fn parse(raw: &str) -> Result<Self, EnrollmentUriError> {
        let rest = raw
            .strip_prefix("portunus://")
            .ok_or(EnrollmentUriError::BadScheme)?;
        let (authority_and_path, query) = rest
            .split_once('?')
            .ok_or(EnrollmentUriError::MissingQuery)?;
        let endpoint = authority_and_path
            .strip_suffix("/enroll")
            .ok_or(EnrollmentUriError::BadPath)?;
        if endpoint.is_empty() {
            return Err(EnrollmentUriError::MissingField("endpoint"));
        }

        let mut pin: Option<String> = None;
        let mut code: Option<String> = None;
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                return Err(EnrollmentUriError::MalformedQueryPair);
            };
            match key {
                "pin" => {
                    let value = value
                        .strip_prefix("sha256:")
                        .ok_or(EnrollmentUriError::BadPin)?;
                    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(EnrollmentUriError::BadPin);
                    }
                    pin = Some(value.to_ascii_lowercase());
                }
                "code" => {
                    if value.is_empty() {
                        return Err(EnrollmentUriError::MissingField("code"));
                    }
                    code = Some(value.to_string());
                }
                _ => {}
            }
        }

        Ok(Self {
            endpoint: endpoint.to_string(),
            pin_sha256: pin.ok_or(EnrollmentUriError::MissingField("pin"))?,
            code: code.ok_or(EnrollmentUriError::MissingField("code"))?,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnrollmentUriError {
    #[error("bad enrollment URI scheme")]
    BadScheme,
    #[error("missing enrollment URI query")]
    MissingQuery,
    #[error("bad enrollment URI path")]
    BadPath,
    #[error("missing enrollment URI field: {0}")]
    MissingField(&'static str),
    #[error("malformed enrollment URI query pair")]
    MalformedQueryPair,
    #[error("bad enrollment URI pin")]
    BadPin,
}

#[derive(Debug, Error)]
pub enum EnrollError {
    #[error("enrollment URI: {0}")]
    Uri(#[from] EnrollmentUriError),
    #[error("client name: {0}")]
    ClientName(#[from] portunus_core::ClientNameError),
    #[error("enrollment material: {0}")]
    EnrollmentMaterial(#[source] std::io::Error),
    #[error("write credential bundle: {0}")]
    WriteBundle(#[source] std::io::Error),
    #[error("transport: {0}")]
    Transport(String),
    #[error("rpc: {0}")]
    Rpc(String),
}

pub async fn enroll(raw_uri: &str, out: Option<PathBuf>) -> Result<PathBuf, EnrollError> {
    let parsed = EnrollmentUri::parse(raw_uri)?;

    // Dial the enrollment endpoint trusting only the pinned certificate
    // (its SHA-256 fingerprint from the URI). The handshake fails before
    // the join code is sent if the server presents a different cert.
    let channel = crate::tls::pinned_endpoint(&parsed.endpoint, &parsed.pin_sha256)
        .map_err(|e| EnrollError::Transport(e.to_string()))?
        .connect()
        .await
        .map_err(|e| EnrollError::Transport(e.to_string()))?;
    let mut client = ClientEnrollmentClient::new(channel);
    let response = client
        .enroll(Request::new(EnrollClientRequest { code: parsed.code }))
        .await
        .map_err(|e| EnrollError::Rpc(e.message().to_string()))?
        .into_inner();

    let client_id = if response.client_id.is_empty() {
        None // pre-upgrade server: no id on the wire
    } else {
        Some(
            response
                .client_id
                .parse()
                .map_err(|e| EnrollError::Rpc(format!("invalid client_id in bundle: {e}")))?,
        )
    };
    let bundle = CredentialBundle::from_enrollment(
        response.version,
        ClientName::new(response.client_name)?,
        client_id,
        response.server_endpoint,
        response.server_cert_sha256,
        response.token,
    )
    .map_err(EnrollError::EnrollmentMaterial)?;
    let path = out.unwrap_or_else(default_bundle_write_path);
    bundle.write_to(&path).map_err(EnrollError::WriteBundle)?;
    Ok(path)
}

fn default_bundle_write_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg)
            .join("portunus")
            .join("client.bundle.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("portunus")
            .join("client.bundle.json");
    }
    PathBuf::from("./client.bundle.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_uri_roundtrips_fields() {
        let uri = format!(
            "portunus://control.example.com:7443/enroll?pin=sha256:{}&code=join-code",
            "a".repeat(64)
        );

        let parsed = EnrollmentUri::parse(&uri).expect("parse");

        assert_eq!(parsed.endpoint, "control.example.com:7443");
        assert_eq!(parsed.pin_sha256, "a".repeat(64));
        assert_eq!(parsed.code, "join-code");
    }

    #[test]
    fn enrollment_uri_ignores_unknown_keys() {
        // A stray cert= (or any future key) is ignored, not rejected.
        let uri = format!(
            "portunus://control.example.com:7443/enroll?pin=sha256:{}&code=join-code&cert=ignored",
            "a".repeat(64)
        );
        let parsed = EnrollmentUri::parse(&uri).expect("parse");
        assert_eq!(parsed.code, "join-code");
    }

    #[test]
    fn enrollment_uri_rejects_missing_pin() {
        let uri = "portunus://control.example.com:7443/enroll?code=join-code";

        assert!(matches!(
            EnrollmentUri::parse(uri).unwrap_err(),
            EnrollmentUriError::MissingField("pin")
        ));
    }

    #[test]
    fn enrollment_uri_rejects_missing_code() {
        let uri = format!(
            "portunus://control.example.com:7443/enroll?pin=sha256:{}",
            "a".repeat(64)
        );
        assert!(matches!(
            EnrollmentUri::parse(&uri).unwrap_err(),
            EnrollmentUriError::MissingField("code")
        ));
    }

    #[test]
    fn enrollment_uri_rejects_query_pair_without_equals() {
        // `pin` present but malformed (no '=').
        let uri = "portunus://control.example.com:7443/enroll?pin&code=join-code";

        assert!(matches!(
            EnrollmentUri::parse(uri).unwrap_err(),
            EnrollmentUriError::MalformedQueryPair
        ));
    }
}
