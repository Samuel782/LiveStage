//! WebSocket remote API.
//!
//! Clients send JSON [`ShowCommand`]s, e.g. `{"cmd":"go"}`,
//! `{"cmd":"go_cue","number":"12"}`, `{"cmd":"panic","hard":false}`, and
//! receive `{"type":"ack"}` / `{"type":"error",...}` plus a status JSON push
//! whenever the show publishes one via [`RemoteServer::publish_status`].
//!
//! Every connection must present a session token in its handshake
//! (`Authorization: Bearer <token>` or `?token=<token>`, see [`crate::auth`]);
//! others are refused with HTTP 401 before any command is read. Being
//! authenticated, the API may run every command, sensitive ones included.
//! The server listens on 127.0.0.1 unless external access is enabled.

use crate::auth::RemoteAuth;
use crate::hub::{ControlEvent, ControlSource};
use crate::ControlError;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use livestage_core::ShowCommand;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

/// A connection must finish its (authenticated) handshake within this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RemoteServer {
    status: watch::Sender<String>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub port: u16,
}

impl RemoteServer {
    /// Listens on `bind:port` (127.0.0.1 unless external access is on);
    /// only clients with a token `auth` verifies get in.
    pub fn start(bind: IpAddr, port: u16, tx: Sender<ControlEvent>, auth: Arc<RemoteAuth>) -> Result<Self, ControlError> {
        let (status, status_rx) = watch::channel(String::from("{}"));
        let (shutdown, shutdown_rx) = oneshot::channel();
        let std_listener = std::net::TcpListener::bind((bind, port))?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();

        let thread = std::thread::Builder::new().name("livestage-ws".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("websocket runtime: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => return tracing::error!("websocket listener: {e}"),
                };
                tokio::select! {
                    _ = shutdown_rx => {}
                    _ = accept_loop(listener, tx, status_rx, auth) => {}
                }
            });
        })?;
        tracing::info!(%bind, port, "WebSocket remote API listening (token required)");
        Ok(Self { status, shutdown: Some(shutdown), thread: Some(thread), port })
    }

    /// Pushes a status document to every connected client.
    pub fn publish_status(&self, json: String) {
        self.status.send_if_modified(|cur| {
            if *cur != json {
                *cur = json;
                true
            } else {
                false
            }
        });
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        if let Some(s) = self.shutdown.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Checks the handshake's token; returns the client's label.
fn authorize(auth: &RemoteAuth, req: &Request) -> Result<String, crate::auth::AuthError> {
    match crate::auth::token_from_request(req) {
        Some(token) => auth.verify(&token, SystemTime::now()).map(|info| info.label),
        None => Err(crate::auth::AuthError::Missing),
    }
}

/// The handshake answer for a refused client: HTTP 401.
fn unauthorized(e: crate::auth::AuthError) -> ErrorResponse {
    let mut resp = ErrorResponse::new(Some(format!("LiveStage remote: {e}")));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp
}

async fn accept_loop(listener: tokio::net::TcpListener, tx: Sender<ControlEvent>, status: watch::Receiver<String>, auth: Arc<RemoteAuth>) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let (tx, mut status, auth) = (tx.clone(), status.clone(), auth.clone());
        tokio::spawn(async move {
            let mut who = None;
            // The callback's error type is tungstenite's HTTP response.
            #[allow(clippy::result_large_err)]
            let callback = |req: &Request, resp: Response| match authorize(&auth, req) {
                Ok(label) => {
                    who = Some(label);
                    Ok(resp)
                }
                Err(e) => Err(unauthorized(e)),
            };
            let ws = match tokio::time::timeout(HANDSHAKE_TIMEOUT, tokio_tungstenite::accept_hdr_async(stream, callback)).await {
                Ok(Ok(ws)) => ws,
                Ok(Err(e)) => return tracing::warn!(%peer, "remote client refused: {e}"),
                Err(_) => return tracing::warn!(%peer, "remote client refused: handshake timed out"),
            };
            let client = who.unwrap_or_default();
            let peer = format!("{peer} ({client})");
            tracing::info!(%peer, "remote client connected");
            let (mut sink, mut source) = ws.split();
            let initial = status.borrow_and_update().clone();
            if sink.send(Message::Text(initial.into())).await.is_err() {
                return;
            }
            loop {
                tokio::select! {
                    msg = source.next() => {
                        let Some(Ok(msg)) = msg else { break };
                        let reply = match msg {
                            Message::Text(text) => match serde_json::from_str::<ShowCommand>(text.as_str()) {
                                Ok(command) => {
                                    let _ = tx.send(ControlEvent { source: ControlSource::WebSocket { peer: peer.to_string() }, command });
                                    r#"{"type":"ack"}"#.to_string()
                                }
                                Err(e) => serde_json::json!({"type": "error", "message": e.to_string()}).to_string(),
                            },
                            Message::Ping(p) => { let _ = sink.send(Message::Pong(p)).await; continue; }
                            Message::Close(_) => break,
                            _ => continue,
                        };
                        if sink.send(Message::Text(reply.into())).await.is_err() { break; }
                    }
                    changed = status.changed() => {
                        if changed.is_err() { break; }
                        let doc = status.borrow_and_update().clone();
                        if sink.send(Message::Text(doc.into())).await.is_err() { break; }
                    }
                }
            }
            tracing::info!(%peer, "remote client disconnected");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_json_protocol() {
        let c: ShowCommand = serde_json::from_str(r#"{"cmd":"go_cue","number":"12"}"#).unwrap();
        assert_eq!(c, ShowCommand::GoCue { number: "12".into() });
        let c: ShowCommand = serde_json::from_str(r#"{"cmd":"panic","hard":true}"#).unwrap();
        assert_eq!(c, ShowCommand::Panic { hard: true });
        let c: ShowCommand = serde_json::from_str(r#"{"cmd":"go","list":null}"#).unwrap();
        assert_eq!(c, ShowCommand::Go { list: None });
    }

    #[test]
    fn server_starts_and_stops() {
        let hub = crate::ControlHub::default();
        let auth = Arc::new(RemoteAuth::from_secret([1; 32]));
        let s = RemoteServer::start(IpAddr::from([127, 0, 0, 1]), 0, hub.sender(), auth).unwrap();
        assert_ne!(s.port, 0);
        s.publish_status("{\"ok\":true}".into());
        drop(s);
    }

    /// A real socket: no token, a forged token and an expired token are
    /// refused with 401 and send nothing; a valid token gets the status
    /// document and its commands (sensitive ones too) reach the hub.
    #[test]
    fn handshake_requires_a_valid_token() {
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error as WsError};
        let hub = crate::ControlHub::default();
        let auth = Arc::new(RemoteAuth::from_secret([3; 32]));
        let server = RemoteServer::start(IpAddr::from([127, 0, 0, 1]), 0, hub.sender(), auth.clone()).unwrap();
        server.publish_status(r#"{"type":"status"}"#.into());
        let url = format!("ws://127.0.0.1:{}/", server.port);
        let now = SystemTime::now();
        let good = auth.issue("test-client", Duration::from_secs(60), now);
        let expired = auth.issue("old", Duration::from_secs(0), now);
        let forged = RemoteAuth::from_secret([4; 32]).issue("mallory", Duration::from_secs(60), now);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let connect = |token: Option<String>, query: bool| {
                let url = match (&token, query) {
                    (Some(t), true) => format!("{url}?token={t}"),
                    _ => url.clone(),
                };
                async move {
                    let mut req = url.into_client_request().unwrap();
                    if let (Some(t), false) = (token, query) {
                        req.headers_mut().insert("Authorization", format!("Bearer {t}").parse().unwrap());
                    }
                    tokio_tungstenite::connect_async(req).await
                }
            };
            for (token, what) in [(None, "no token"), (Some(forged), "forged"), (Some(expired), "expired")] {
                match connect(token, false).await {
                    Err(WsError::Http(resp)) => assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{what}"),
                    other => panic!("{what}: expected 401, got {:?}", other.map(|_| ())),
                }
            }
            for query in [false, true] {
                let (mut ws, _) = connect(Some(good.clone()), query).await.unwrap();
                let first = ws.next().await.unwrap().unwrap();
                assert_eq!(first.into_text().unwrap().as_str(), r#"{"type":"status"}"#);
                ws.send(Message::Text(r#"{"cmd":"panic","hard":false}"#.into())).await.unwrap();
                let ack = ws.next().await.unwrap().unwrap();
                assert_eq!(ack.into_text().unwrap().as_str(), r#"{"type":"ack"}"#);
            }
        });
        let events: Vec<_> = hub.drain().collect();
        assert_eq!(events.len(), 2, "only the authenticated clients' commands: {events:?}");
        assert!(events.iter().all(|e| e.command == ShowCommand::Panic { hard: false }));
        assert!(matches!(&events[0].source, ControlSource::WebSocket { peer } if peer.ends_with("(test-client)")));
    }
}
