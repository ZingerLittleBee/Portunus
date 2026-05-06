//! Control-plane connection lifecycle: TLS dial, Welcome handshake,
//! reconnect with full-jitter exponential backoff.

use std::sync::Arc;
use std::time::Duration;

use forward_proto::v1::{ClientMessage, Hello, ServerMessage, control_client::ControlClient};
use rand::Rng;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};
use tracing::{info, warn};

use crate::bundle::CredentialBundle;

const PROTOCOL_VERSION: &str = "1.0.0";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const HELLO_QUEUE_CAPACITY: usize = 16;

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("tls: {0}")]
    Tls(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("auth: {0}")]
    Auth(String),
    #[error("welcome_missing")]
    WelcomeMissing,
    #[error("token_revoked")]
    TokenRevoked,
    #[error("io: {0}")]
    Io(String),
}

impl ControlError {
    /// Errors that should NOT trigger reconnect (operator must intervene).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::TokenRevoked | Self::Auth(_))
    }
}

/// One-shot connection: dial TLS, attach bearer metadata, open Channel,
/// send Hello, await Welcome. Returns the live duplex once Welcome arrives.
pub async fn connect_once(
    bundle: &CredentialBundle,
    cancel: &CancellationToken,
) -> Result<LiveSession, ControlError> {
    // Pinning model: the bundle carries the server's self-signed leaf cert PEM
    // *and* its SHA-256 fingerprint. `CredentialBundle::read_from` already
    // verified that `sha256(DER(server_cert_pem)) == server_cert_sha256` at
    // load time, so trusting `server_cert_pem` here is equivalent to trusting
    // the pin. We pass it as the *only* CA — system roots are not consulted.
    let ca = Certificate::from_pem(bundle.server_cert_pem.as_bytes());
    let endpoint = Endpoint::from_shared(format!("https://{}", bundle.server_endpoint))
        .map_err(|e| ControlError::Transport(e.to_string()))?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(ca)
                .domain_name(extract_host(&bundle.server_endpoint)),
        )
        .map_err(|e| ControlError::Tls(e.to_string()))?;

    let channel = endpoint
        .connect()
        .await
        .map_err(|e| ControlError::Transport(format_chain(&e)))?;

    let token = bundle.token.clone();
    let mut grpc = ControlClient::with_interceptor(channel, move |mut req: Request<()>| {
        let value: MetadataValue<_> = format!("Bearer {token}")
            .parse()
            .map_err(|_| tonic::Status::unauthenticated("malformed_token"))?;
        req.metadata_mut().insert("authorization", value);
        Ok(req)
    });

    let (outbound_tx, outbound_rx) = mpsc::channel::<ClientMessage>(HELLO_QUEUE_CAPACITY);

    let hello = ClientMessage {
        payload: Some(forward_proto::v1::client_message::Payload::Hello(Hello {
            protocol_version: PROTOCOL_VERSION.to_string(),
            client_version: CLIENT_VERSION.to_string(),
        })),
    };
    outbound_tx
        .send(hello)
        .await
        .map_err(|e| ControlError::Io(e.to_string()))?;

    let outbound_stream = ReceiverStream::new(outbound_rx);
    let response = grpc
        .channel(Request::new(outbound_stream))
        .await
        .map_err(|status| status_to_error(&status))?;
    let mut inbound = response.into_inner();

    // Await Welcome.
    let first = tokio::select! {
        () = cancel.cancelled() => return Err(ControlError::Io("cancelled".into())),
        first = inbound.next() => first,
    };
    let welcome = match first {
        Some(Ok(msg)) => match msg.payload {
            Some(forward_proto::v1::server_message::Payload::Welcome(w)) => w,
            _ => return Err(ControlError::WelcomeMissing),
        },
        Some(Err(status)) => return Err(status_to_error(&status)),
        None => return Err(ControlError::WelcomeMissing),
    };
    info!(
        event = "control.connected",
        server_version = %welcome.server_version,
    );

    Ok(LiveSession {
        outbound: outbound_tx,
        inbound: Box::pin(inbound),
    })
}

fn format_chain<E: std::error::Error + std::fmt::Debug + 'static>(e: &E) -> String {
    let mut out = format!("{e} | debug={e:?}");
    let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
    while let Some(s) = src {
        out.push_str(" -> ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

fn extract_host(endpoint: &str) -> String {
    endpoint
        .rsplit_once(':')
        .map_or_else(|| endpoint.to_string(), |(h, _)| h.to_string())
}

fn status_to_error(status: &tonic::Status) -> ControlError {
    let msg = status.message().to_string();
    if status.code() == tonic::Code::Unauthenticated {
        if msg == "token_revoked" {
            ControlError::TokenRevoked
        } else {
            ControlError::Auth(msg)
        }
    } else {
        ControlError::Transport(format!("{:?}: {msg}", status.code()))
    }
}

pub struct LiveSession {
    // Used by US2 (RuleStatus acks) and US3 (StatsReport).
    #[allow(dead_code)]
    pub outbound: mpsc::Sender<ClientMessage>,
    pub inbound: std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<ServerMessage, tonic::Status>> + Send + 'static>,
    >,
}

/// Reconnect loop with full-jitter exponential backoff.
pub async fn run_with_reconnect(
    bundle: Arc<CredentialBundle>,
    initial_delay_ms: u64,
    max_delay_secs: u64,
    cancel: CancellationToken,
) {
    let mut attempt: u32 = 0;
    let max_delay = Duration::from_secs(max_delay_secs);
    loop {
        if cancel.is_cancelled() {
            return;
        }
        info!(event = "control.connecting", attempt = attempt + 1);
        match connect_once(&bundle, &cancel).await {
            Ok(session) => {
                attempt = 0;
                pump(session, &cancel).await;
                info!(event = "control.disconnected");
            }
            Err(e) if e.is_terminal() => {
                warn!(event = "control.terminal", error = %e);
                return;
            }
            Err(e) => {
                warn!(event = "control.connect_failed", error = %e);
            }
        }
        let delay = jittered_backoff(attempt, initial_delay_ms, max_delay);
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(delay) => {},
        }
    }
}

fn jittered_backoff(attempt: u32, base_ms: u64, max: Duration) -> Duration {
    let exp = base_ms.saturating_mul(2u64.saturating_pow(attempt));
    let cap = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
    let upper = exp.min(cap).max(1);
    let chosen = rand::thread_rng().gen_range(0..=upper);
    Duration::from_millis(chosen)
}

async fn pump(mut session: LiveSession, cancel: &CancellationToken) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            msg = session.inbound.next() => match msg {
                Some(Ok(server_msg)) => handle_server_message(server_msg),
                Some(Err(status)) => {
                    warn!(event = "control.stream_error", error = %status);
                    break;
                }
                None => break,
            }
        }
    }
}

fn handle_server_message(_msg: ServerMessage) {
    // US2 wires RuleUpdate into the forwarder; for US1 we accept and drop
    // every payload. Welcome is consumed before pump starts.
}
