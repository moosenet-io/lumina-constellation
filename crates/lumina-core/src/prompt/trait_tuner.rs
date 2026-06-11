//! DPROMPT-02: Sleep-time trait self-tuning.
//!
//! Engagement signals detected per-turn by the
//! [`super::engagement::EngagementAnalyzer`] are buffered here and applied to a
//! user's [`TraitVector`] during nightly sleep-time consolidation — never
//! immediately, so noise from a single turn cannot swing the personality.
//!
//! The consolidation pipeline is:
//! 1. Average all of the day's signal deltas into one mean adjustment.
//! 2. Push that mean onto a 7-day rolling history (persisted in
//!    `signal-history.json` next to the trait vector).
//! 3. Compute an **exponentially-weighted** average over the window (most
//!    recent day weighted highest, decay `0.85`/day) so one unusual day cannot
//!    dominate.
//! 4. Enforce a **minimum of 5 signals** for the day — below that the day is
//!    too sparse to be meaningful and consolidation is skipped (the buffer is
//!    still cleared).
//! 5. Apply the weighted adjustment to the trait vector, clamp to the soft
//!    bounds, persist, and log the change.
//!
//! All paths are caller-supplied, so the tuner is inherently per-user (each
//! user's trait vector and history live under their own layer directory) and
//! fully testable with `tempfile::tempdir()`.  No LLM, no network, no clock —
//! timestamps are injected by the caller as unix seconds.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::engagement::{EngagementSignal, TraitDeltas};
use super::traits::{clamp_trait, TraitVector};
use crate::error::Result;

/// Minimum signals accumulated in a day before consolidation will adjust.
pub const MIN_DAILY_SIGNALS: usize = 5;
/// Per-day decay factor for the exponentially-weighted rolling window.
pub const DAILY_DECAY: f32 = 0.85;
/// Length of the rolling window, in days.
pub const WINDOW_DAYS: usize = 7;

/// One day's consolidated adjustment, kept in the rolling history.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct DailyEntry {
    /// Unix-seconds timestamp of the consolidation that produced this entry.
    ts: i64,
    flair: f32,
    spontaneity: f32,
    humor: f32,
    focus: f32,
}

impl DailyEntry {
    fn deltas(&self) -> TraitDeltas {
        TraitDeltas { flair: self.flair, spontaneity: self.spontaneity, humor: self.humor, focus: self.focus }
    }
}

/// Persisted 7-day rolling window of consolidated daily adjustments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SignalHistory {
    /// Oldest first, newest last; trimmed to [`WINDOW_DAYS`] on update.
    days: Vec<DailyEntry>,
}

impl SignalHistory {
    fn load(path: &Path) -> SignalHistory {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                log::warn!("signal-history.json corrupt ({e}); starting fresh");
                SignalHistory::default()
            }),
            Err(_) => SignalHistory::default(),
        }
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, json)
    }

    /// Append a day and keep only the most recent [`WINDOW_DAYS`].
    fn push_day(&mut self, entry: DailyEntry) {
        self.days.push(entry);
        if self.days.len() > WINDOW_DAYS {
            let drop = self.days.len() - WINDOW_DAYS;
            self.days.drain(0..drop);
        }
    }

    /// Exponentially-weighted average over the window.
    ///
    /// The newest entry (last in `days`) gets weight `1.0`; each older entry is
    /// multiplied by an extra factor of [`DAILY_DECAY`].  The result is the
    /// weighted mean of the per-day deltas.
    fn weighted_adjustment(&self) -> TraitDeltas {
        if self.days.is_empty() {
            return TraitDeltas::default();
        }
        let mut acc = TraitDeltas::default();
        let mut weight_sum = 0.0f32;
        let n = self.days.len();
        for (i, day) in self.days.iter().enumerate() {
            // i = 0 is oldest, i = n-1 is newest.
            let age = (n - 1 - i) as i32; // 0 for newest
            let w = DAILY_DECAY.powi(age);
            acc = acc.add(day.deltas().scale(w));
            weight_sum += w;
        }
        if weight_sum == 0.0 {
            TraitDeltas::default()
        } else {
            acc.scale(1.0 / weight_sum)
        }
    }
}

/// Outcome of a [`TraitTuner::consolidate_daily`] call.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsolidationOutcome {
    /// Fewer than [`MIN_DAILY_SIGNALS`] signals — nothing applied.
    Skipped { signal_count: usize },
    /// Traits were adjusted; carries before/after for logging/audit.
    Applied {
        signal_count: usize,
        before: TraitVector,
        after: TraitVector,
        applied: TraitDeltas,
    },
}

/// Buffers per-turn engagement signals and applies them nightly.
///
/// The daily buffer is in-memory; the 7-day rolling window is persisted to
/// `signal-history.json` alongside the trait vector so it survives restarts.
#[derive(Debug, Default)]
pub struct TraitTuner {
    /// Accumulated deltas for today (one entry per recorded signal).
    buffer: Vec<TraitDeltas>,
}

impl TraitTuner {
    pub fn new() -> Self {
        TraitTuner { buffer: Vec::new() }
    }

    /// Number of signals currently buffered for today.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Record the signals detected for one turn.
    ///
    /// `timestamp_secs` is accepted (and currently used only to make the API
    /// future-proof / explicit about determinism); buffering is per-day and the
    /// caller invokes [`consolidate_daily`](Self::consolidate_daily) once per
    /// day, so the timestamp does not need to bucket here.
    pub fn record_signal(&mut self, signals: &[EngagementSignal], _timestamp_secs: i64) {
        for s in signals {
            self.buffer.push(s.deltas());
        }
    }

    /// Average all buffered deltas for the day (mean per trait).
    fn daily_average(&self) -> TraitDeltas {
        if self.buffer.is_empty() {
            return TraitDeltas::default();
        }
        let mut acc = TraitDeltas::default();
        for d in &self.buffer {
            acc = acc.add(*d);
        }
        acc.scale(1.0 / self.buffer.len() as f32)
    }

    /// Path of the rolling-window history file, beside the trait vector.
    fn history_path(trait_path: &Path) -> PathBuf {
        let dir = trait_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        dir.join("signal-history.json")
    }

    /// Nightly sleep-time consolidation.
    ///
    /// Steps (see module docs): enforce min-signal threshold, average today's
    /// deltas, push onto the persisted 7-day window, compute the
    /// exponentially-weighted adjustment, apply + clamp + persist the trait
    /// vector, log the change, and clear the daily buffer.  The buffer is
    /// **always** cleared, even when the threshold is not met, so a sparse day
    /// does not bleed into the next.
    pub fn consolidate_daily(
        &mut self,
        trait_path: &Path,
        timestamp_secs: i64,
    ) -> Result<ConsolidationOutcome> {
        let signal_count = self.buffer.len();

        if signal_count < MIN_DAILY_SIGNALS {
            log::info!(
                "trait consolidation skipped: {signal_count} signals (< {MIN_DAILY_SIGNALS} threshold)"
            );
            self.buffer.clear();
            return Ok(ConsolidationOutcome::Skipped { signal_count });
        }

        let mean = self.daily_average();

        // Update the persisted rolling window.
        let hist_path = Self::history_path(trait_path);
        let mut history = SignalHistory::load(&hist_path);
        history.push_day(DailyEntry {
            ts: timestamp_secs,
            flair: mean.flair,
            spontaneity: mean.spontaneity,
            humor: mean.humor,
            focus: mean.focus,
        });
        history.save(&hist_path)?;

        let adj = history.weighted_adjustment();

        // Apply to the trait vector and clamp.
        let before = TraitVector::load(trait_path);
        let after = TraitVector {
            flair: clamp_trait(before.flair + adj.flair),
            spontaneity: clamp_trait(before.spontaneity + adj.spontaneity),
            humor: clamp_trait(before.humor + adj.humor),
            focus: clamp_trait(before.focus + adj.focus),
        };
        after.save(trait_path)?;

        log::info!(
            "Trait adjustment ({signal_count} signals): \
             flair {:.2}→{:.2} spontaneity {:.2}→{:.2} humor {:.2}→{:.2} focus {:.2}→{:.2}",
            before.flair, after.flair,
            before.spontaneity, after.spontaneity,
            before.humor, after.humor,
            before.focus, after.focus,
        );

        self.buffer.clear();
        Ok(ConsolidationOutcome::Applied { signal_count, before, after, applied: adj })
    }
}

#[cfg(test)]
mod tests {
    use super::EngagementSignal::*;
    use super::*;
    use tempfile::tempdir;

    fn trait_path(dir: &Path) -> PathBuf {
        dir.join("operator").join("trait-vector.json")
    }

    /// Record `n` copies of a single signal type.
    fn record_n(tuner: &mut TraitTuner, sig: EngagementSignal, n: usize) {
        for _ in 0..n {
            tuner.record_signal(&[sig], 1_700_000_000);
        }
    }

    #[test]
    fn below_threshold_skips_and_clears_buffer() {
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        TraitVector::default().save(&tp).unwrap();

        let mut tuner = TraitTuner::new();
        record_n(&mut tuner, Laughter, 4); // < 5
        let out = tuner.consolidate_daily(&tp, 1_700_000_000).unwrap();
        assert_eq!(out, ConsolidationOutcome::Skipped { signal_count: 4 });
        assert_eq!(tuner.buffered(), 0, "buffer must clear even when skipped");
        // Trait vector unchanged.
        assert_eq!(TraitVector::load(&tp), TraitVector::default());
    }

    #[test]
    fn daily_averaging_updates_json() {
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        TraitVector::default().save(&tp).unwrap();

        let mut tuner = TraitTuner::new();
        // 5 ExplicitPositive: each +0.01 across all traits; mean = +0.01 each.
        record_n(&mut tuner, ExplicitPositive, 5);
        let out = tuner.consolidate_daily(&tp, 1_700_000_000).unwrap();

        let after = TraitVector::load(&tp);
        // Single day in window → weighted == daily mean (+0.01).
        assert!((after.humor - 0.66).abs() < 1e-4, "humor: {}", after.humor);
        assert!((after.focus - 0.76).abs() < 1e-4, "focus: {}", after.focus);
        match out {
            ConsolidationOutcome::Applied { signal_count, .. } => assert_eq!(signal_count, 5),
            _ => panic!("expected Applied"),
        }
    }

    #[test]
    fn buffer_cleared_after_consolidation() {
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        TraitVector::default().save(&tp).unwrap();
        let mut tuner = TraitTuner::new();
        record_n(&mut tuner, Laughter, 6);
        assert_eq!(tuner.buffered(), 6);
        tuner.consolidate_daily(&tp, 1).unwrap();
        assert_eq!(tuner.buffered(), 0);
    }

    #[test]
    fn min_5_threshold_boundary() {
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        TraitVector::default().save(&tp).unwrap();
        let mut tuner = TraitTuner::new();
        record_n(&mut tuner, Laughter, 5); // exactly 5 → applied
        let out = tuner.consolidate_daily(&tp, 1).unwrap();
        assert!(matches!(out, ConsolidationOutcome::Applied { .. }));
    }

    #[test]
    fn seven_day_exponential_weighting() {
        // Construct a history file by hand and verify the weighted average.
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        std::fs::create_dir_all(tp.parent().unwrap()).unwrap();
        let hist_path = TraitTuner::history_path(&tp);

        // Two days: older day humor +1.0, newer day humor 0.0.
        let history = SignalHistory {
            days: vec![
                DailyEntry { ts: 1, flair: 0.0, spontaneity: 0.0, humor: 1.0, focus: 0.0 },
                DailyEntry { ts: 2, flair: 0.0, spontaneity: 0.0, humor: 0.0, focus: 0.0 },
            ],
        };
        history.save(&hist_path).unwrap();

        // Newest weight 1.0, older weight 0.85.  Weighted mean of humor:
        // (0.0*1.0 + 1.0*0.85) / (1.0 + 0.85) = 0.85/1.85 ≈ 0.4594.
        let adj = SignalHistory::load(&hist_path).weighted_adjustment();
        assert!((adj.humor - (0.85 / 1.85)).abs() < 1e-4, "humor adj: {}", adj.humor);
    }

    #[test]
    fn window_trims_to_seven_days() {
        let mut h = SignalHistory::default();
        for i in 0..10 {
            h.push_day(DailyEntry { ts: i, flair: 0.0, spontaneity: 0.0, humor: 0.0, focus: 0.0 });
        }
        assert_eq!(h.days.len(), WINDOW_DAYS);
        // Oldest retained is ts=3 (0,1,2 dropped).
        assert_eq!(h.days.first().unwrap().ts, 3);
        assert_eq!(h.days.last().unwrap().ts, 9);
    }

    #[test]
    fn clamps_to_upper_bound() {
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        // Start near the top so repeated positive days push past 0.85.
        TraitVector { flair: 0.84, spontaneity: 0.84, humor: 0.84, focus: 0.84 }.save(&tp).unwrap();
        let mut tuner = TraitTuner::new();
        // Many days of strong positive humor signals.
        for day in 0..7 {
            record_n(&mut tuner, Laughter, 20);
            tuner.consolidate_daily(&tp, day).unwrap();
        }
        let after = TraitVector::load(&tp);
        assert!(after.humor <= super::super::traits::TRAIT_MAX + 1e-6);
        assert!((after.humor - super::super::traits::TRAIT_MAX).abs() < 1e-6, "humor: {}", after.humor);
    }

    #[test]
    fn clamps_to_lower_bound() {
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        TraitVector { flair: 0.16, spontaneity: 0.16, humor: 0.16, focus: 0.40 }.save(&tp).unwrap();
        let mut tuner = TraitTuner::new();
        for day in 0..7 {
            // ExplicitNegative pushes flair/spontaneity/humor down.
            record_n(&mut tuner, ExplicitNegative, 20);
            tuner.consolidate_daily(&tp, day).unwrap();
        }
        let after = TraitVector::load(&tp);
        assert!(after.humor >= super::super::traits::TRAIT_MIN - 1e-6);
        assert!((after.humor - super::super::traits::TRAIT_MIN).abs() < 1e-6, "humor: {}", after.humor);
    }

    #[test]
    fn ten_positive_humor_signals_increase_humor() {
        // Integration-style: drive the analyzer end-to-end into the tuner.
        use super::super::engagement::EngagementAnalyzer;
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        TraitVector::default().save(&tp).unwrap();
        let before = TraitVector::load(&tp);

        let mut tuner = TraitTuner::new();
        for _ in 0..10 {
            let signals = EngagementAnalyzer::analyze_turn(
                "haha that's hilarious 😂",
                "Here's the report.",
                Some("previous turn"),
            );
            assert!(signals.contains(&Laughter));
            tuner.record_signal(&signals, 1_700_000_000);
        }
        tuner.consolidate_daily(&tp, 1_700_000_000).unwrap();

        let after = TraitVector::load(&tp);
        assert!(after.humor > before.humor, "humor {} should exceed {}", after.humor, before.humor);
    }

    #[test]
    fn per_user_histories_are_independent() {
        let dir = tempdir().unwrap();
        let alice = dir.path().join("alice").join("trait-vector.json");
        let bob = dir.path().join("bob").join("trait-vector.json");
        TraitVector::default().save(&alice).unwrap();
        TraitVector::default().save(&bob).unwrap();

        let mut ta = TraitTuner::new();
        record_n(&mut ta, Laughter, 10);
        ta.consolidate_daily(&alice, 1).unwrap();

        // Bob untouched.
        assert_eq!(TraitVector::load(&bob), TraitVector::default());
        assert!(TraitVector::load(&alice).humor > 0.65);
        // Histories live in separate dirs.
        assert!(dir.path().join("alice").join("signal-history.json").exists());
        assert!(!dir.path().join("bob").join("signal-history.json").exists());
    }

    #[test]
    fn history_persists_across_tuner_instances() {
        let dir = tempdir().unwrap();
        let tp = trait_path(dir.path());
        TraitVector::default().save(&tp).unwrap();

        let mut t1 = TraitTuner::new();
        record_n(&mut t1, Laughter, 10);
        t1.consolidate_daily(&tp, 1).unwrap();

        // A fresh tuner (e.g. after restart) sees the persisted window.
        let mut t2 = TraitTuner::new();
        record_n(&mut t2, Laughter, 10);
        t2.consolidate_daily(&tp, 2).unwrap();

        let hist = SignalHistory::load(&TraitTuner::history_path(&tp));
        assert_eq!(hist.days.len(), 2);
    }
}
