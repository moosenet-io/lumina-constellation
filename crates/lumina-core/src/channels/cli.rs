//! EDGE-08: CLI channel adapter.
//!
//! Reads lines from stdin and forwards them as [`ChannelMessage`]s.
//! Responses are written to stdout via `println!`.
//!
//! This adapter is always compiled in; it is registered at runtime only
//! when running in stdin mode.

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::{Channel, ChannelMessage, MessageContext};
use crate::error::Result;

/// Channel adapter that reads from stdin and writes to stdout.
pub struct CliChannel {
    name: String,
    /// Handle to the background stdin-reader task, held for cancellation.
    reader_handle: Option<tokio::task::JoinHandle<()>>,
}

impl CliChannel {
    /// Create a new CLI channel.
    pub fn new() -> Self {
        Self { name: "cli".to_string(), reader_handle: None }
    }
}

impl Default for CliChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Channel for CliChannel {
    fn name(&self) -> &str {
        &self.name
    }

    /// Spawn a background task that reads lines from stdin and sends them to
    /// `sender`.  The task terminates when stdin is closed (EOF).
    ///
    /// The `JoinHandle` is stored so [`stop`] can abort the task.
    async fn start(
        &mut self,
        sender: mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let handle = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let stdin = tokio::io::stdin();
            let mut lines = BufReader::new(stdin).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let msg = ChannelMessage {
                    source_channel: "cli".to_string(),
                    sender_id: "local".to_string(),
                    content: line,
                    context: MessageContext::default(),
                    timestamp: std::time::SystemTime::now(),
                };

                if sender.send(msg).await.is_err() {
                    // Receiver dropped — stop the task.
                    break;
                }
            }
        });
        self.reader_handle = Some(handle);
        Ok(())
    }

    /// Write the response to stdout.
    async fn send_response(
        &self,
        response: &str,
        _context: &MessageContext,
    ) -> Result<()> {
        println!("{}", response);
        Ok(())
    }

    /// Stop the CLI channel — abort the background stdin reader task.
    async fn stop(&mut self) -> Result<()> {
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn cli_channel_name_is_cli() {
        let ch = CliChannel::new();
        assert_eq!(ch.name(), "cli");
    }

    #[test]
    fn cli_channel_default_name_is_cli() {
        let ch = CliChannel::default();
        assert_eq!(ch.name(), "cli");
    }

    #[tokio::test]
    async fn cli_channel_start_returns_ok() {
        let (tx, _rx) = mpsc::channel(4);
        let mut ch = CliChannel::new();
        // start() spawns a background task and returns Ok immediately.
        let result = ch.start(tx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn cli_channel_stop_returns_ok() {
        let mut ch = CliChannel::new();
        assert!(ch.stop().await.is_ok());
    }

    #[tokio::test]
    async fn cli_channel_send_response_returns_ok() {
        let ch = CliChannel::new();
        let ctx = MessageContext::default();
        let result = ch.send_response("hello", &ctx).await;
        assert!(result.is_ok());
    }

    #[test]
    fn channel_message_from_cli_has_correct_source() {
        // Verify that manually constructing a ChannelMessage with cli source works.
        let msg = ChannelMessage {
            source_channel: "cli".to_string(),
            sender_id: "local".to_string(),
            content: "ping".to_string(),
            context: MessageContext::default(),
            timestamp: std::time::SystemTime::now(),
        };
        assert_eq!(msg.source_channel, "cli");
        assert_eq!(msg.sender_id, "local");
        assert_eq!(msg.content, "ping");
    }
}
