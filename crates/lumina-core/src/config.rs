//! Configuration for lumina-core

use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use crate::vault;
use secrecy::ExposeSecret;
use std::env;
use std::path::PathBuf;

/// Read a secret by name: vault first, environment variable as fallback.
/// Non-secret config (paths, flags, bind addresses) should use `env::var` directly.
fn secret_or(key: &str) -> Option<String> {
    if let Some(mgr) = vault::manager_opt() {
        if let Some(s) = mgr.get(key) {
            return Some(s.expose_secret().to_string());
        }
    }
    env::var(key).ok()
}

/// CONV-03: pure parse of the `LUMINA_CONV_BUFFER_ENABLED` flag. Defaults to
/// `true`; only the literal `"false"` or `"0"` disable the buffer. Split out as a
/// free function so it can be unit-tested without mutating process-global env
/// (which races with other tests under parallel `cargo test`).
pub(crate) fn parse_conv_buffer_enabled(v: Option<&str>) -> bool {
    !matches!(v, Some("false") | Some("0"))
}

const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Lumina, a personal AI assistant created for and owned by the operator. \
You run entirely on the operator's local home infrastructure — you never run in the cloud. \
You are helpful, direct, and conversational with a warm personality. \
You never refer to yourself as ChatGPT, GPT, OpenAI, or any other AI assistant. \
Your name is Lumina. You were built in Rust and you reason through a local language \
model on the operator's own hardware.";

/// Load system prompt: LUMINA_SYSTEM_PROMPT (vault/env) → ~/.lumina/system-prompt.txt → default.
fn load_system_prompt() -> String {
    if let Some(prompt) = secret_or("LUMINA_SYSTEM_PROMPT") {
        if !prompt.trim().is_empty() {
            return prompt;
        }
    }
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".lumina").join("system-prompt.txt");
        if let Ok(content) = std::fs::read_to_string(&path) {
            let trimmed = content.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
    }
    DEFAULT_SYSTEM_PROMPT.to_string()
}

#[derive(Debug, Clone)]
pub struct Config {
    // Chord (LLM proxy)
    pub chord_proxy_url: String,
    pub lumina_chord_secret: String,

    // Matrix channel
    pub matrix_homeserver: Option<String>,
    pub matrix_user: Option<String>,
    pub matrix_password: Option<String>,
    pub matrix_room_id: Option<String>,
    pub matrix_store_path: PathBuf,
    pub matrix_announce_startup: bool,

    // HTTP server (MATRIX-05)
    pub lumina_http_token: Option<String>,
    pub lumina_http_bind: String,

    // Identity
    pub system_prompt: String,

    // EDGE-02: Egress inspection
    /// Parsed `LUMINA_EGRESS_ALLOWLIST` entries.
    ///
    /// Use [`Config::build_egress_inspector`] to construct an `EgressInspector`
    /// from this list. When empty, `build_egress_inspector` falls back to
    /// loopback-only (via `EgressInspector::from_env`).
    ///
    /// Operators MUST set `LUMINA_EGRESS_ALLOWLIST` to include the Chord proxy,
    /// MCP hub, Matrix homeserver, and any other hosts that tools need to reach.
    /// No infrastructure addresses are hardcoded here — all values come from
    /// the environment at runtime.
    ///
    /// Format: `hostname`, `IP`, or `*.subdomain` for single-level wildcard.
    /// Example: `LUMINA_EGRESS_ALLOWLIST=mcp.example.com,198.51.100.5,*.internal.example.com`
    pub egress_allowlist: Vec<String>,

    // P2-01: Multi-user
    /// Matrix user ID that is auto-promoted to Admin on first contact.
    ///
    /// Set `LUMINA_ADMIN_MATRIX_ID` to e.g. `@admin:your.homeserver.org`.
    /// When a message arrives from this Matrix user and no account exists yet,
    /// `UserStore::get_or_create_matrix_user` creates an `Admin` account.
    /// If the account already exists as a non-Admin, call
    /// `UserStore::apply_admin_matrix_promotion` at startup.
    pub admin_matrix_id: Option<String>,

    /// MATRIX-SECURITY: Allowlist of Matrix user IDs permitted to interact with the bot.
    ///
    /// Set `MATRIX_ALLOWED_USERS` to a comma-separated list of fully-qualified Matrix IDs,
    /// e.g. `"@operator:example.com,@alice:example.com"`.
    /// When non-empty, messages and invites from users NOT in this list are silently ignored.
    /// When empty (not configured), all users are accepted (backward-compatible default).
    pub matrix_allowed_users: Vec<String>,
}

/// Phase 1 vault secrets (never read via env — always via `vault::manager().get(key)`):
///   - `TERMINUS_SSH_KEY`  — private SSH key for the Terminus MCP hub (P1-10)
///   - `ENGRAM_DB_KEY`     — SQLCipher key for conversation + engram stores (P1-03, P1-06)
///
/// EDGE-09 Telegram secrets (vault-managed, referenced by env-var name):
///   - `TELEGRAM_BOT_TOKEN`    — Telegram Bot API token obtained from @BotFather.
///                               Read at runtime by `TelegramChannel` via the env-var
///                               name stored in `token_env_key`.  Never hardcode.
///   - `TELEGRAM_ALLOWED_USERS` — Comma-separated numeric Telegram user IDs permitted
///                               to interact with the bot (e.g. `"123456789,987654321"`).
///                               Non-numeric entries are skipped with a stderr warning.
impl Config {
    /// Return the admin Matrix ID as a `&str`, or `""` if not configured.
    ///
    /// Pass this value to `UserStore::get_or_create_matrix_user` and
    /// `UserStore::apply_admin_matrix_promotion`.
    pub fn admin_matrix_id(&self) -> &str {
        self.admin_matrix_id.as_deref().unwrap_or("")
    }

    /// Terminus MCP hub hostname or IP (non-secret, from `TERMINUS_HOST` env var).
    pub fn terminus_host(&self) -> String {
        env::var("TERMINUS_HOST").unwrap_or_else(|_| String::new())
    }

    /// Ollama embeddings endpoint URL (non-secret, from `OLLAMA_EMBEDDING_URL` env var).
    pub fn ollama_embedding_url(&self) -> String {
        env::var("OLLAMA_EMBEDDING_URL").unwrap_or_else(|_| String::new())
    }

    /// Embedding model name for Engram (from `ENGRAM_EMBED_MODEL` env var, default `mxbai-embed-large`).
    pub fn engram_embed_model(&self) -> String {
        env::var("ENGRAM_EMBED_MODEL").unwrap_or_else(|_| "mxbai-embed-large".to_string())
    }

    /// Rolling conversation window size in messages (from `CONVERSATION_WINDOW`, default 20).
    pub fn conversation_window(&self) -> usize {
        env::var("CONVERSATION_WINDOW")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20)
    }

    /// Session idle timeout in minutes (from `SESSION_IDLE_MINUTES`, default 30).
    pub fn session_idle_minutes(&self) -> u64 {
        env::var("SESSION_IDLE_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30)
    }

    /// CONV-02: max turn-pairs held in the in-memory conversation buffer
    /// (from `LUMINA_CONV_BUFFER_SIZE`, default tuned by CONV-01).
    pub fn conv_buffer_size(&self) -> usize {
        env::var("LUMINA_CONV_BUFFER_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::conversation::buffer::DEFAULT_BUFFER_SIZE)
    }

    /// CONV-02: max tokens held in the in-memory conversation buffer
    /// (from `LUMINA_CONV_TOKEN_BUDGET`, default tuned by CONV-01).
    pub fn conv_token_budget(&self) -> usize {
        env::var("LUMINA_CONV_TOKEN_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::conversation::buffer::DEFAULT_TOKEN_BUDGET)
    }

    /// CONV-02: conversation-buffer session inactivity timeout in seconds
    /// (from `LUMINA_SESSION_TIMEOUT_SECS`, default 1800 = 30 min).
    pub fn conv_session_timeout_secs(&self) -> i64 {
        env::var("LUMINA_SESSION_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::conversation::buffer::DEFAULT_SESSION_TIMEOUT_SECS)
    }

    /// CONV-05: whether progressive summarization compresses old turns instead of
    /// dropping them (from `LUMINA_CONV_SUMMARIZE_ENABLED`, default `true`). When
    /// `false`, the buffer uses pure FIFO eviction (CONV-02).
    pub fn conv_summarize_enabled(&self) -> bool {
        env::var("LUMINA_CONV_SUMMARIZE_ENABLED")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true)
    }

    /// CONV-05: fast local model used for summarization (from
    /// `LUMINA_CONV_SUMMARIZE_MODEL`, default `qwen3:8b`). MUST NOT be the
    /// personality model (gpt-oss:20b) — that would contend for VRAM.
    pub fn conv_summarize_model(&self) -> String {
        env::var("LUMINA_CONV_SUMMARIZE_MODEL").unwrap_or_else(|_| "qwen3:8b".to_string())
    }

    /// CONV-05: verbatim-turn count that triggers summarization of the oldest
    /// half (from `LUMINA_CONV_SUMMARIZE_THRESHOLD`, default 80% of the buffer
    /// size, floor 2).
    pub fn conv_summarize_threshold(&self) -> usize {
        env::var("LUMINA_CONV_SUMMARIZE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| (self.conv_buffer_size() * 8 / 10).max(2))
    }

    /// CONV-05: Ollama base URL for the summarizer. Prefers an explicit
    /// `LUMINA_CONV_SUMMARIZE_URL`, then the CPU-only endpoint (`OLLAMA_CPU_URL`,
    /// avoids any VRAM contention), then `OLLAMA_URL`, finally deriving the base
    /// from `OLLAMA_EMBEDDING_URL` (stripping a trailing `/api/...`). Empty if
    /// none are configured (summarization then no-ops → FIFO fallback).
    pub fn conv_summarize_url(&self) -> String {
        if let Ok(u) = env::var("LUMINA_CONV_SUMMARIZE_URL") {
            if !u.is_empty() { return u; }
        }
        if let Ok(u) = env::var("OLLAMA_CPU_URL") {
            if !u.is_empty() { return u; }
        }
        if let Ok(u) = env::var("OLLAMA_URL") {
            if !u.is_empty() { return u; }
        }
        // Derive base from the embedding URL, e.g. ".../api/embeddings" → base.
        if let Ok(u) = env::var("OLLAMA_EMBEDDING_URL") {
            if let Some(idx) = u.find("/api/") {
                return u[..idx].to_string();
            }
        }
        String::new()
    }

    /// CONV-03: whether the in-memory conversation buffer is wired into the Chord
    /// request path (from `LUMINA_CONV_BUFFER_ENABLED`, default `true`). When
    /// `false`, behaviour matches pre-CONV (no buffered multi-turn context),
    /// allowing quick rollback.
    pub fn conv_buffer_enabled(&self) -> bool {
        parse_conv_buffer_enabled(env::var("LUMINA_CONV_BUFFER_ENABLED").ok().as_deref())
    }

    /// Build an `EgressInspector` from this config's allowlist (EDGE-02).
    ///
    /// If `egress_allowlist` is empty (not configured), falls back to
    /// `EgressInspector::from_env()` which uses the loopback-only default.
    /// Callers that want deny-all behaviour should pass a non-empty list
    /// containing only the explicitly required hosts.
    pub fn build_egress_inspector(&self) -> EgressInspector {
        if self.egress_allowlist.is_empty() {
            EgressInspector::from_env()
        } else {
            EgressInspector::new(self.egress_allowlist.clone())
        }
    }

    /// Maximum MCP tool calls per turn (from `MCP_MAX_TOOL_CALLS`, default 3).
    pub fn mcp_max_tool_calls(&self) -> usize {
        env::var("MCP_MAX_TOOL_CALLS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3)
    }

    /// Whether to use Chord's agentic execution mode (from `CHORD_AGENTIC_MODE`, default `true`).
    ///
    /// When `true` (default), `process_turn_with_session` packages the full context and
    /// sends it to `CHORD_PROXY_URL/v1/agent/execute`, letting Chord run the tool-calling
    /// loop internally.  The loop includes all four security guards.
    ///
    /// Set `CHORD_AGENTIC_MODE=false` to fall back to the legacy client-side tool loop.
    pub fn chord_agentic_mode(&self) -> bool {
        env::var("CHORD_AGENTIC_MODE")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true)
    }

    pub fn from_env() -> Result<Self> {
        // CHORD-04: CHORD_PROXY_URL is the primary (and normally only) backend for
        // both chat completions and tool operations (list, call, discover).
        //
        // MCP_URL (if set) is available as an **emergency-only bypass** and is read
        // directly by McpTransport::connect() in mcp_client.rs when needed.
        // Under normal operation MCP_URL is not required and should not be set.
        let chord_proxy_url = secret_or("CHORD_PROXY_URL")
            .ok_or_else(|| LuminaError::Config("CHORD_PROXY_URL required (set in vault or environment)".to_string()))?;

        let lumina_chord_secret = secret_or("LUMINA_CHORD_SECRET").unwrap_or_default();

        if chord_proxy_url.is_empty() {
            return Err(LuminaError::Config("CHORD_PROXY_URL cannot be empty".to_string()));
        }

        if !chord_proxy_url.starts_with("http://") && !chord_proxy_url.starts_with("https://") {
            return Err(LuminaError::Config("CHORD_PROXY_URL must start with http:// or https://".to_string()));
        }

        let matrix_homeserver = secret_or("MATRIX_HOMESERVER").filter(|s| !s.is_empty());
        let matrix_user = secret_or("MATRIX_USER").filter(|s| !s.is_empty());
        let matrix_password = secret_or("MATRIX_PASSWORD").filter(|s| !s.is_empty());
        let matrix_room_id = secret_or("MATRIX_ROOM_ID").filter(|s| !s.is_empty());

        // Non-secret config — env only
        let matrix_store_path = env::var("MATRIX_STORE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".lumina")
                    .join("matrix-store")
            });

        let matrix_announce_startup = env::var("LUMINA_ANNOUNCE_STARTUP")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);

        let lumina_http_token = secret_or("LUMINA_HTTP_TOKEN").filter(|s| !s.is_empty());
        let lumina_http_bind = env::var("LUMINA_HTTP_BIND")
            .unwrap_or_else(|_| "127.0.0.1:3300".to_string());

        // EDGE-02: Load egress allowlist from env (non-secret, operators configure this).
        // Default: empty list (EgressInspector will fall back to loopback-only).
        let egress_allowlist: Vec<String> = env::var("LUMINA_EGRESS_ALLOWLIST")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // P2-01: admin Matrix ID for auto-promotion (non-secret — just an ID).
        let admin_matrix_id = env::var("LUMINA_ADMIN_MATRIX_ID")
            .ok()
            .filter(|s| !s.trim().is_empty());

        // MATRIX-SECURITY: comma-separated allowlist of permitted Matrix user IDs.
        // Empty list = allow all (backward-compatible default).
        let matrix_allowed_users: Vec<String> = env::var("MATRIX_ALLOWED_USERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(Config {
            chord_proxy_url,
            lumina_chord_secret,
            matrix_homeserver,
            matrix_user,
            matrix_password,
            matrix_room_id,
            matrix_store_path,
            matrix_announce_startup,
            lumina_http_token,
            lumina_http_bind,
            system_prompt: load_system_prompt(),
            egress_allowlist,
            admin_matrix_id,
            matrix_allowed_users,
        })
    }

    /// Validate that all required Matrix fields are present, returning a clear error if not.
    pub fn require_matrix(&self) -> Result<MatrixCredentials> {
        let homeserver = self.matrix_homeserver.clone()
            .ok_or_else(|| LuminaError::Config("MATRIX_HOMESERVER is required for --matrix mode".to_string()))?;
        let user = self.matrix_user.clone()
            .ok_or_else(|| LuminaError::Config("MATRIX_USER is required for --matrix mode".to_string()))?;
        let password = self.matrix_password.clone()
            .ok_or_else(|| LuminaError::Config("MATRIX_PASSWORD is required for --matrix mode".to_string()))?;
        let room_id = self.matrix_room_id.clone()
            .ok_or_else(|| LuminaError::Config("MATRIX_ROOM_ID is required for --matrix mode".to_string()))?;

        if !homeserver.starts_with("http://") && !homeserver.starts_with("https://") {
            return Err(LuminaError::Config("MATRIX_HOMESERVER must start with http:// or https://".to_string()));
        }

        // Normalize user ID: "lumina" → "@lumina:server.name"
        let full_user_id = if user.starts_with('@') {
            user.clone()
        } else {
            let server_name = extract_server_name(&homeserver);
            format!("@{}:{}", user, server_name)
        };

        Ok(MatrixCredentials { homeserver, user, full_user_id, password, room_id })
    }
}

#[derive(Debug, Clone)]
pub struct MatrixCredentials {
    pub homeserver: String,
    pub user: String,
    pub full_user_id: String,
    pub password: String,
    pub room_id: String,
}

fn extract_server_name(homeserver_url: &str) -> String {
    let without_scheme = homeserver_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    // Strip port if present
    without_scheme
        .split(':')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::sync::Mutex;

    // All config tests mutate global env vars — serialize them to avoid races.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_base_env() {
        env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        env::set_var("LUMINA_CHORD_SECRET", "test-secret-123");
    }

    fn clear_env() {
        for key in &["CHORD_PROXY_URL", "LUMINA_CHORD_SECRET", "MATRIX_HOMESERVER",
                      "MATRIX_USER", "MATRIX_PASSWORD", "MATRIX_ROOM_ID",
                      "LUMINA_HTTP_TOKEN", "LUMINA_HTTP_BIND", "LUMINA_ANNOUNCE_STARTUP",
                      "TERMINUS_HOST", "OLLAMA_EMBEDDING_URL", "ENGRAM_EMBED_MODEL",
                      "CONVERSATION_WINDOW", "SESSION_IDLE_MINUTES", "MCP_MAX_TOOL_CALLS",
                      "LUMINA_EGRESS_ALLOWLIST", "CHORD_AGENTIC_MODE",
                      "LUMINA_CONV_BUFFER_ENABLED", "LUMINA_CONV_BUFFER_SIZE",
                      "LUMINA_CONV_TOKEN_BUDGET", "LUMINA_SESSION_TIMEOUT_SECS"] {
            env::remove_var(key);
        }
    }

    #[test]
    fn test_config_from_env_success() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();

        let config = Config::from_env().expect("Config should load");
        assert_eq!(config.chord_proxy_url, "http://localhost:4000");
        assert_eq!(config.lumina_chord_secret, "test-secret-123");
        assert!(config.matrix_homeserver.is_none());

        clear_env();
    }

    #[test]
    #[serial]
    fn test_config_missing_url() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("LUMINA_CHORD_SECRET", "test-secret");

        let result = Config::from_env();
        assert!(result.is_err());
        assert!(matches!(result, Err(LuminaError::Config(_))));

        clear_env();
    }

    #[test]
    fn test_conv_buffer_enabled_flag_parse() {
        // Pure logic test — no env mutation (avoids races with parallel tests).
        assert!(parse_conv_buffer_enabled(None), "default is enabled");
        assert!(parse_conv_buffer_enabled(Some("true")));
        assert!(parse_conv_buffer_enabled(Some("1")));
        assert!(parse_conv_buffer_enabled(Some("yes")));
        assert!(!parse_conv_buffer_enabled(Some("false")), "explicit off");
        assert!(!parse_conv_buffer_enabled(Some("0")), "explicit off");
    }

    #[test]
    #[serial]
    fn test_config_missing_secret_allowed() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("CHORD_PROXY_URL", "http://localhost:4000");

        let config = Config::from_env().expect("Missing secret should be allowed");
        assert_eq!(config.lumina_chord_secret, "");

        clear_env();
    }

    #[test]
    #[serial]
    fn test_config_empty_url() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("CHORD_PROXY_URL", "");
        env::set_var("LUMINA_CHORD_SECRET", "test-secret");

        let result = Config::from_env();
        assert!(result.is_err());

        clear_env();
    }

    #[test]
    #[serial]
    fn test_config_invalid_url_format() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("CHORD_PROXY_URL", "not-a-url");
        env::set_var("LUMINA_CHORD_SECRET", "test-secret");

        let result = Config::from_env();
        assert!(result.is_err());

        clear_env();
    }

    #[test]
    #[serial]
    fn test_config_loads_matrix_fields() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        env::set_var("LUMINA_CHORD_SECRET", "");
        env::set_var("MATRIX_HOMESERVER", "http://localhost:6167");
        env::set_var("MATRIX_USER", "lumina");
        env::remove_var("MATRIX_PASSWORD");
        env::set_var("MATRIX_ROOM_ID", "!abc:example.com");

        let config = Config::from_env().expect("Config should load with Matrix fields set");
        assert_eq!(config.matrix_homeserver.as_deref(), Some("http://localhost:6167"));
        assert_eq!(config.matrix_user.as_deref(), Some("lumina"));
        assert!(config.matrix_password.is_none());
        assert_eq!(config.matrix_room_id.as_deref(), Some("!abc:example.com"));

        clear_env();
    }

    #[test]
    #[serial]
    fn test_require_matrix_missing_homeserver() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("MATRIX_USER", "lumina");
        env::set_var("MATRIX_PASSWORD", "pass");
        env::set_var("MATRIX_ROOM_ID", "!abc:example.com");

        let config = Config::from_env().expect("Config loads");
        let result = config.require_matrix();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("MATRIX_HOMESERVER"));

        clear_env();
    }

    #[test]
    #[serial]
    fn test_require_matrix_user_normalization() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        env::set_var("LUMINA_CHORD_SECRET", "");
        env::set_var("MATRIX_HOMESERVER", "http://localhost:6167");
        env::set_var("MATRIX_USER", "lumina");
        env::set_var("MATRIX_PASSWORD", "pass");
        env::set_var("MATRIX_ROOM_ID", "!abc:example.com");

        let config = Config::from_env().expect("Config loads");
        let creds = config.require_matrix().expect("Matrix creds should parse");
        assert_eq!(creds.full_user_id, "@lumina:localhost");

        env::set_var("MATRIX_USER", "@lumina:example.com");
        let config2 = Config::from_env().expect("Config loads");
        let creds2 = config2.require_matrix().expect("Matrix creds should parse");
        assert_eq!(creds2.full_user_id, "@lumina:example.com");

        clear_env();
    }

    #[test]
    #[serial]
    fn test_matrix_announce_startup_default() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        env::set_var("LUMINA_CHORD_SECRET", "test-secret-123");
        let config = Config::from_env().expect("Config loads");
        assert!(config.matrix_announce_startup);
        clear_env();
    }

    #[test]
    #[serial]
    fn test_matrix_announce_startup_disabled() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        env::set_var("LUMINA_CHORD_SECRET", "test-secret-123");
        env::set_var("LUMINA_ANNOUNCE_STARTUP", "false");
        let config = Config::from_env().expect("Config loads");
        assert!(!config.matrix_announce_startup);
        clear_env();
    }

    #[test]
    fn test_http_bind_default() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        let config = Config::from_env().expect("Config loads");
        assert_eq!(config.lumina_http_bind, "127.0.0.1:3300");
        clear_env();
    }

    #[test]
    fn test_phase1_config_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        let config = Config::from_env().expect("Config loads");
        assert_eq!(config.terminus_host(), "");
        assert_eq!(config.ollama_embedding_url(), "");
        assert_eq!(config.engram_embed_model(), "mxbai-embed-large");
        assert_eq!(config.conversation_window(), 20);
        assert_eq!(config.session_idle_minutes(), 30);
        assert_eq!(config.mcp_max_tool_calls(), 3);
        clear_env();
    }

    #[test]
    #[serial]
    fn test_phase1_config_overrides() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("TERMINUS_HOST", "terminus.local");
        env::set_var("OLLAMA_EMBEDDING_URL", "http://ollama.local:11434/api/embeddings");
        env::set_var("ENGRAM_EMBED_MODEL", "nomic-embed-text");
        env::set_var("CONVERSATION_WINDOW", "10");
        env::set_var("SESSION_IDLE_MINUTES", "15");
        env::set_var("MCP_MAX_TOOL_CALLS", "5");
        let config = Config::from_env().expect("Config loads");
        assert_eq!(config.terminus_host(), "terminus.local");
        assert_eq!(config.ollama_embedding_url(), "http://ollama.local:11434/api/embeddings");
        assert_eq!(config.engram_embed_model(), "nomic-embed-text");
        assert_eq!(config.conversation_window(), 10);
        assert_eq!(config.session_idle_minutes(), 15);
        assert_eq!(config.mcp_max_tool_calls(), 5);
        clear_env();
    }

    #[test]
    #[serial]
    fn test_phase1_config_bad_numeric_falls_back_to_default() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("CONVERSATION_WINDOW", "not-a-number");
        env::set_var("SESSION_IDLE_MINUTES", "abc");
        env::set_var("MCP_MAX_TOOL_CALLS", "");
        let config = Config::from_env().expect("Config loads");
        assert_eq!(config.conversation_window(), 20);
        assert_eq!(config.session_idle_minutes(), 30);
        assert_eq!(config.mcp_max_tool_calls(), 3);
        for key in &["CONVERSATION_WINDOW", "SESSION_IDLE_MINUTES", "MCP_MAX_TOOL_CALLS"] {
            env::remove_var(key);
        }
        clear_env();
    }

    #[test]
    fn test_egress_allowlist_default_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        let config = Config::from_env().expect("Config loads");
        assert!(config.egress_allowlist.is_empty(), "Default egress allowlist should be empty");
        clear_env();
    }

    #[test]
    #[serial]
    fn test_egress_allowlist_from_env() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("LUMINA_EGRESS_ALLOWLIST", "api.example.com, other.example.com, 198.51.100.1");
        let config = Config::from_env().expect("Config loads");
        assert_eq!(config.egress_allowlist.len(), 3);
        assert!(config.egress_allowlist.contains(&"api.example.com".to_string()));
        assert!(config.egress_allowlist.contains(&"other.example.com".to_string()));
        assert!(config.egress_allowlist.contains(&"198.51.100.1".to_string()));
        clear_env();
    }

    #[test]
    #[serial]
    fn test_egress_allowlist_trims_whitespace() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("LUMINA_EGRESS_ALLOWLIST", "  api.example.com  ,  198.51.100.1  ");
        let config = Config::from_env().expect("Config loads");
        assert!(config.egress_allowlist.contains(&"api.example.com".to_string()));
        assert!(config.egress_allowlist.contains(&"198.51.100.1".to_string()));
        clear_env();
    }

    #[test]
    #[serial]
    fn test_build_egress_inspector_with_allowlist() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("LUMINA_EGRESS_ALLOWLIST", "api.example.com");
        let config = Config::from_env().expect("Config loads");
        let inspector = config.build_egress_inspector();
        // Allowlisted host should pass
        assert!(inspector.inspect("https://api.example.com/", "tool").is_ok());
        // Non-allowlisted host should be blocked
        assert!(inspector.inspect("https://other.example.com/", "tool").is_err());
        clear_env();
    }

    // ── AGENT-02: chord_agentic_mode ─────────────────────────────────────────

    #[test]
    fn test_chord_agentic_mode_default_true() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        let config = Config::from_env().expect("Config loads");
        assert!(config.chord_agentic_mode(), "Default should be true when env var not set");
        clear_env();
    }

    #[test]
    #[serial]
    fn test_chord_agentic_mode_explicit_false() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("CHORD_AGENTIC_MODE", "false");
        let config = Config::from_env().expect("Config loads");
        assert!(!config.chord_agentic_mode(), "Should be false when set to 'false'");
        clear_env();
    }

    #[test]
    #[serial]
    fn test_chord_agentic_mode_explicit_zero() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("CHORD_AGENTIC_MODE", "0");
        let config = Config::from_env().expect("Config loads");
        assert!(!config.chord_agentic_mode(), "Should be false when set to '0'");
        clear_env();
    }

    #[test]
    #[serial]
    fn test_chord_agentic_mode_explicit_true() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("CHORD_AGENTIC_MODE", "true");
        let config = Config::from_env().expect("Config loads");
        assert!(config.chord_agentic_mode(), "Should be true when set to 'true'");
        clear_env();
    }

    #[test]
    #[serial]
    fn test_chord_agentic_mode_explicit_one() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        env::set_var("CHORD_AGENTIC_MODE", "1");
        let config = Config::from_env().expect("Config loads");
        assert!(config.chord_agentic_mode(), "Should be true when set to '1'");
        clear_env();
    }

    #[test]
    fn test_build_egress_inspector_empty_allowlist_falls_back_to_loopback() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        set_base_env();
        // No LUMINA_EGRESS_ALLOWLIST set — config.egress_allowlist is empty
        // build_egress_inspector should fall back to from_env() → loopback-only default
        let config = Config::from_env().expect("Config loads");
        assert!(config.egress_allowlist.is_empty());
        let inspector = config.build_egress_inspector();
        assert!(inspector.inspect("http://localhost/", "tool").is_ok());
        assert!(inspector.inspect("http://127.0.0.1/", "tool").is_ok());
        assert!(inspector.inspect("https://api.example.com/", "tool").is_err());
        clear_env();
    }
}