//! GUARD-05: Rate limiting and request throttling
//!
//! Provides protection against abuse and DoS attacks by limiting
//! the rate of requests from specific sources.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Configuration for rate limiting
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per window
    pub max_requests: u32,
    /// Time window for rate limiting
    pub window_duration: Duration,
    /// Whether to block or just log violations
    pub enforce: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window_duration: Duration::from_secs(60), // 1 minute
            enforce: true,
        }
    }
}

impl RateLimitConfig {
    /// Create permissive config for development
    pub fn permissive() -> Self {
        Self {
            max_requests: 1000,
            window_duration: Duration::from_secs(60),
            enforce: false, // Just log violations
        }
    }

    /// Create strict config for production
    pub fn strict() -> Self {
        Self {
            max_requests: 50,
            window_duration: Duration::from_secs(60),
            enforce: true,
        }
    }

    /// Create burst config for handling temporary spikes
    pub fn burst() -> Self {
        Self {
            max_requests: 500,
            window_duration: Duration::from_secs(10), // Shorter window
            enforce: true,
        }
    }
}

/// Rate limiter implementation using token bucket algorithm
pub struct RateLimiter {
    config: RateLimitConfig,
    buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
}

/// Token bucket for a specific client/source
#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    capacity: f64,
    refill_rate: f64, // tokens per second
}

impl TokenBucket {
    fn new(capacity: u32, window_duration: Duration) -> Self {
        let refill_rate = capacity as f64 / window_duration.as_secs_f64();
        Self {
            tokens: capacity as f64,
            last_refill: Instant::now(),
            capacity: capacity as f64,
            refill_rate,
        }
    }

    /// Try to consume tokens, returning true if allowed
    fn try_consume(&mut self, tokens: f64) -> bool {
        self.refill();

        if self.tokens >= tokens {
            self.tokens -= tokens;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();

        let new_tokens = elapsed * self.refill_rate;
        self.tokens = (self.tokens + new_tokens).min(self.capacity);
        self.last_refill = now;
    }

    /// Get current token count (after refill)
    fn current_tokens(&mut self) -> f64 {
        self.refill();
        self.tokens
    }

    /// Check if bucket would allow a request without consuming tokens
    fn can_consume(&mut self, tokens: f64) -> bool {
        self.refill();
        self.tokens >= tokens
    }
}

impl RateLimiter {
    /// Create new rate limiter with default config
    pub fn new() -> Self {
        Self::with_config(RateLimitConfig::default())
    }

    /// Create rate limiter with custom config
    pub fn with_config(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if a request should be allowed
    pub fn check_request(&self, client_id: &str) -> RateLimitResult {
        self.check_request_with_cost(client_id, 1.0)
    }

    /// Check request with custom token cost
    pub fn check_request_with_cost(&self, client_id: &str, cost: f64) -> RateLimitResult {
        let mut buckets = self.buckets.lock().unwrap();

        let bucket = buckets
            .entry(client_id.to_string())
            .or_insert_with(|| TokenBucket::new(
                self.config.max_requests,
                self.config.window_duration,
            ));

        let allowed = bucket.try_consume(cost);
        let remaining = bucket.current_tokens() as u32;

        if allowed || !self.config.enforce {
            RateLimitResult {
                allowed: true,
                remaining,
                reset_time: bucket.last_refill + self.config.window_duration,
                retry_after: None,
            }
        } else {
            // Calculate retry after duration
            let needed_tokens = cost - bucket.tokens;
            let wait_time = Duration::from_secs_f64(needed_tokens / bucket.refill_rate);

            RateLimitResult {
                allowed: false,
                remaining,
                reset_time: bucket.last_refill + self.config.window_duration,
                retry_after: Some(wait_time),
            }
        }
    }

    /// Get current status for a client without consuming tokens
    pub fn get_status(&self, client_id: &str) -> RateLimitResult {
        let mut buckets = self.buckets.lock().unwrap();

        let bucket = buckets
            .entry(client_id.to_string())
            .or_insert_with(|| TokenBucket::new(
                self.config.max_requests,
                self.config.window_duration,
            ));

        let can_request = bucket.can_consume(1.0);
        let remaining = bucket.current_tokens() as u32;

        RateLimitResult {
            allowed: can_request,
            remaining,
            reset_time: bucket.last_refill + self.config.window_duration,
            retry_after: if can_request { None } else { Some(Duration::from_secs(1)) },
        }
    }

    /// Clear all rate limit data (useful for testing)
    pub fn clear(&self) {
        let mut buckets = self.buckets.lock().unwrap();
        buckets.clear();
    }

    /// Remove expired buckets to prevent memory leaks
    pub fn cleanup_expired(&self) {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();

        buckets.retain(|_, bucket| {
            // Keep buckets that were active recently
            now.duration_since(bucket.last_refill) < self.config.window_duration * 2
        });
    }

    /// Get current configuration
    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }

    /// Update configuration
    pub fn update_config(&mut self, config: RateLimitConfig) {
        self.config = config;
        // Clear existing buckets since they were configured for old limits
        self.clear();
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a rate limit check
#[derive(Debug, Clone)]
pub struct RateLimitResult {
    /// Whether the request should be allowed
    pub allowed: bool,
    /// Number of remaining requests in current window
    pub remaining: u32,
    /// When the rate limit window resets
    pub reset_time: Instant,
    /// How long to wait before retrying (if blocked)
    pub retry_after: Option<Duration>,
}

impl RateLimitResult {
    /// Convert to HTTP-style headers
    pub fn to_headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("X-RateLimit-Remaining", self.remaining.to_string()),
            ("X-RateLimit-Reset", self.reset_time.elapsed().as_secs().to_string()),
        ];

        if let Some(retry_after) = self.retry_after {
            headers.push(("Retry-After", retry_after.as_secs().to_string()));
        }

        headers
    }
}

/// Global rate limiter instances for different use cases
static GLOBAL_RATE_LIMITER: OnceLock<RateLimiter> = OnceLock::new();
static BURST_RATE_LIMITER: OnceLock<RateLimiter> = OnceLock::new();

/// Get or initialize global rate limiter
pub fn global_rate_limiter() -> &'static RateLimiter {
    GLOBAL_RATE_LIMITER.get_or_init(|| RateLimiter::new())
}

/// Get or initialize burst rate limiter for handling spikes
pub fn burst_rate_limiter() -> &'static RateLimiter {
    BURST_RATE_LIMITER.get_or_init(|| RateLimiter::with_config(RateLimitConfig::burst()))
}

/// Check if request should be allowed using global rate limiter
pub fn check_global_rate_limit(client_id: &str) -> RateLimitResult {
    global_rate_limiter().check_request(client_id)
}

/// Check if burst request should be allowed
pub fn check_burst_rate_limit(client_id: &str) -> RateLimitResult {
    burst_rate_limiter().check_request(client_id)
}

/// Middleware-style rate limiting function
pub fn with_rate_limit<F, R>(client_id: &str, operation: F) -> Result<R, RateLimitError>
where
    F: FnOnce() -> R,
{
    let result = check_global_rate_limit(client_id);

    if result.allowed {
        Ok(operation())
    } else {
        Err(RateLimitError {
            remaining: result.remaining,
            retry_after: result.retry_after,
        })
    }
}

/// Rate limiting error
#[derive(Debug, Clone)]
pub struct RateLimitError {
    pub remaining: u32,
    pub retry_after: Option<Duration>,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(retry_after) = self.retry_after {
            write!(
                f,
                "Rate limit exceeded. {} requests remaining. Retry after {} seconds.",
                self.remaining,
                retry_after.as_secs()
            )
        } else {
            write!(f, "Rate limit exceeded. {} requests remaining.", self.remaining)
        }
    }
}

impl std::error::Error for RateLimitError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_rate_limiter_creation() {
        let limiter = RateLimiter::new();
        assert_eq!(limiter.config().max_requests, 100);
        assert_eq!(limiter.config().window_duration, Duration::from_secs(60));
    }

    #[test]
    fn test_custom_config() {
        let config = RateLimitConfig {
            max_requests: 10,
            window_duration: Duration::from_secs(30),
            enforce: true,
        };
        let limiter = RateLimiter::with_config(config);
        assert_eq!(limiter.config().max_requests, 10);
    }

    #[test]
    fn test_single_request_allowed() {
        let limiter = RateLimiter::new();
        let result = limiter.check_request("client1");
        assert!(result.allowed);
        assert_eq!(result.remaining, 99); // Should have consumed 1 token
    }

    #[test]
    fn test_multiple_requests() {
        let limiter = RateLimiter::new();

        // Make several requests
        for i in 1..=5 {
            let result = limiter.check_request("client1");
            assert!(result.allowed);
            assert_eq!(result.remaining, 100 - i); // Each request consumes 1 token
        }
    }

    #[test]
    fn test_rate_limit_exceeded() {
        let config = RateLimitConfig {
            max_requests: 2,
            window_duration: Duration::from_secs(60),
            enforce: true,
        };
        let limiter = RateLimiter::with_config(config);

        // First two requests should be allowed
        assert!(limiter.check_request("client1").allowed);
        assert!(limiter.check_request("client1").allowed);

        // Third request should be blocked
        let result = limiter.check_request("client1");
        assert!(!result.allowed);
        assert!(result.retry_after.is_some());
    }

    #[test]
    fn test_different_clients_separate_limits() {
        let config = RateLimitConfig {
            max_requests: 2,
            window_duration: Duration::from_secs(60),
            enforce: true,
        };
        let limiter = RateLimiter::with_config(config);

        // Client 1 exhausts their limit
        assert!(limiter.check_request("client1").allowed);
        assert!(limiter.check_request("client1").allowed);
        assert!(!limiter.check_request("client1").allowed);

        // Client 2 should still be able to make requests
        assert!(limiter.check_request("client2").allowed);
        assert!(limiter.check_request("client2").allowed);
    }

    #[test]
    fn test_token_refill() {
        let config = RateLimitConfig {
            max_requests: 10,
            window_duration: Duration::from_millis(500), // 500ms window for testing
            enforce: true,
        };
        let limiter = RateLimiter::with_config(config);

        // Exhaust the limit
        for _ in 0..10 {
            assert!(limiter.check_request("client1").allowed);
        }
        // Now should be exhausted
        assert!(!limiter.check_request("client1").allowed);

        // Wait for full refill period
        thread::sleep(Duration::from_millis(600)); // Wait longer than window

        // Should be able to make requests again
        let _result = limiter.check_request("client1");
        assert!(_result.allowed, "Should be allowed after refill period");

        // Verify we have tokens available
        let status = limiter.get_status("client1");
        assert!(status.remaining > 0, "Should have tokens after refill");
    }

    #[test]
    fn test_request_cost() {
        let config = RateLimitConfig {
            max_requests: 10,
            window_duration: Duration::from_secs(60),
            enforce: true,
        };
        let limiter = RateLimiter::with_config(config);

        // Make a request with cost 5
        let result = limiter.check_request_with_cost("client1", 5.0);
        assert!(result.allowed);
        assert_eq!(result.remaining, 5); // 10 - 5 = 5

        // Make another request with cost 3
        let result = limiter.check_request_with_cost("client1", 3.0);
        assert!(result.allowed);
        assert_eq!(result.remaining, 2); // 5 - 3 = 2

        // Request cost 5 should be blocked (only 2 tokens left)
        let result = limiter.check_request_with_cost("client1", 5.0);
        assert!(!result.allowed);
    }

    #[test]
    fn test_permissive_mode() {
        let config = RateLimitConfig {
            max_requests: 1,
            window_duration: Duration::from_secs(60),
            enforce: false, // Permissive mode
        };
        let limiter = RateLimiter::with_config(config);

        // Exhaust the limit
        assert!(limiter.check_request("client1").allowed);

        // Should still be allowed in permissive mode
        assert!(limiter.check_request("client1").allowed);
        assert!(limiter.check_request("client1").allowed);
    }

    #[test]
    fn test_get_status() {
        let limiter = RateLimiter::new();

        // Check initial status
        let status = limiter.get_status("client1");
        assert!(status.allowed);
        assert_eq!(status.remaining, 100);

        // Make a request and check status
        limiter.check_request("client1");
        let status = limiter.get_status("client1");
        assert_eq!(status.remaining, 99);
    }

    #[test]
    fn test_cleanup_expired() {
        let limiter = RateLimiter::new();

        // Make requests from multiple clients
        limiter.check_request("client1");
        limiter.check_request("client2");
        limiter.check_request("client3");

        // Verify buckets exist
        {
            let buckets = limiter.buckets.lock().unwrap();
            assert_eq!(buckets.len(), 3);
        }

        // Cleanup shouldn't remove active buckets
        limiter.cleanup_expired();
        {
            let buckets = limiter.buckets.lock().unwrap();
            assert_eq!(buckets.len(), 3);
        }
    }

    #[test]
    fn test_global_rate_limiter() {
        let result = check_global_rate_limit("global_client");
        assert!(result.allowed);
    }

    #[test]
    fn test_burst_rate_limiter() {
        let result = check_burst_rate_limit("burst_client");
        assert!(result.allowed);
    }

    #[test]
    fn test_with_rate_limit_middleware() {
        // Test the middleware pattern with global limiter
        let result = with_rate_limit("middleware_client", || "operation_result");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "operation_result");
    }

    #[test]
    fn test_rate_limit_headers() {
        let limiter = RateLimiter::new();
        let result = limiter.check_request("headers_client");

        let headers = result.to_headers();
        assert!(!headers.is_empty());

        // Check header format
        let remaining_header = headers.iter().find(|(k, _)| *k == "X-RateLimit-Remaining");
        assert!(remaining_header.is_some());
    }

    #[test]
    fn test_config_presets() {
        let permissive = RateLimitConfig::permissive();
        assert_eq!(permissive.max_requests, 1000);
        assert!(!permissive.enforce);

        let strict = RateLimitConfig::strict();
        assert_eq!(strict.max_requests, 50);
        assert!(strict.enforce);

        let burst = RateLimitConfig::burst();
        assert_eq!(burst.max_requests, 500);
        assert_eq!(burst.window_duration, Duration::from_secs(10));
    }
}