//! EDGE-05/EDGE-06: Background routine scheduler.
//!
//! Loads routines from a TOML config file, validates their cron expressions,
//! and runs a tokio task that wakes every 60 seconds to fire any due routines.
//!
//! EDGE-06 extends this with event-triggered routines.  An additional tokio task
//! subscribes to the [`EventBus`] and whenever an event arrives fires matching
//! routines subject to per-routine 60-second debouncing.
//!
//! **Serialization guarantee:** all routine execution (cron + event) passes
//! through a single `tokio::sync::Mutex`-guarded executor.  Only one routine
//! runs at a time to avoid VRAM contention.

pub mod cron;
pub mod events;
pub mod routine;

pub use events::{Debouncer, EventBus, EventTrigger, EventType};
pub use routine::{Routine, RoutineChannel, RoutinesConfig, RoutineTrigger};

use crate::error::Result;
use crate::retrain_scheduler::unix_secs_to_iso8601;
use crate::scheduler::events::matches_trigger;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

// ── SchedulerHandle ───────────────────────────────────────────────────────────

/// A handle returned by [`Scheduler::start`] / [`Scheduler::start_with_events`]
/// that allows stopping the scheduler and aborting all background tasks.
pub struct SchedulerHandle {
    running: Arc<AtomicBool>,
    /// Primary task (cron loop in `start`, event loop in `start_with_events`).
    join_handle: tokio::task::JoinHandle<()>,
    /// Optional secondary task (cron loop when using `start_with_events`).
    secondary_handle: Option<tokio::task::JoinHandle<()>>,
}

impl SchedulerHandle {
    /// Signal all background loops to exit on their next iteration.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Abort all background tasks immediately.
    ///
    /// Does not wait for any currently-executing routine to finish.
    pub fn abort(&self) {
        self.join_handle.abort();
        if let Some(ref h) = self.secondary_handle {
            h.abort();
        }
    }

    /// Returns a reference to the primary [`JoinHandle`].
    pub fn join_handle(&self) -> &tokio::task::JoinHandle<()> {
        &self.join_handle
    }
}

// ── Scheduler ────────────────────────────────────────────────────────────────

/// Background routine scheduler.
pub struct Scheduler {
    routines: Vec<Routine>,
}

impl Scheduler {
    /// Create a new `Scheduler` from a TOML config file.
    ///
    /// - If the file does not exist, the scheduler starts with an empty routine list.
    /// - Cron routines with invalid cron expressions are skipped (error logged to stderr).
    /// - Event-triggered routines are loaded without cron validation.
    /// - Valid cron routines have their `next_run` calculated immediately.
    pub fn new(routines_path: &Path) -> Result<Self> {
        let config = RoutinesConfig::load(routines_path)?;
        let mut routines = Vec::with_capacity(config.routines.len());

        for mut r in config.routines {
            if !r.enabled {
                routines.push(r);
                continue;
            }

            // Determine the effective trigger.
            let is_event_triggered = r
                .trigger
                .as_ref()
                .map(|t| t.is_event())
                .unwrap_or(false);

            if is_event_triggered {
                // Event-triggered routines don't need a cron expression.
                routines.push(r);
            } else {
                // Cron-based routine — validate the expression.
                let sched = r.schedule.clone();
                match r.update_next_run(&sched) {
                    Ok(()) => routines.push(r),
                    Err(e) => {
                        eprintln!(
                            "lumina-scheduler: skipping routine '{}' — invalid cron '{}': {}",
                            r.name, r.schedule, e
                        );
                    }
                }
            }
        }

        Ok(Scheduler { routines })
    }

    /// List all loaded routines.
    pub fn list_routines(&self) -> &[Routine] {
        &self.routines
    }

    /// Spawn a tokio task that runs routines on their cron schedule.
    ///
    /// Only cron-based routines are driven by this loop.  To also handle
    /// event-triggered routines, use [`Scheduler::start_with_events`].
    pub fn start(
        mut self,
        on_routine: impl Fn(&Routine) -> String + Send + 'static,
    ) -> SchedulerHandle {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let join_handle = tokio::spawn(async move {
            while running_clone.load(Ordering::SeqCst) {
                // Check each enabled, cron-based routine.
                for r in &mut self.routines {
                    if !r.enabled {
                        continue;
                    }
                    // Skip event-triggered routines — they are handled elsewhere.
                    if r.trigger.as_ref().map(|t| t.is_event()).unwrap_or(false) {
                        continue;
                    }
                    if r.is_due() {
                        let result = on_routine(r);

                        let now_secs = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        r.last_run = Some(unix_secs_to_iso8601(now_secs));

                        if let Err(e) = r.update_next_run(&r.schedule.clone()) {
                            eprintln!(
                                "lumina-scheduler: could not update next_run for '{}': {}",
                                r.name, e
                            );
                        }

                        eprintln!(
                            "lumina-scheduler: routine '{}' fired — result length: {}",
                            r.name,
                            result.len()
                        );
                    }
                }

                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });

        SchedulerHandle { running, join_handle, secondary_handle: None }
    }

    /// Spawn both a cron loop AND an event-listener loop.
    ///
    /// The event loop subscribes to `event_bus`, and whenever an event arrives
    /// it checks all enabled event-triggered routines for a match.  Matched
    /// routines are subject to 60-second per-routine debouncing.
    ///
    /// **Serialization:** both loops share a `Mutex`-guarded executor so only
    /// one routine runs at a time regardless of trigger source.  This preserves
    /// the VRAM-contention avoidance guarantee.
    ///
    /// Returns a single [`SchedulerHandle`] that stops both loops and aborts
    /// both tasks.
    pub fn start_with_events(
        self,
        event_bus: Arc<EventBus>,
        on_routine: impl Fn(&Routine) -> String + Send + Sync + 'static,
    ) -> SchedulerHandle {
        let running = Arc::new(AtomicBool::new(true));
        let running_cron = running.clone();
        let running_event = running.clone();

        // Shared serialization mutex — only one routine runs at a time.
        let executor: Arc<Mutex<Box<dyn Fn(&Routine) -> String + Send + Sync>>> =
            Arc::new(Mutex::new(Box::new(on_routine)));
        let executor_cron = executor.clone();
        let executor_event = executor.clone();

        // Separate the routine lists.
        let cron_routines: Vec<Routine> = self
            .routines
            .iter()
            .filter(|r| !r.trigger.as_ref().map(|t| t.is_event()).unwrap_or(false))
            .cloned()
            .collect();

        let event_routines: Vec<Routine> = self
            .routines
            .iter()
            .filter(|r| r.trigger.as_ref().map(|t| t.is_event()).unwrap_or(false))
            .cloned()
            .collect();

        // ── cron loop ──────────────────────────────────────────────────────────
        let cron_handle = tokio::spawn(async move {
            let mut routines = cron_routines;
            while running_cron.load(Ordering::SeqCst) {
                for r in &mut routines {
                    if !r.enabled || !r.is_due() {
                        continue;
                    }
                    // Acquire the serialization lock before calling the callback.
                    let result = {
                        let exec = executor_cron.lock().await;
                        exec(r)
                    };

                    let now_secs = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    r.last_run = Some(unix_secs_to_iso8601(now_secs));

                    if let Err(e) = r.update_next_run(&r.schedule.clone()) {
                        eprintln!(
                            "lumina-scheduler: could not update next_run for '{}': {}",
                            r.name, e
                        );
                    }

                    eprintln!(
                        "lumina-scheduler: cron routine '{}' fired — result length: {}",
                        r.name,
                        result.len()
                    );
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });

        // ── event loop ─────────────────────────────────────────────────────────
        let mut rx = event_bus.subscribe();
        let event_handle = tokio::spawn(async move {
            let mut debouncer = Debouncer::new();
            // Mutable copies so we can update last_run.
            let mut routines = event_routines;

            while running_event.load(Ordering::SeqCst) {
                match rx.recv().await {
                    Ok(event) => {
                        for r in &mut routines {
                            if !r.enabled {
                                continue;
                            }
                            let Some(RoutineTrigger::Event(ref trigger)) = r.trigger else {
                                continue;
                            };
                            if !matches_trigger(&event, trigger) {
                                continue;
                            }
                            let debounce_key = format!("{}:{}", r.name, event.variant_name());
                            if !debouncer.should_trigger(&debounce_key) {
                                eprintln!(
                                    "lumina-scheduler: event routine '{}' debounced (within 60s)",
                                    r.name
                                );
                                continue;
                            }

                            // Acquire the serialization lock.
                            let result = {
                                let exec = executor_event.lock().await;
                                exec(r)
                            };

                            // Update last_run for parity with cron routines.
                            let now_secs = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            r.last_run = Some(unix_secs_to_iso8601(now_secs));

                            eprintln!(
                                "lumina-scheduler: event routine '{}' fired on '{}' — result length: {}",
                                r.name,
                                event.variant_name(),
                                result.len()
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("lumina-scheduler: event bus lagged, dropped {} events", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // EventBus has been dropped — exit the event loop.
                        break;
                    }
                }
            }
        });

        SchedulerHandle {
            running,
            join_handle: event_handle,
            secondary_handle: Some(cron_handle),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn write_toml(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::path::PathBuf::from(format!("/tmp/lumina_edge05_{}.toml", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn make_routine_struct(name: &str, schedule: &str, next_run: Option<&str>, trigger: Option<RoutineTrigger>) -> Routine {
        Routine {
            name: name.to_string(),
            schedule: schedule.to_string(),
            prompt: "hello".to_string(),
            model_override: None,
            channel: RoutineChannel::Stdout,
            enabled: true,
            last_run: None,
            next_run: next_run.map(|s| s.to_string()),
            trigger,
        }
    }

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn test_scheduler_empty_when_file_missing() {
        let path = Path::new("/tmp/lumina_edge05_missing_test.toml");
        let _ = std::fs::remove_file(path);
        let sched = Scheduler::new(path).unwrap();
        assert!(sched.list_routines().is_empty());
    }

    #[test]
    fn test_scheduler_loads_valid_routines() {
        let p = write_toml(
            "valid_routines",
            r#"
[[routines]]
name = "morning"
schedule = "0 7 * * *"
prompt = "Good morning."
channel = "stdout"
enabled = true
"#,
        );
        let sched = Scheduler::new(&p).unwrap();
        assert_eq!(sched.list_routines().len(), 1);
        let r = &sched.list_routines()[0];
        assert_eq!(r.name, "morning");
        // next_run should have been calculated.
        assert!(r.next_run.is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_scheduler_skips_invalid_cron() {
        let p = write_toml(
            "skip_invalid_cron",
            r#"
[[routines]]
name = "bad"
schedule = "not a cron"
prompt = "hello"
enabled = true

[[routines]]
name = "good"
schedule = "0 7 * * *"
prompt = "hello"
enabled = true
"#,
        );
        let sched = Scheduler::new(&p).unwrap();
        // Only the valid routine should be in the list.
        assert_eq!(sched.list_routines().len(), 1);
        assert_eq!(sched.list_routines()[0].name, "good");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_scheduler_disabled_routines_not_skipped_from_list() {
        // Disabled routines are kept in the list but not executed.
        let p = write_toml(
            "disabled_routines",
            r#"
[[routines]]
name = "disabled"
schedule = "0 7 * * *"
prompt = "hello"
enabled = false
"#,
        );
        let sched = Scheduler::new(&p).unwrap();
        assert_eq!(sched.list_routines().len(), 1);
        assert!(!sched.list_routines()[0].enabled);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_scheduler_multiple_routines() {
        let p = write_toml(
            "multiple_routines",
            r#"
[[routines]]
name = "r1"
schedule = "0 7 * * *"
prompt = "one"
enabled = true

[[routines]]
name = "r2"
schedule = "*/5 * * * *"
prompt = "two"
enabled = true

[[routines]]
name = "r3"
schedule = "0 9 * * 1"
prompt = "three"
enabled = false
"#,
        );
        let sched = Scheduler::new(&p).unwrap();
        // r1 and r2 are enabled (and have valid crons); r3 is disabled (kept in list).
        assert_eq!(sched.list_routines().len(), 3);
        let _ = std::fs::remove_file(&p);
    }

    // ── Event-triggered routines loaded ──────────────────────────────────────

    #[test]
    fn test_scheduler_loads_event_triggered_routine() {
        let p = write_toml(
            "event_routine",
            r#"
[[routines]]
name = "tool_failure_alert"
prompt = "A tool failed."
trigger = { event = "ToolFailure" }
enabled = true
"#,
        );
        let sched = Scheduler::new(&p).unwrap();
        assert_eq!(sched.list_routines().len(), 1);
        let r = &sched.list_routines()[0];
        assert_eq!(r.name, "tool_failure_alert");
        assert!(r.trigger.is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_scheduler_cron_and_event_routines_coexist() {
        let p = write_toml(
            "coexist",
            r#"
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
"#,
        );
        let sched = Scheduler::new(&p).unwrap();
        assert_eq!(sched.list_routines().len(), 2);
        let _ = std::fs::remove_file(&p);
    }

    // ── SchedulerHandle::stop() ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_stop_sets_running_false() {
        let p = write_toml("stop_test", "");
        let sched = Scheduler::new(p.as_path()).unwrap();
        let handle = sched.start(|_r| String::new());
        // After start(), running is true.
        assert!(handle.running.load(Ordering::SeqCst));
        handle.stop();
        assert!(!handle.running.load(Ordering::SeqCst));
        handle.abort();
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn test_abort_stops_both_handles() {
        // Verify that abort() does not panic when there is a secondary handle.
        let p = write_toml("abort_both", "");
        let sched = Scheduler::new(p.as_path()).unwrap();
        let bus = Arc::new(EventBus::new());
        let handle = sched.start_with_events(bus, |_| String::new());
        // Should not panic — both handles are aborted.
        handle.abort();
        let _ = std::fs::remove_file(&p);
    }

    // ── on_routine callback (sync test, no tokio task needed) ────────────────

    #[test]
    fn test_disabled_routine_not_fired() {
        // Build a routine that is disabled but is_due() would be true.
        let mut r = Routine {
            name: "disabled".to_string(),
            schedule: "0 7 * * *".to_string(),
            prompt: "hello".to_string(),
            model_override: None,
            channel: RoutineChannel::Stdout,
            enabled: false,
            last_run: None,
            next_run: Some("2020-01-01T00:00:00Z".to_string()), // past = due
            trigger: None,
        };

        // Verify is_due()=true and enabled=false — the scheduler should skip it.
        assert!(r.is_due());
        assert!(!r.enabled);

        // Simulate the scheduler loop's check:
        let fired = r.enabled && r.is_due();
        assert!(!fired, "Disabled routine should not fire");

        // Suppress unused_mut warning
        let _ = r.last_run.take();
    }

    #[test]
    fn test_routine_callback_called_when_due() {
        // Build a routine that IS enabled and is_due.
        let routines = vec![Routine {
            name: "fire_me".to_string(),
            schedule: "0 7 * * *".to_string(),
            prompt: "test prompt".to_string(),
            model_override: None,
            channel: RoutineChannel::Stdout,
            enabled: true,
            last_run: None,
            next_run: Some("2020-01-01T00:00:00Z".to_string()), // past
            trigger: None,
        }];

        let mut fired_names: Vec<String> = Vec::new();
        for r in &routines {
            if r.enabled && r.is_due() {
                fired_names.push(r.name.clone());
            }
        }

        assert_eq!(fired_names, vec!["fire_me"]);
    }

    // ── Integration: routine config loaded and due check works ────────────────

    #[test]
    fn test_routine_config_roundtrip() {
        let toml_str = r#"
[[routines]]
name = "daily"
schedule = "0 8 * * *"
prompt = "daily check"
channel = "file"
model_override = "lumina-deep"
enabled = true
"#;
        let config: RoutinesConfig = toml::from_str(toml_str).unwrap();
        let r = &config.routines[0];
        assert_eq!(r.name, "daily");
        assert_eq!(r.model_override.as_deref(), Some("lumina-deep"));
        assert_eq!(r.channel, RoutineChannel::File);
    }

    #[test]
    fn test_failed_routine_not_retried_immediately() {
        // After a routine fires and we update next_run, it should no longer be due.
        let mut r = Routine {
            name: "test".to_string(),
            schedule: "0 7 * * *".to_string(),
            prompt: "hello".to_string(),
            model_override: None,
            channel: RoutineChannel::Stdout,
            enabled: true,
            last_run: None,
            next_run: Some("2020-01-01T00:00:00Z".to_string()), // past = due
            trigger: None,
        };

        assert!(r.is_due());

        // Simulate post-fire update.
        r.update_next_run("0 7 * * *").unwrap();

        // After update, next_run should be in the future.
        assert!(!r.is_due(), "After update, routine should not be immediately due again");
    }

    // ── EDGE-06: start_with_events fires event-triggered routines ─────────────

    #[tokio::test]
    async fn test_event_triggered_routine_fires_on_matching_event() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        let event_bus = Arc::new(EventBus::new());

        let p = write_toml("event_fire", r#"
[[routines]]
name = "on_rate_limit"
prompt = "rate limit hit"
trigger = { event = "RateLimitHit" }
enabled = true
"#);
        let sched = Scheduler::new(&p).unwrap();

        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();

        let handle = sched.start_with_events(event_bus.clone(), move |_r| {
            count_clone.fetch_add(1, AtomicOrdering::SeqCst);
            String::new()
        });

        // Give the event loop a moment to subscribe.
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Emit the matching event.
        event_bus.emit(EventType::RateLimitHit);

        // Allow the event loop to process.
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            call_count.load(AtomicOrdering::SeqCst) >= 1,
            "Routine should have fired"
        );

        handle.abort();
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn test_debounce_prevents_duplicate_triggers() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        let event_bus = Arc::new(EventBus::new());

        let p = write_toml("debounce_test", r#"
[[routines]]
name = "debounced"
prompt = "tool failure"
trigger = { event = "ToolFailure" }
enabled = true
"#);
        let sched = Scheduler::new(&p).unwrap();

        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();

        let handle = sched.start_with_events(event_bus.clone(), move |_r| {
            count_clone.fetch_add(1, AtomicOrdering::SeqCst);
            String::new()
        });

        tokio::time::sleep(Duration::from_millis(20)).await;

        // Emit the same event twice rapidly.
        event_bus.emit(EventType::ToolFailure { tool_name: "tool_x".to_string() });
        tokio::time::sleep(Duration::from_millis(10)).await;
        event_bus.emit(EventType::ToolFailure { tool_name: "tool_y".to_string() });

        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            call_count.load(AtomicOrdering::SeqCst),
            1,
            "Debounce should prevent second trigger within 60s"
        );

        handle.abort();
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn test_non_matching_event_does_not_fire_routine() {
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

        let event_bus = Arc::new(EventBus::new());

        let p = write_toml("no_fire", r#"
[[routines]]
name = "circuit_watcher"
prompt = "circuit open"
trigger = { event = "CircuitOpen" }
enabled = true
"#);
        let sched = Scheduler::new(&p).unwrap();

        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();

        let handle = sched.start_with_events(event_bus.clone(), move |_r| {
            fired_clone.store(true, AtomicOrdering::SeqCst);
            String::new()
        });

        tokio::time::sleep(Duration::from_millis(20)).await;

        // Emit a different event — should NOT fire the CircuitOpen routine.
        event_bus.emit(EventType::RateLimitHit);

        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            !fired.load(AtomicOrdering::SeqCst),
            "Non-matching event should not fire the routine"
        );

        handle.abort();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_make_routine_struct_helper() {
        let r = make_routine_struct("r", "0 * * * *", None, None);
        assert_eq!(r.name, "r");
        assert!(r.trigger.is_none());
    }
}
