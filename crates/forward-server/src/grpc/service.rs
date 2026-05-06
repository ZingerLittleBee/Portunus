//! `Control` service implementation.
//!
//! For US1 the `Channel` rpc handles handshake (await `Hello`, send
//! `Welcome`) and registers the client in [`crate::clients`]. Rule push
//! and stats handling land in US2/US3.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use forward_auth::ClientIdentity;
use forward_proto::v1::{
    ClientMessage, ServerMessage, Welcome, control_server::Control, server_message,
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use crate::clients::StatusWaiters;
use crate::state::AppState;

/// Channel from the operator-side push path into the per-client send-half.
/// Used by US2 to push `RuleUpdates` from the operator HTTP API to a live session.
#[allow(dead_code)]
pub type OutboundSender = mpsc::Sender<Result<ServerMessage, Status>>;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const OUTBOUND_QUEUE_CAPACITY: usize = 32;

pub struct ControlService {
    pub state: Arc<AppState>,
}

impl ControlService {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl Control for ControlService {
    type ChannelStream =
        Pin<Box<dyn Stream<Item = Result<ServerMessage, Status>> + Send + 'static>>;

    async fn channel(
        &self,
        request: Request<Streaming<ClientMessage>>,
    ) -> Result<Response<Self::ChannelStream>, Status> {
        let identity = request
            .extensions()
            .get::<ClientIdentity>()
            .cloned()
            .ok_or_else(|| Status::unauthenticated("missing_identity"))?;
        let remote_addr = request.remote_addr();
        let mut inbound = request.into_inner();
        let state = Arc::clone(&self.state);

        let (tx, rx) = mpsc::channel::<Result<ServerMessage, Status>>(OUTBOUND_QUEUE_CAPACITY);
        let status_waiters: StatusWaiters = Arc::new(Mutex::new(HashMap::new()));

        let cancel_token = state.clients.session_root_token().child_token();
        let session_id = state
            .clients
            .register(
                identity.client_name.clone(),
                remote_addr,
                cancel_token.clone(),
                tx.clone(),
                status_waiters.clone(),
            )
            .await;
        info!(
            event = "client.connected",
            client_name = %identity.client_name,
            remote_addr = ?remote_addr,
            session_id,
        );

        // Send Welcome immediately.
        let welcome = ServerMessage {
            payload: Some(server_message::Payload::Welcome(Welcome {
                server_version: SERVER_VERSION.to_string(),
                server_time_unix_ms: now_ms(),
            })),
        };
        if tx.send(Ok(welcome)).await.is_err() {
            // Receiver dropped before we could even send Welcome; clean up.
            state
                .clients
                .unregister(&identity.client_name, session_id)
                .await;
            return Err(Status::cancelled("client_dropped_before_welcome"));
        }

        let pump_state = Arc::clone(&state);
        let pump_identity = identity.clone();
        let pump_cancel = cancel_token.clone();
        let pump_waiters = status_waiters.clone();
        // Move `tx` into the pump task so the response stream stays open for
        // the lifetime of the session. Without this the sender drops at the
        // end of `channel()`, the receiver yields None right after Welcome,
        // and the client immediately reconnects in a tight loop. (US2 also
        // sends rule pushes through this same channel.)
        tokio::spawn(async move {
            let _outbound = tx;
            loop {
                tokio::select! {
                    () = pump_cancel.cancelled() => {
                        break;
                    }
                    msg = inbound.next() => {
                        match msg {
                            Some(Ok(client_msg)) => {
                                handle_client_message(
                                    &pump_state,
                                    &pump_identity,
                                    &pump_waiters,
                                    client_msg,
                                )
                                .await;
                            }
                            Some(Err(e)) => {
                                warn!(
                                    event = "client.transport_error",
                                    client_name = %pump_identity.client_name,
                                    error = %e,
                                );
                                break;
                            }
                            None => break, // graceful EOF from client
                        }
                    }
                }
            }
            pump_state
                .clients
                .unregister(&pump_identity.client_name, session_id)
                .await;
            info!(
                event = "client.disconnected",
                client_name = %pump_identity.client_name,
                session_id,
            );
        });

        let outbound = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(outbound) as Self::ChannelStream))
    }
}

async fn handle_client_message(
    _state: &AppState,
    identity: &ClientIdentity,
    waiters: &StatusWaiters,
    msg: ClientMessage,
) {
    use forward_proto::v1::client_message::Payload;
    match msg.payload {
        Some(Payload::Hello(h)) => {
            info!(
                event = "client.hello",
                client_name = %identity.client_name,
                protocol_version = %h.protocol_version,
                client_version = %h.client_version,
            );
        }
        Some(Payload::RuleStatus(rs)) => {
            // Hand the status to the operator path waiting on `request_id`.
            // If the waiter isn't there, this is either a late arrival
            // (post-timeout) or the unsolicited Removed echo when the
            // listener's drain finished — both are fine; we just log.
            let request_id = rs.request_id.clone();
            let mut guard = waiters.lock().await;
            if let Some(tx) = guard.remove(&request_id) {
                let _ = tx.send(rs);
            } else {
                info!(
                    event = "client.rule_status_unmatched",
                    client_name = %identity.client_name,
                    request_id = %request_id,
                    rule_id = rs.rule_id,
                );
            }
        }
        Some(Payload::StatsReport(_)) | None => {
            // US3 (StatsReport) / no payload — currently no-op.
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
