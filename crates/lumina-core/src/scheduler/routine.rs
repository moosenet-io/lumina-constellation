//! EDGE-05/EDGE-06: Routine struct — a single scheduled/event-triggered task definition.

use crate::error::{LuminaError, Result};
use crate::scheduler::cron::CronExpr;
use crate::scheduler::events::EventTrigger;
use crate::retrain_scheduler::unix_secs_to_iso8601;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ── RoutineChannel ────────────────────────────────────────────────────────────

/// Where routine results are delivered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutineChannel {
    /// Post result to the Matrix room.
    Matrix,
    /// Write result to `~/.lumina/routine-output/{name}-{date}.md`.
    File,
    /// Print result to stdout (useful for testing).
    Stdout,
}

impl Default for RoutineChannel {
    fn default() -> Self {
        RoutineChannel::Stdout
    }
}

// ── RoutineTrigger ────────────────────────────────────────────────────────────

/// How a routine is activated: either on a cron schedule (EDGE-05) or by a
/// specific event (EDGE-06).
///
/// When `trigger` is absent in the TOML the routine defaults to
/// `RoutineTrigger::Cron` using the `schedule` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineTrigger {
    /// Fire on a cron schedule — value is the cron expression string.
    Cron(String),
    /// Fire when the matching event is emitted on the [`EventBus`].
    Event(EventTrigger),
}

impl RoutineTrigger {
    /// Returns `true` if this trigger is cron-based.
    pub fn is_cron(&self) -> bool {
        matches!(self, RoutineTrigger::Cron(_))
    }

    /// Returns `true` if this trigger is event-based.
    pub fn is_event(&self) -> bool {
        matches!(self, RoutineTrigger::Event(_))
    }
}

// ── Routine ───────────────────────────────────────────────────────────────────

/// A single configured routine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routine {
    /// Stable identifier for this routine.
    pub name: String,
    /// Cron expression: `"min hour dom month dow"`.
    ///
    /// Required for cron-based routines.  Ignored when `trigger` is set to an
    /// event variant.  Kept for backwards-compatibility with EDGE-05 configs.
    #[serde(default)]
    pub schedule: String,
    /// The message sent to the agent when this routine fires.
    pub prompt: String,
    /// If set, override the default model for this routine.
    pub model_override: Option<String>,
    /// Where to deliver the result.
    #[serde(default)]
    pub channel: RoutineChannel,
    /// Whether this routine should run.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// ISO-8601 timestamp of the last execution (UTC).
    pub last_run: Option<String>,
    /// ISO-8601 timestamp of the next scheduled execution (UTC).
    pub next_run: Option<String>,
    /// How the routine is triggered.  `None` means use the `schedule` cron
    /// field (EDGE-05 backwards compatibility).
    pub trigger: Option<RoutineTrigger>,
}

fn default_true() -> bool {
    true
}

impl Routine {
    /// Returns `true` if `next_run` is set and <= now, or if `next_run` is `None`.
    ///
    /// Routines with no `next_run` (e.g. freshly loaded without a calculated
    /// next-run yet) are treated as not due.
    pub fn is_due(&self) -> bool {
        match &self.next_run {
            None => false,
            Some(ts) => {
                let now = current_iso8601();
                // Lexicographic comparison of ISO-8601 UTC timestamps is valid
                // because the format is left-to-right from most- to least-significant.
                ts.as_str() <= now.as_str()
            }
        }
    }

    /// Parse the cron expression, calculate the next run time from now,
    /// and set `self.next_run`.
    pub fn update_next_run(&mut self, cron_expr: &str) -> Result<()> {
        let parsed = CronExpr::parse(cron_expr)?;
        let next = parsed.next_after(SystemTime::now());
        let secs = next
            .duration_since(UNIX_EPOCH)
            .map_err(|e| LuminaError::Internal(format!("SystemTime before UNIX_EPOCH: {}", e)))?
            .as_secs();
        self.next_run = Some(unix_secs_to_iso8601(secs));
        Ok(())
    }
}

// ── RoutinesConfig ────────────────────────────────────────────────────────────

/// Top-level TOML structure for the routines config file.
#[derive(Debug, Deserialize)]
pub struct RoutinesConfig {
    #[serde(default)]
    pub routines: Vec<Routine>,
}

impl RoutinesConfig {
    /// Load routines from a TOML file at `path`.
    ///
    /// If the file does not exist, returns an empty config (no error).
    /// Returns an error only for file read or TOML parse failures.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(RoutinesConfig { routines: Vec::new() })
            }
            Err(e) => Err(LuminaError::Io(e)),
            Ok(content) => {
                toml::from_str::<RoutinesConfig>(&content)
                    .map_err(|e| LuminaError::Config(format!("routines.toml parse error: {}", e)))
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Return the current UTC time as an ISO-8601 string.
fn current_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    unix_secs_to_iso8601(secs)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_routine(schedule: &str, next_run: Option<&str>, enabled: bool) -> Routine {
        Routine {
            name: "test".to_string(),
            schedule: schedule.to_string(),
            prompt: "hello".to_string(),
            model_override: None,
            channel: RoutineChannel::Stdout,
            enabled,
            last_run: None,
            next_run: next_run.map(|s| s.to_string()),
            trigger: None,
        }
    }

    #[test]
    fn test_routine_is_due_past_timestamp() {
        // A timestamp well in the past should be due.
        let r = make_routine("0 7 * * *", Some("2020-01-01T00:00:00Z"), true);
        assert!(r.is_due());
    }

    #[test]
    fn test_routine_not_due_future_timestamp() {
        // A timestamp far in the future should not be due.
        let r = make_routine("0 7 * * *", Some("2099-01-01T00:00:00Z"), true);
        assert!(!r.is_due());
    }

    #[test]
    fn test_routine_not_due_when_next_run_none() {
        let r = make_routine("0 7 * * *", None, true);
        assert!(!r.is_due());
    }

    #[test]
    fn test_update_next_run_sets_future_timestamp() {
        let mut r = make_routine("0 7 * * *", None, true);
        r.update_next_run("0 7 * * *").unwrap();
        assert!(r.next_run.is_some());
        // The next_run should be in the future — i.e. greater than now-ish.
        let next = r.next_run.unwrap();
        let now = unix_secs_to_iso8601(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        assert!(next.as_str() > now.as_str(), "next_run should be in the future");
    }

    #[test]
    fn test_update_next_run_invalid_cron() {
        let mut r = make_routine("bad cron", None, true);
        assert!(r.update_next_run("bad cron").is_err());
    }

    #[test]
    fn test_routines_config_load_missing_file() {
        let path = Path::new("/tmp/lumina_test_missing_routines_EDGE05.toml");
        // Ensure the file doesn't exist.
        let _ = std::fs::remove_file(path);
        let config = RoutinesConfig::load(path).unwrap();
        assert!(config.routines.is_empty());
    }

    #[test]
    fn test_routines_config_load_valid_toml() {
        let path = std::path::PathBuf::from("/tmp/lumina_test_routine_valid_EDGE05.toml");
        std::fs::write(
            &path,
            r#"
[[routines]]
name = "morning_briefing"
schedule = "0 7 * * *"
prompt = "Good morning."
channel = "matrix"
enabled = true
"#,
        )
        .unwrap();

        let config = RoutinesConfig::load(&path).unwrap();
        assert_eq!(config.routines.len(), 1);
        assert_eq!(config.routines[0].name, "morning_briefing");
        assert_eq!(config.routines[0].channel, RoutineChannel::Matrix);
        assert!(config.routines[0].enabled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_routines_config_empty_toml() {
        let path = std::path::PathBuf::from("/tmp/lumina_test_routine_empty_EDGE05.toml");
        std::fs::write(&path, "").unwrap();
        let config = RoutinesConfig::load(&path).unwrap();
        assert!(config.routines.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_routines_config_invalid_toml() {
        let path = std::path::PathBuf::from("/tmp/lumina_test_routine_invalid_EDGE05.toml");
        std::fs::write(&path, "not valid toml [[[[").unwrap();
        assert!(RoutinesConfig::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_routine_channel_deserialization() {
        let toml_str = r#"
[[routines]]
name = "test"
schedule = "0 * * * *"
prompt = "check"
channel = "file"
"#;
        let config: RoutinesConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.routines[0].channel, RoutineChannel::File);
    }

    #[test]
    fn test_routine_defaults() {
        // enabled should default to true, channel to Stdout
        let toml_str = r#"
[[routines]]
name = "minimal"
schedule = "0 * * * *"
prompt = "hello"
"#;
        let config: RoutinesConfig = toml::from_str(toml_str).unwrap();
        let r = &config.routines[0];
        assert!(r.enabled);
        assert_eq!(r.channel, RoutineChannel::Stdout);
        assert!(r.model_override.is_none());
        // trigger defaults to None (cron-based backward-compat)
        assert!(r.trigger.is_none());
    }

    // ── RoutineTrigger ────────────────────────────────────────────────────────

    #[test]
    fn test_routine_trigger_cron_is_cron() {
        let t = RoutineTrigger::Cron("0 7 * * *".to_string());
        assert!(t.is_cron());
        assert!(!t.is_event());
    }

    #[test]
    fn test_routine_trigger_event_is_event() {
        use crate::scheduler::events::EventTrigger;
        let t = RoutineTrigger::Event(EventTrigger::ToolFailure);
        assert!(t.is_event());
        assert!(!t.is_cron());
    }

    #[test]
    fn test_routine_with_event_trigger_deserialises() {
        let toml_str = r#"
[[routines]]
name = "tool_failure_alert"
prompt = "A tool just failed. Check the audit log."
channel = "matrix"
trigger = { event = "ToolFailure" }
"#;
        let config: RoutinesConfig = toml::from_str(toml_str).unwrap();
        let r = &config.routines[0];
        assert_eq!(r.name, "tool_failure_alert");
        assert!(r.trigger.is_some());
        let trig = r.trigger.as_ref().unwrap();
        assert!(trig.is_event());
    }

    #[test]
    fn test_routine_cron_and_event_coexist() {
        // Both types can be loaded from the same TOML.
        let toml_str = r#"
[[routines]]
name = "cron_routine"
schedule = "0 7 * * *"
prompt = "good morning"
enabled = true

[[routines]]
name = "event_routine"
prompt = "tool failed"
trigger = { event = "ToolFailure" }
enabled = true
"#;
        let config: RoutinesConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.routines.len(), 2);
        assert!(config.routines[0].trigger.is_none()); // cron uses schedule field
        assert!(config.routines[1].trigger.is_some());
    }
}
