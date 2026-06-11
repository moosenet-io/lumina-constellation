//! EDGE-08: HTTP channel adapter (feature-gated).
//!
//! Serves a minimal POST `/` endpoint.  Each request body is treated as a
//! plain-text message and forwarded to the core loop via a [`ChannelMessage`].
//! Responses are stored in a shared map keyed by `request_id` so that callers
//! can poll for the result via GET `/?id=<request_id>`.
//!
//! This module is only compiled when the `http` cargo feature is enabled.

#[cfg(feature = "http")]
pub use self::inner::HttpChannel;

#[cfg(feature = "http")]
mod inner {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use async_trait::async_trait;
    use tokio::sync::{mpsc, Mutex};

    use crate::channels::{Channel, ChannelMessage, MessageContext};
    use crate::error::{LuminaError, Result};

    /// Pending responses keyed by request_id.
    ///
    /// Entries older than `RESPONSE_TTL_SECS` are evicted on each insert.
    type ResponseStore = Arc<Mutex<HashMap<String, (String, std::time::Instant)>>>;

    /// Maximum number of pending responses retained in memory.
    const RESPONSE_CAP: usize = 1024;
    /// Seconds after which a response entry is eligible for eviction.
    const RESPONSE_TTL_SECS: u64 = 300;

    /// HTTP channel adapter.
    ///
    /// Binds to `addr`, accepts `POST /` requests with a text body, and
    /// forwards each one as a [`ChannelMessage`].  Responses sent via
    /// [`send_response`] are stored in memory and can be retrieved with
    /// `GET /?id=<request_id>`.
    pub struct HttpChannel {
        addr: SocketAddr,
        responses: ResponseStore,
        /// Handle to the background axum server task, held for cancellation.
        server_handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl HttpChannel {
        /// Create a new HTTP channel that will listen on `addr`.
        pub fn new(addr: SocketAddr) -> Self {
            Self {
                addr,
                responses: Arc::new(Mutex::new(HashMap::new())),
                server_handle: None,
            }
        }
    }

    #[async_trait]
    impl Channel for HttpChannel {
        fn name(&self) -> &str {
            "http"
        }

        async fn start(
            &mut self,
            sender: mpsc::Sender<ChannelMessage>,
        ) -> Result<()> {
            use axum::{
                extract::{Query, State},
                http::StatusCode,
                response::IntoResponse,
                routing::{get, post},
                Router,
            };

            let responses = self.responses.clone();
            let addr = self.addr;

            // POST / — receive a message
            async fn post_message(
                State((tx, _responses)): State<(mpsc::Sender<ChannelMessage>, ResponseStore)>,
                body: String,
            ) -> impl IntoResponse {
                let body = body.trim().to_string();
                if body.is_empty() {
                    return (StatusCode::BAD_REQUEST, "empty body".to_string());
                }
                let request_id = random_request_id();
                let msg = ChannelMessage {
                    source_channel: "http".to_string(),
                    sender_id: "http-client".to_string(),
                    content: body,
                    context: MessageContext {
                        request_id: Some(request_id.clone()),
                        ..Default::default()
                    },
                    timestamp: std::time::SystemTime::now(),
                };
                if tx.send(msg).await.is_err() {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "channel closed".to_string(),
                    );
                }
                (StatusCode::ACCEPTED, request_id)
            }

            // GET /?id=<request_id> — poll for response
            async fn get_response(
                State((_tx, responses)): State<(mpsc::Sender<ChannelMessage>, ResponseStore)>,
                Query(params): Query<HashMap<String, String>>,
            ) -> impl IntoResponse {
                match params.get("id") {
                    None => (StatusCode::BAD_REQUEST, "missing id".to_string()),
                    Some(id) => {
                        let mut map = responses.lock().await;
                        match map.remove(id) {
                            Some((resp, _ts)) => (StatusCode::OK, resp),
                            None => (StatusCode::NOT_FOUND, "pending".to_string()),
                        }
                    }
                }
            }

            let app = Router::new()
                .route("/", post(post_message))
                .route("/", get(get_response))
                .with_state((sender, responses));

            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(LuminaError::Io)?;

            let handle = tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, app).await {
                    eprintln!("http-channel: server error: {}", e);
                }
            });
            self.server_handle = Some(handle);

            Ok(())
        }

        async fn send_response(
            &self,
            response: &str,
            context: &MessageContext,
        ) -> Result<()> {
            if let Some(id) = &context.request_id {
                let mut map = self.responses.lock().await;
                let now = std::time::Instant::now();
                let ttl = std::time::Duration::from_secs(RESPONSE_TTL_SECS);

                // Evict stale (TTL-expired) entries first.
                if map.len() >= RESPONSE_CAP {
                    map.retain(|_, (_, ts)| now.duration_since(*ts) < ttl);
                }

                // If still at or over cap, hard-evict the oldest entry regardless
                // of TTL to prevent unbounded growth under burst traffic.
                if map.len() >= RESPONSE_CAP {
                    if let Some(oldest_key) = map
                        .iter()
                        .min_by_key(|(_, (_, ts))| *ts)
                        .map(|(k, _)| k.clone())
                    {
                        map.remove(&oldest_key);
                    }
                }

                map.insert(id.clone(), (response.to_string(), now));
            }
            Ok(())
        }

        async fn stop(&mut self) -> Result<()> {
            if let Some(handle) = self.server_handle.take() {
                handle.abort();
            }
            Ok(())
        }
    }

    /// Generate a cryptographically random request ID using the `rand` crate.
    ///
    /// The result is formatted as a UUID-shaped hex string but does **not** set
    /// RFC 4122 version/variant bits — callers should treat it as an opaque ID,
    /// not a conformant UUID v4.
    fn random_request_id() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        // Format as a hyphenated hex string (UUID v4 style).
        format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            u32::from_be_bytes(bytes[0..4].try_into().unwrap()),
            u16::from_be_bytes(bytes[4..6].try_into().unwrap()),
            u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
            u16::from_be_bytes(bytes[8..10].try_into().unwrap()),
            {
                let mut arr = [0u8; 8];
                arr[2..8].copy_from_slice(&bytes[10..16]);
                u64::from_be_bytes(arr)
            },
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::SocketAddr;
        use tokio::sync::mpsc;

        #[test]
        fn http_channel_name_is_http() {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let ch = HttpChannel::new(addr);
            assert_eq!(ch.name(), "http");
        }

        #[tokio::test]
        async fn http_channel_stop_returns_ok() {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let mut ch = HttpChannel::new(addr);
            assert!(ch.stop().await.is_ok());
        }

        #[tokio::test]
        async fn http_channel_send_response_stores_by_request_id() {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let ch = HttpChannel::new(addr);
            let ctx = MessageContext {
                request_id: Some("req-001".to_string()),
                ..Default::default()
            };
            ch.send_response("pong", &ctx).await.unwrap();
            let stored = ch.responses.lock().await;
            assert_eq!(
                stored.get("req-001").map(|(s, _)| s.as_str()),
                Some("pong")
            );
        }

        #[tokio::test]
        async fn http_channel_send_response_no_request_id_does_nothing() {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let ch = HttpChannel::new(addr);
            let ctx = MessageContext::default();
            ch.send_response("ignored", &ctx).await.unwrap();
            assert!(ch.responses.lock().await.is_empty());
        }

        #[tokio::test]
        async fn http_channel_stop_aborts_server_handle() {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let mut ch = HttpChannel::new(addr);
            // stop() on a channel that was never started should be a no-op.
            assert!(ch.stop().await.is_ok());
        }

        #[tokio::test]
        async fn http_channel_start_binds_successfully() {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let (tx, _rx) = mpsc::channel(4);
            let mut ch = HttpChannel::new(addr);
            // Should bind on an ephemeral port without error.
            assert!(ch.start(tx).await.is_ok());
        }
    }
}
