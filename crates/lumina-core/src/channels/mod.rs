//! EDGE-08: Channel adapter trait and registry.
//!
//! Defines the [`Channel`] trait that every messaging platform implements,
//! and a [`ChannelRegistry`] that manages their lifecycle (start / stop).
//!
//! All channels feed into a shared `mpsc::Sender<ChannelMessage>` so the core
//! agent loop processes every message through the same guarded pipeline
//! regardless of origin.

use async_trait::async_trait;
use tokio::sync::mpsc;

pub mod matrix_commands;
pub mod cli;
pub mod matrix;

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "telegram")]
pub mod telegram;

/// A message received from any channel.
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    /// Name of the originating channel, e.g. `"matrix"`, `"cli"`, `"http"`.
    pub source_channel: String,
    /// Platform-specific user identifier (Matrix user ID, HTTP client IP, …).
    pub sender_id: String,
    /// The text content of the message.
    pub content: String,
    /// Channel-specific routing context needed to deliver a response.
    pub context: MessageContext,
    /// Wall-clock time when the message arrived.
    pub timestamp: std::time::SystemTime,
}

/// Channel-specific metadata attached to every [`ChannelMessage`].
///
/// Fields that are irrelevant to a given channel are left as `None`.
#[derive(Debug, Clone, Default)]
pub struct MessageContext {
    /// Matrix room ID (`!room:homeserver`).
    pub room_id: Option<String>,
    /// HTTP request ID (for correlating async responses).
    pub request_id: Option<String>,
    /// Thread / reply ID (platform-specific).
    pub thread_id: Option<String>,
}

/// A bidirectional channel adapter.
///
/// Implementors bridge a specific messaging platform (Matrix, CLI, HTTP, …) to
/// the Lumina core loop.  All trait methods are async; the `Send + Sync` bounds
/// allow instances to live inside `Arc<Mutex<…>>` or be sent across tokio tasks.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Short, stable identifier for this channel, e.g. `"cli"`, `"matrix"`.
    fn name(&self) -> &str;

    /// Start receiving messages from this channel.
    ///
    /// Implementations should spawn a background task that reads from the
    /// underlying transport and forwards each message to `sender`.  The method
    /// returns once the background task is running; it does **not** block until
    /// the channel closes.
    async fn start(
        &mut self,
        sender: mpsc::Sender<ChannelMessage>,
    ) -> crate::error::Result<()>;

    /// Deliver a response back to the originating user/room.
    ///
    /// `context` carries the routing information recorded when the original
    /// message arrived (e.g. the Matrix room_id).
    async fn send_response(
        &self,
        response: &str,
        context: &MessageContext,
    ) -> crate::error::Result<()>;

    /// Stop the channel and free any resources.
    async fn stop(&mut self) -> crate::error::Result<()>;
}

/// Manages the lifecycle of all registered [`Channel`] adapters.
///
/// # Example
/// ```no_run
/// # use lumina_core::channels::{ChannelRegistry, cli::CliChannel};
/// # #[tokio::main]
/// # async fn main() {
/// let (tx, mut rx) = tokio::sync::mpsc::channel(32);
/// let mut registry = ChannelRegistry::new();
/// registry.register(Box::new(CliChannel::new()));
/// registry.start_all(tx).await.unwrap();
/// # }
/// ```
pub struct ChannelRegistry {
    channels: Vec<Box<dyn Channel>>,
}

impl ChannelRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { channels: Vec::new() }
    }

    /// Add a channel to the registry.
    ///
    /// Channels are started in registration order when [`start_all`] is called.
    pub fn register(&mut self, channel: Box<dyn Channel>) {
        self.channels.push(channel);
    }

    /// Start all registered channels.
    ///
    /// Each channel receives a clone of `sender`.  If a channel fails to start,
    /// the error is logged and the remaining channels are still attempted.
    ///
    /// Returns an error only when **all** channels fail to start.
    pub async fn start_all(
        &mut self,
        sender: mpsc::Sender<ChannelMessage>,
    ) -> crate::error::Result<()> {
        let mut started = 0usize;

        for channel in &mut self.channels {
            match channel.start(sender.clone()).await {
                Ok(()) => {
                    eprintln!("channels: started '{}'", channel.name());
                    started += 1;
                }
                Err(e) => {
                    eprintln!("channels: '{}' failed to start: {}", channel.name(), e);
                }
            }
        }

        if started == 0 && !self.channels.is_empty() {
            return Err(crate::error::LuminaError::Config(
                "No channels available. Check your configuration.".to_string(),
            ));
        }

        Ok(())
    }

    /// Stop all registered channels, logging errors but not propagating them.
    pub async fn stop_all(&mut self) {
        for channel in &mut self.channels {
            if let Err(e) = channel.stop().await {
                eprintln!("channels: error stopping '{}': {}", channel.name(), e);
            }
        }
    }

    /// Return the names of all registered channels.
    pub fn channel_names(&self) -> Vec<&str> {
        self.channels.iter().map(|c| c.name()).collect()
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    /// A minimal no-op channel used only in tests.
    struct MockChannel {
        name: String,
        /// Counts how many times `start` was called.
        starts: Arc<Mutex<usize>>,
        /// If true, `start` returns an error.
        fail_start: bool,
    }

    impl MockChannel {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                starts: Arc::new(Mutex::new(0)),
                fail_start: false,
            }
        }

        fn failing(name: &str) -> Self {
            Self {
                name: name.to_string(),
                starts: Arc::new(Mutex::new(0)),
                fail_start: true,
            }
        }

        fn start_count(&self) -> Arc<Mutex<usize>> {
            self.starts.clone()
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            &self.name
        }

        async fn start(
            &mut self,
            _sender: mpsc::Sender<ChannelMessage>,
        ) -> crate::error::Result<()> {
            *self.starts.lock().await += 1;
            if self.fail_start {
                Err(crate::error::LuminaError::Config(
                    format!("{} start failed", self.name),
                ))
            } else {
                Ok(())
            }
        }

        async fn send_response(
            &self,
            _response: &str,
            _context: &MessageContext,
        ) -> crate::error::Result<()> {
            Ok(())
        }

        async fn stop(&mut self) -> crate::error::Result<()> {
            Ok(())
        }
    }

    // ── ChannelMessage construction ──────────────────────────────────────

    #[test]
    fn channel_message_fields_set_correctly() {
        let msg = ChannelMessage {
            source_channel: "cli".to_string(),
            sender_id: "user_42".to_string(),
            content: "hello".to_string(),
            context: MessageContext {
                room_id: Some("!room:example.org".to_string()),
                request_id: None,
                thread_id: None,
            },
            timestamp: std::time::SystemTime::now(),
        };
        assert_eq!(msg.source_channel, "cli");
        assert_eq!(msg.sender_id, "user_42");
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.context.room_id.as_deref(), Some("!room:example.org"));
    }

    #[test]
    fn message_context_default_is_all_none() {
        let ctx = MessageContext::default();
        assert!(ctx.room_id.is_none());
        assert!(ctx.request_id.is_none());
        assert!(ctx.thread_id.is_none());
    }

    // ── ChannelRegistry ──────────────────────────────────────────────────

    #[test]
    fn registry_channel_names_empty() {
        let registry = ChannelRegistry::new();
        assert!(registry.channel_names().is_empty());
    }

    #[test]
    fn registry_channel_names_after_register() {
        let mut registry = ChannelRegistry::new();
        registry.register(Box::new(MockChannel::new("alpha")));
        registry.register(Box::new(MockChannel::new("beta")));
        let names = registry.channel_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[tokio::test]
    async fn registry_start_all_calls_start_on_each_channel() {
        let (tx, _rx) = mpsc::channel(8);
        let ch1 = MockChannel::new("ch1");
        let ch2 = MockChannel::new("ch2");
        let count1 = ch1.start_count();
        let count2 = ch2.start_count();

        let mut registry = ChannelRegistry::new();
        registry.register(Box::new(ch1));
        registry.register(Box::new(ch2));

        registry.start_all(tx).await.expect("start_all should succeed");

        assert_eq!(*count1.lock().await, 1);
        assert_eq!(*count2.lock().await, 1);
    }

    #[tokio::test]
    async fn registry_start_all_continues_after_single_failure() {
        let (tx, _rx) = mpsc::channel(8);
        let bad = MockChannel::failing("bad");
        let good = MockChannel::new("good");
        let good_count = good.start_count();

        let mut registry = ChannelRegistry::new();
        registry.register(Box::new(bad));
        registry.register(Box::new(good));

        // Should succeed because at least one channel started.
        registry.start_all(tx).await.expect("should succeed with one good channel");
        assert_eq!(*good_count.lock().await, 1);
    }

    #[tokio::test]
    async fn registry_start_all_returns_error_when_all_fail() {
        let (tx, _rx) = mpsc::channel(8);
        let mut registry = ChannelRegistry::new();
        registry.register(Box::new(MockChannel::failing("bad1")));
        registry.register(Box::new(MockChannel::failing("bad2")));

        let result = registry.start_all(tx).await;
        assert!(result.is_err(), "should error when all channels fail");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No channels available"));
    }

    #[tokio::test]
    async fn registry_stop_all_does_not_panic() {
        let (tx, _rx) = mpsc::channel(8);
        let mut registry = ChannelRegistry::new();
        registry.register(Box::new(MockChannel::new("c1")));
        registry.start_all(tx).await.unwrap();
        registry.stop_all().await; // must not panic
    }

    #[tokio::test]
    async fn multiple_channels_can_be_registered_simultaneously() {
        let (tx, mut rx) = mpsc::channel(32);
        let mut registry = ChannelRegistry::new();
        // Register three channels at once
        for name in &["alpha", "beta", "gamma"] {
            registry.register(Box::new(MockChannel::new(name)));
        }
        assert_eq!(registry.channel_names().len(), 3);
        registry.start_all(tx).await.unwrap();
        // rx should not have any messages yet (MockChannel doesn't send on start)
        assert!(rx.try_recv().is_err());
    }
}
