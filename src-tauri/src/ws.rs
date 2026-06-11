//! Local websocket server so the overlay can be captured in OBS as a browser
//! source (obs-html/overlay.html), matching the original app's streaming setup.

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct WsServer {
    pub tx: broadcast::Sender<String>,
}

impl WsServer {
    pub fn new() -> Self {
        Self {
            tx: broadcast::channel(16).0,
        }
    }

    pub fn send_player_data(&self, data: &Value) {
        let msg = serde_json::json!({ "type": "player_data", "data": data });
        let _ = self.tx.send(msg.to_string());
    }

    pub fn send_colors(&self, colors: &[[f64; 4]]) {
        let msg = serde_json::json!({ "type": "color", "data": colors });
        let _ = self.tx.send(msg.to_string());
    }
}

/// Runs forever; on connect, sends current colors + last data, then forwards broadcasts.
pub async fn run(
    server: WsServer,
    port: u16,
    initial: impl Fn() -> Vec<String> + Send + Sync + 'static,
) {
    let listener = match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("websocket server failed to bind port {port}: {e}");
            return;
        }
    };
    log::info!("websocket server listening on ws://127.0.0.1:{port}");
    let initial = std::sync::Arc::new(initial);

    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let mut rx = server.tx.subscribe();
        let initial = initial.clone();
        tokio::spawn(async move {
            let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };
            let (mut sink, mut src) = ws.split();
            for msg in initial() {
                let _ = sink.send(msg.into()).await;
            }
            loop {
                tokio::select! {
                    m = rx.recv() => match m {
                        Ok(text) => {
                            if sink.send(text.into()).await.is_err() { break; }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    },
                    m = src.next() => if m.is_none() || matches!(m, Some(Err(_))) { break; },
                }
            }
        });
    }
}
