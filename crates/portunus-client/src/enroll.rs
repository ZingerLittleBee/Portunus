use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use portunus_core::ClientName;
use portunus_proto::v1::{EnrollClientRequest, client_enrollment_client::ClientEnrollmentClient};
use thiserror::Error;
use tonic::Request;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};

use crate::bundle::CredentialBundle;
use crate::control::extract_host;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentUri {
    pub endpoint: String,
    pub pin_sha256: String,
    pub code: String,
    pub server_cert_pem: String,
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
        let mut cert: Option<String> = None;
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
                "cert" => {
                    let bytes = URL_SAFE_NO_PAD
                        .decode(value.as_bytes())
                        .map_err(|_| EnrollmentUriError::BadCert)?;
                    let pem = String::from_utf8(bytes).map_err(|_| EnrollmentUriError::BadCert)?;
                    cert = Some(pem);
                }
                _ => {}
            }
        }

        Ok(Self {
            endpoint: endpoint.to_string(),
            pin_sha256: pin.ok_or(EnrollmentUriError::MissingField("pin"))?,
            code: code.ok_or(EnrollmentUriError::MissingField("code"))?,
            server_cert_pem: cert.ok_or(EnrollmentUriError::MissingField("cert"))?,
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
    #[error("bad enrollment URI certificate")]
    BadCert,
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
    let preliminary = CredentialBundle::from_enrollment(
        1,
        ClientName::new("enrollment-probe")?,
        parsed.endpoint.clone(),
        parsed.pin_sha256.clone(),
        parsed.server_cert_pem.clone(),
        "probe-token".into(),
    )
    .map_err(EnrollError::EnrollmentMaterial)?;

    let ca = Certificate::from_pem(preliminary.server_cert_pem.as_bytes());
    let endpoint = Endpoint::from_shared(format!("https://{}", parsed.endpoint))
        .map_err(|e| EnrollError::Transport(e.to_string()))?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(ca)
                .domain_name(extract_host(&preliminary.server_endpoint)),
        )
        .map_err(|e| EnrollError::Transport(e.to_string()))?;
    let channel = endpoint
        .connect()
        .await
        .map_err(|e| EnrollError::Transport(e.to_string()))?;
    let mut client = ClientEnrollmentClient::new(channel);
    let response = client
        .enroll(Request::new(EnrollClientRequest { code: parsed.code }))
        .await
        .map_err(|e| EnrollError::Rpc(e.message().to_string()))?
        .into_inner();

    let bundle = CredentialBundle::from_enrollment(
        response.version,
        ClientName::new(response.client_name)?,
        response.server_endpoint,
        response.server_cert_sha256,
        response.server_cert_pem,
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

    const PEM: &str = "-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n";

    #[test]
    fn enrollment_uri_roundtrips_fields() {
        let cert = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(PEM.as_bytes());
        let uri = format!(
            "portunus://control.example.com:7443/enroll?pin=sha256:{}&code=join-code&cert={cert}",
            "a".repeat(64)
        );

        let parsed = EnrollmentUri::parse(&uri).expect("parse");

        assert_eq!(parsed.endpoint, "control.example.com:7443");
        assert_eq!(parsed.pin_sha256, "a".repeat(64));
        assert_eq!(parsed.code, "join-code");
        assert_eq!(parsed.server_cert_pem, PEM);
    }

    #[test]
    fn enrollment_uri_rejects_missing_pin() {
        let cert = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(PEM.as_bytes());
        let uri = format!("portunus://control.example.com:7443/enroll?code=join-code&cert={cert}");

        assert!(matches!(
            EnrollmentUri::parse(&uri).unwrap_err(),
            EnrollmentUriError::MissingField("pin")
        ));
    }

    #[test]
    fn enrollment_uri_rejects_query_pair_without_equals() {
        let cert = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(PEM.as_bytes());
        let uri =
            format!("portunus://control.example.com:7443/enroll?pin&code=join-code&cert={cert}");

        assert!(matches!(
            EnrollmentUri::parse(&uri).unwrap_err(),
            EnrollmentUriError::MalformedQueryPair
        ));
    }
}
