// Lumina Core — clean-room Rust rewrite
// The agent loop: input → LLM reason → output

mod agent_loop;
mod caldav;
mod channels;
mod chord;
mod chord_lifecycle;
mod config;
mod error;
mod hardening;
mod input_guard;
mod matrix_bot;
mod matrix_format;
mod router;
mod router_rules;
mod secure_string;
mod security;
mod conversation;
mod prompt;
mod onboarding;
mod tool_types;
mod tool_gate;
mod audit_log;
mod wasm_sandbox;
mod egress_inspector;
mod engram;
mod mcp_client;
mod tool_discovery;
mod tool_resolver;
mod nexus;
mod training_store;
mod train_cli;
mod retrain_scheduler;
mod scheduler;
mod skills;
mod users;
mod user_config;
mod vault;
mod web;

#[cfg(feature = "http")]
mod http_server;

use std::sync::Arc;

use agent_loop::run_agent_loop;
use config::Config;
use error::{LuminaError, Result};
use matrix_bot::MatrixBot;
use train_cli::handle_train_command;
use users::cli::handle_users_command;
use engram::cli::handle_memories_command;
use vault::wizard::{VaultCli, VaultWizard};

/// CLI run mode parsed from args.
enum RunMode {
    Stdin,
    Matrix,
    Http,
    MatrixAndHttp,
}

/// Handle vault subcommands (vault init/set/get/list/remove).
/// Returns true if a vault command was handled — caller should exit cleanly.
fn handle_vault_command(args: &[String]) -> bool {
    if args.len() < 2 || args[1] != "vault" {
        return false;
    }

    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("help");

    match sub {
        "init" => {
            if let Err(e) = VaultWizard::run() {
                eprintln!("Vault init error: {}", e);
                std::process::exit(1);
            }
        }
        "set" => {
            let key = match args.get(3) {
                Some(k) => k.clone(),
                None => {
                    eprintln!("Usage: lumina-core vault set <key> <value>");
                    std::process::exit(1);
                }
            };
            let value = args.get(4).cloned().unwrap_or_default();
            if let Err(e) = VaultCli::set(key, value) {
                eprintln!("Vault set error: {}", e);
                std::process::exit(1);
            }
        }
        "get" => {
            let key = match args.get(3) {
                Some(k) => k.clone(),
                None => {
                    eprintln!("Usage: lumina-core vault get <key>");
                    std::process::exit(1);
                }
            };
            if let Err(e) = VaultCli::get(key) {
                eprintln!("Vault get error: {}", e);
                std::process::exit(1);
            }
        }
        "list" => {
            if let Err(e) = VaultCli::list() {
                eprintln!("Vault list error: {}", e);
                std::process::exit(1);
            }
        }
        "remove" => {
            let key = match args.get(3) {
                Some(k) => k.clone(),
                None => {
                    eprintln!("Usage: lumina-core vault remove <key>");
                    std::process::exit(1);
                }
            };
            if let Err(e) = VaultCli::remove(key) {
                eprintln!("Vault remove error: {}", e);
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("Unknown vault subcommand: '{}'", sub);
            eprintln!("Subcommands: init, set <key> <value>, get <key>, list, remove <key>");
            std::process::exit(1);
        }
    }

    true
}

fn parse_run_mode() -> RunMode {
    let args: Vec<String> = std::env::args().collect();
    let has_matrix = args.contains(&"--matrix".to_string());
    let has_http = args.contains(&"--http".to_string());

    match (has_matrix, has_http) {
        (true, true) => RunMode::MatrixAndHttp,
        (true, false) => RunMode::Matrix,
        (false, true) => RunMode::Http,
        (false, false) => RunMode::Stdin,
    }
}

fn print_help() {
    eprintln!("Usage: lumina-core [--matrix] [--http]");
    eprintln!("       lumina-core train <subcommand>");
    eprintln!("       lumina-core users <subcommand>");
    eprintln!("       lumina-core memories <subcommand>");
    eprintln!("       lumina-core vault <subcommand>");
    eprintln!();
    eprintln!("Run modes:");
    eprintln!("  (no flags)       Stdin agent loop — reads from stdin, writes to stdout");
    eprintln!("  --matrix         Matrix bot mode — connects to Tuwunel, listens in room");
    eprintln!("  --http           HTTP server mode — POST /v1/chat/completions on LUMINA_HTTP_BIND");
    eprintln!("  --matrix --http  Both Matrix and HTTP simultaneously");
    eprintln!();
    eprintln!("Train subcommands (FORGE-05 curation CLI):");
    train_cli::print_train_help();
    eprintln!();
    eprintln!("Users subcommands (P2-06 user management):");
    users::cli::print_users_help();
    eprintln!();
    eprintln!("Memories subcommands (EMEM-10 memory management):");
    engram::cli::print_memories_help();
    eprintln!();
    eprintln!("Vault subcommands:");
    eprintln!("  vault init              Interactive setup wizard (generates key, creates vault)");
    eprintln!("  vault set <key> <val>   Store a secret in the encrypted vault");
    eprintln!("  vault get <key>         Read a secret from the vault");
    eprintln!("  vault list              List all secret keys (values never shown)");
    eprintln!("  vault remove <key>      Remove a secret from the vault");
    eprintln!();
    eprintln!("Required secrets (all modes): CHORD_PROXY_URL");
    eprintln!("Matrix mode also requires: MATRIX_HOMESERVER, MATRIX_USER, MATRIX_PASSWORD, MATRIX_ROOM_ID");
    eprintln!("HTTP mode also requires:   LUMINA_HTTP_TOKEN (optional but recommended)");
    eprintln!("Secrets are read from the vault first, then environment variables as fallback.");
}

#[tokio::main]
async fn main() -> Result<()> {
    // HARDEN-02: process hardening runs before vault decryption or any secret handling
    hardening::init()?;

    let args: Vec<String> = std::env::args().collect();
    if args.contains(&"--help".to_string()) || args.contains(&"-h".to_string()) {
        print_help();
        return Ok(());
    }

    // FORGE-05: train subcommands run before any service startup
    if handle_train_command(&args) {
        return Ok(());
    }

    // P2-06: user management subcommands run before any service startup
    if handle_users_command(&args) {
        return Ok(());
    }

    // EMEM-10: memory management subcommands run before any service startup
    if handle_memories_command(&args) {
        return Ok(());
    }

    // Vault subcommands run before any service startup
    if handle_vault_command(&args) {
        return Ok(());
    }

    // GUARD-02: soft-init vault — loads encrypted secrets if vault.key + vault.enc exist.
    // Logs a notice and continues if vault is not configured; config falls back to env vars.
    if let Err(e) = vault::init() {
        eprintln!("lumina-core: vault not available ({}), reading secrets from environment", e);
    }

    // HARDEN-06: retention cleanup at startup
    run_startup_cleanup();

    let mode = parse_run_mode();
    let config = Arc::new(Config::from_env().map_err(|e| {
        eprintln!("Configuration error: {}", e);
        eprintln!("Run with --help for usage.");
        e
    })?);

    // CONV-03: install the process-global conversation buffer (Tier-1 working
    // memory) and spawn a periodic sweep that closes inactive sessions and frees
    // their memory. Sized from config (LUMINA_CONV_BUFFER_SIZE / _TOKEN_BUDGET /
    // _SESSION_TIMEOUT_SECS). Disabled buffers still init harmlessly; the agent
    // loop checks LUMINA_CONV_BUFFER_ENABLED per turn.
    {
        use conversation::buffer::ConversationBuffer;
        let buffer = std::sync::Arc::new(std::sync::RwLock::new(ConversationBuffer::new(
            config.conv_buffer_size(),
            config.conv_token_budget(),
            config.conv_session_timeout_secs(),
        )));
        conversation::buffer::init_global(buffer);
        tokio::spawn(async {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                tick.tick().await;
                // CONV-04: sweep expired sessions and flush each to Engram as a raw
                // episodic record before its memory is freed. Snapshot the closed
                // sessions out of the lock, then flush (IO) without holding it.
                let closed = match conversation::buffer::global() {
                    Some(buf) => {
                        let now = conversation::buffer::unix_now();
                        match buf.write() {
                            Ok(mut b) => b.cleanup_expired(now),
                            Err(_) => Vec::new(),
                        }
                    }
                    None => Vec::new(),
                };
                if !closed.is_empty() {
                    eprintln!("conversation: swept {} expired session(s)", closed.len());
                }
                // Flush swept sessions + retry the pending queue on a blocking
                // thread (synchronous SQLCipher IO must not stall the executor).
                tokio::task::spawn_blocking(move || {
                    for (user_id, session) in &closed {
                        conversation::engram_flush::flush_session(user_id, session);
                    }
                    conversation::engram_flush::retry_pending();
                });
            }
        });
    }

    // EDGE-05/EDGE-06: Start the background routine scheduler alongside the main service.
    // Routines are loaded from LUMINA_ROUTINES_PATH (env) or ~/.lumina/routines.toml.
    // If the file doesn't exist, the scheduler starts with an empty list (no error).
    // An EventBus is created here, shared with the scheduler (for event-triggered routines)
    // and registered globally so agent_loop can emit events (rate-limit hits, circuit opens).
    let event_bus = std::sync::Arc::new(scheduler::EventBus::new());
    agent_loop::set_event_bus(event_bus.clone());
    let _scheduler_handle = start_scheduler_with_events(event_bus);

    // EDGE-08: Build the channel registry.
    //
    // The registry provides the channel adapter infrastructure introduced in EDGE-08.
    // In this phase, non-conflicting channels (Matrix, HTTP) are registered here.
    //
    // CliChannel is intentionally NOT registered here: starting it would spawn a
    // tokio task reading from stdin that races with the existing run_agent_loop()
    // stdin reader in RunMode::Stdin.  The CliChannel is fully functional and is
    // used in tests; it will replace run_agent_loop() in EDGE-09 when the unified
    // channel_rx → process_message() pipeline is wired up.
    //
    // _channel_rx is held here (not dropped) so the channel is not closed.
    let (_channel_tx, _channel_rx) = tokio::sync::mpsc::channel::<channels::ChannelMessage>(256);
    let mut channel_registry = channels::ChannelRegistry::new();
    // Note: CliChannel deferred to EDGE-09 (see comment above).
    // Matrix and HTTP channels require credentials checked in run modes below.

    match mode {
        RunMode::Stdin => {
            if let Err(e) = run_agent_loop().await {
                match e {
                    LuminaError::Config(msg) => {
                        eprintln!("Configuration error: {}", msg);
                        eprintln!("Set CHORD_PROXY_URL via vault ('lumina-core vault set') or environment.");
                        std::process::exit(1);
                    }
                    _ => {
                        eprintln!("Agent loop error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }

        RunMode::Matrix => {
            run_matrix(config).await?;
        }

        RunMode::Http => {
            #[cfg(feature = "http")]
            {
                http_server::serve(config).await?;
            }
            #[cfg(not(feature = "http"))]
            {
                eprintln!("Error: --http requires the 'http' cargo feature.");
                eprintln!("Rebuild with: cargo build --features http");
                std::process::exit(1);
            }
        }

        RunMode::MatrixAndHttp => {
            #[cfg(feature = "http")]
            {
                let config2 = config.clone();
                let http_handle = tokio::spawn(async move {
                    if let Err(e) = http_server::serve(config2).await {
                        eprintln!("HTTP server error: {}", e);
                    }
                });
                run_matrix(config).await?;
                http_handle.abort();
            }
            #[cfg(not(feature = "http"))]
            {
                eprintln!("Error: --http requires the 'http' cargo feature.");
                std::process::exit(1);
            }
        }
    }

    // Stop all channels on clean exit.
    channel_registry.stop_all().await;

    Ok(())
}

/// EDGE-05/EDGE-06: Start the background routine scheduler with EventBus support.
///
/// The routines config path is read from `LUMINA_ROUTINES_PATH` (env var) or
/// defaults to `~/.lumina/routines.toml`.  If the file does not exist, the
/// scheduler starts with an empty routine list — no error or warning is emitted.
///
/// `event_bus` is shared with the scheduler so event-triggered routines fire when
/// the agent_loop emits events (rate-limit hits, circuit opens, etc.).
///
/// Returns a `SchedulerHandle` that is held by the caller for the lifetime of the
/// process.  On process exit, the handle is dropped and the task stops naturally.
fn start_scheduler_with_events(event_bus: std::sync::Arc<scheduler::EventBus>) -> Option<scheduler::SchedulerHandle> {
    let routines_path = {
        let from_env = std::env::var("LUMINA_ROUTINES_PATH").ok();
        match from_env {
            Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
            _ => dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".lumina")
                .join("routines.toml"),
        }
    };

    match scheduler::Scheduler::new(&routines_path) {
        Ok(sched) => {
            let n = sched.list_routines().iter().filter(|r| r.enabled).count();
            if n > 0 {
                eprintln!("lumina-scheduler: started with {} enabled routine(s)", n);
            }
            let handle = sched.start_with_events(event_bus, |routine| {
                // Deliver the result to the configured channel.
                // Truncate prompt to 80 chars safely (avoid byte-boundary panic on non-ASCII).
                let prompt_preview: String = routine.prompt.chars().take(80).collect();
                let output = format!(
                    "[scheduler] routine '{}' fired (prompt: {})",
                    routine.name,
                    prompt_preview
                );
                eprintln!("{}", output);
                output
            });
            Some(handle)
        }
        Err(e) => {
            eprintln!("lumina-scheduler: failed to load routines from {:?}: {}", routines_path, e);
            None
        }
    }
}

fn run_startup_cleanup() {
    let retention_days: u64 = std::env::var("LUMINA_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90);

    // Audit log retention (rotated .gz files)
    if let Some(home) = dirs::home_dir() {
        let log_path = home.join(".lumina").join("audit.jsonl");
        let cleaned = security::cleanup_old_rotated_logs(&log_path, retention_days);
        if cleaned > 0 {
            eprintln!("lumina-core: retention — removed {} old audit log file(s)", cleaned);
        }
    }

    // Training data retention (encrypted SQLCipher database)
    match training_store::TrainingStore::open_default() {
        Ok(store) => match store.cleanup_expired(retention_days) {
            Ok(0) => {}
            Ok(n) => eprintln!(
                "lumina-core: retention — removed {} uncurated training turn(s) older than {} days",
                n, retention_days
            ),
            Err(e) => eprintln!("lumina-core: retention cleanup error: {}", e),
        },
        Err(_) => {} // DB not yet created — nothing to clean up
    }
}

async fn run_matrix(config: Arc<Config>) -> Result<()> {
    // Register signal handlers for graceful shutdown
    let shutdown = tokio::signal::ctrl_c();

    let bot_fut = async {
        let bot = MatrixBot::connect(config.clone()).await?;
        bot.login().await?;
        bot.join_room().await?;
        bot.initial_sync().await?;
        bot.run().await
    };

    tokio::select! {
        result = bot_fut => {
            if let Err(e) = result {
                eprintln!("Matrix bot error: {}", e);
                std::process::exit(1);
            }
        }
        _ = shutdown => {
            eprintln!("Received shutdown signal");
            // Announcement is handled inside MatrixBot::run's drop logic
            // (future improvement: pass cancellation token to post shutdown message)
        }
    }

    Ok(())
}
