//! WebSocket remote API.
//!
//! Clients send JSON [`ShowCommand`]s, e.g. `{"cmd":"go"}`,
//! `{"cmd":"go_cue","number":"12"}`, `{"cmd":"panic","hard":false}`, and
//! receive `{"type":"ack"}` / `{"type":"error",...}` plus a status JSON push
//! whenever the show publishes one via [`RemoteServer::publish_status`].

use crate::hub::{ControlEvent, ControlSource};
use crate::ControlError;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use livestage_core::ShowCommand;
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

pub struct RemoteServer {
    status: watch::Sender<String>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub port: u16,
}

impl RemoteServer {
    pub fn start(port: u16, tx: Sender<ControlEvent>) -> Result<Self, ControlError> {
        let (status, status_rx) = watch::channel(String::from("{}"));
        let (shutdown, shutdown_rx) = oneshot::channel();
        let std_listener = std::net::TcpListener::bind(("0.0.0.0", port))?;
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
                    _ = accept_loop(listener, tx, status_rx) => {}
                }
            });
        })?;
        tracing::info!(port, "WebSocket remote API listening");
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

async fn accept_loop(listener: tokio::net::TcpListener, tx: Sender<ControlEvent>, status: watch::Receiver<String>) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let (tx, mut status) = (tx.clone(), status.clone());
        tokio::spawn(async move {
            let Ok(ws) = tokio_tungstenite::accept_async(stream).await else { return };
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
        let s = RemoteServer::start(0, hub.sender()).unwrap();
        assert_ne!(s.port, 0);
        s.publish_status("{\"ok\":true}".into());
        drop(s);
    }
}
