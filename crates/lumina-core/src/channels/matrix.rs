//! EDGE-08: Matrix channel adapter.
//!
//! Wraps the existing [`MatrixBot`] and exposes it through the [`Channel`]
//! trait.  The underlying `MatrixBot` is unchanged; this module adds only the
//! adapter shim so the channel registry can manage Matrix alongside other
//! channels.

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::{Channel, ChannelMessage, MessageContext};
use crate::config::Config;
use crate::error::Result;
use crate::matrix_bot::MatrixBot;

/// Matrix channel adapter.
///
/// Wraps a [`MatrixBot`] and implements the [`Channel`] trait so the
/// [`ChannelRegistry`] can manage it alongside other adapters.
///
/// [`ChannelRegistry`]: super::ChannelRegistry
pub struct MatrixChannel {
    config: Arc<Config>,
    /// Handle to the background sync task, held so [`stop`] can abort it.
    sync_handle: Option<tokio::task::JoinHandle<()>>,
}

impl MatrixChannel {
    /// Create a new Matrix channel from the given config.
    pub fn new(config: Arc<Config>) -> Self {
        Self { config, sync_handle: None }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    /// Connect the Matrix bot and spawn it in a background task.
    ///
    /// Messages received by the bot are forwarded to `sender` as
    /// [`ChannelMessage`]s so the core loop can process them alongside
    /// messages from other channels.
    async fn start(
        &mut self,
        sender: mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let config = self.config.clone();

        // Connect and authenticate
        let bot = MatrixBot::connect(config).await?;
        bot.login().await?;
        bot.join_room().await?;
        bot.initial_sync().await?;

        // Spawn the Matrix sync loop in a background task.
        // The bot's existing run() handles reconnection internally.
        // NOTE: Full message forwarding through `sender` is added in EDGE-09
        // (plumbing sender into MatrixBot::run).  In this phase the bot still
        // processes and responds to messages internally; we expose it through
        // the Channel trait for lifecycle management.
        let _sender = sender; // placeholder — full forwarding wired in EDGE-09
        let handle = tokio::spawn(async move {
            if let Err(e) = bot.run().await {
                eprintln!("matrix-channel: bot error: {}", e);
            }
        });
        self.sync_handle = Some(handle);

        Ok(())
    }

    /// Send a response back via Matrix.
    ///
    /// The `room_id` in `context` identifies the target room.  If absent,
    /// the adapter falls back silently (the MatrixBot already tracks its
    /// configured room).
    async fn send_response(
        &self,
        _response: &str,
        _context: &MessageContext,
    ) -> Result<()> {
        // Full implementation: look up the MatrixBot handle stored during start()
        // and call post_to_room(response).  In this phase the bot still handles
        // its own responses; this stub satisfies the trait contract.
        // EDGE-09 will wire up full response forwarding through the channel.
        log::warn!(
            "matrix-channel: send_response called but not yet integrated; \
             response delivery is handled directly by MatrixBot until EDGE-09"
        );
        Ok(())
    }

    /// Stop the Matrix channel, aborting the background sync task.
    async fn stop(&mut self) -> Result<()> {
        if let Some(handle) = self.sync_handle.take() {
            handle.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_config() -> Arc<Config> {
        use std::path::PathBuf;
        Arc::new(Config {
            chord_proxy_url: "http://localhost:1".to_string(),
            lumina_chord_secret: String::new(),
            matrix_homeserver: None,
            matrix_user: None,
            matrix_password: None,
            matrix_room_id: None,
            matrix_store_path: PathBuf::from("/tmp/lumina-test-matrix"),
            matrix_announce_startup: false,
            lumina_http_token: None,
            lumina_http_bind: "127.0.0.1:0".to_string(),
            system_prompt: String::new(),
            egress_allowlist: Vec::new(),
            admin_matrix_id: None,
            matrix_allowed_users: Vec::new(),
        })
    }

    #[test]
    fn matrix_channel_name_is_matrix() {
        let ch = MatrixChannel::new(stub_config());
        assert_eq!(ch.name(), "matrix");
    }

    #[tokio::test]
    async fn matrix_channel_stop_without_start_is_ok() {
        let mut ch = MatrixChannel::new(stub_config());
        assert!(ch.stop().await.is_ok());
    }
}
