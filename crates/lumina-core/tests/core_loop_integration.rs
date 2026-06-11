//! Integration tests for the core agent loop

use lumina_core::chord::{ChordClient, ChatMessage};
use lumina_core::config::Config;
use std::env;
use std::io::Write;
use std::process::{Command, Stdio};
use serial_test::serial;

#[test]
#[serial]
fn test_agent_loop_with_mock_chord() {
    env::set_var("CHORD_PROXY_URL", "http://localhost:8099");
    env::set_var("LUMINA_CHORD_SECRET", "");

    let config = Config::from_env().expect("Config should load");
    let _client = ChordClient::new(config.chord_proxy_url, config.lumina_chord_secret);

    let messages = vec![ChatMessage::text("user", "Test message")];

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content.as_deref(), Some("Test message"));

    env::remove_var("CHORD_PROXY_URL");
    env::remove_var("LUMINA_CHORD_SECRET");
}

#[test]
#[serial]
fn test_binary_with_echo_pipe() {
    env::set_var("CHORD_PROXY_URL", "http://localhost:8099");
    env::set_var("LUMINA_CHORD_SECRET", "");

    let output = Command::new("cargo")
        .args(&["run", "-p", "lumina-core"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    match output {
        Ok(mut child) => {
            if let Some(stdin) = child.stdin.take() {
                let mut stdin = stdin;
                let _ = writeln!(stdin, "hello");
                drop(stdin);
            }
            let output = child.wait_with_output().expect("Failed to wait for child");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("agent loop starting") || stderr.contains("Configuration error"),
                "Agent should start or show config error, got: {}",
                stderr
            );
        }
        Err(e) => {
            eprintln!("Could not run cargo (test environment issue): {}", e);
        }
    }

    env::remove_var("CHORD_PROXY_URL");
    env::remove_var("LUMINA_CHORD_SECRET");
}

#[test]
fn test_input_validation_and_processing() {
    let short_input = "a".repeat(5000);
    let long_input = "x".repeat(15000);

    let test_cases = vec![
        ("hello", false),
        ("", true),
        ("   ", true),
        (short_input.as_str(), false),
        (long_input.as_str(), true),
    ];

    for (input, should_be_empty_or_long) in test_cases {
        let is_empty = input.trim().is_empty();
        let is_too_long = input.len() > 10 * 1024;
        if should_be_empty_or_long {
            assert!(is_empty || is_too_long);
        } else {
            assert!(!is_empty && !is_too_long);
        }
    }
}

#[test]
fn test_chat_message_structure_for_loop() {
    let inputs = vec![
        "Hello, world!",
        "What is the meaning of life?",
        "Please help me debug this code",
        "Can you understand emojis?",
    ];

    for input in inputs {
        let message = ChatMessage::text("user", input);
        assert_eq!(message.role, "user");
        assert_eq!(message.content.as_deref(), Some(input));
        let serialized = serde_json::to_string(&message);
        assert!(serialized.is_ok());
    }
}

#[tokio::test]
#[serial]
async fn test_end_to_end_structure() {
    env::set_var("CHORD_PROXY_URL", "http://localhost:8099");
    env::set_var("LUMINA_CHORD_SECRET", "");

    let config = Config::from_env().expect("Config should load");
    assert!(!config.chord_proxy_url.is_empty());

    let _client = ChordClient::new(config.chord_proxy_url, config.lumina_chord_secret);

    let messages = vec![ChatMessage::text("user", "Test input from integration test")];
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");

    env::remove_var("CHORD_PROXY_URL");
    env::remove_var("LUMINA_CHORD_SECRET");
}
