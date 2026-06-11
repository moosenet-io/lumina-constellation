//! Lumina Core Library
//!
//! The core agent loop for Lumina: input → LLM reason → output

pub mod vault;
pub mod error;
pub mod secure_string;
pub mod hardening;
pub mod agent_loop;
pub mod chord;
pub mod chord_lifecycle;
pub mod config;
pub mod router_rules;
pub mod router;
pub mod security;
pub mod input_guard;
pub mod matrix_bot;
pub mod matrix_format;
pub mod training_store;
pub mod train_cli;
pub mod retrain_scheduler;
pub mod conversation;
pub mod prompt;
pub mod onboarding;
pub mod tool_types;
pub mod tool_gate;
pub mod audit_log;
pub mod wasm_sandbox;
pub mod egress_inspector;
pub mod engram;
pub mod nexus;
pub mod mcp_client;
pub mod tool_resolver;
pub mod scheduler;
pub mod skills;
pub mod channels;
pub mod vigil;
pub mod users;
pub mod user_config;
pub mod tool_builder;
pub mod caldav;
pub mod email;
pub mod sentinel;
pub mod feeds;
pub mod skill_hub;

#[cfg(feature = "http")]
pub mod http_server;

pub mod dashboard;
pub mod pwa;
pub mod soma;
pub mod web;
pub mod connectors;
pub mod tool_discovery;
