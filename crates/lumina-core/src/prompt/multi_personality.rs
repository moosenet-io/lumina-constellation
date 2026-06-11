//! DPROMPT-17: Shared personality with per-user modifiers.
//!
//! Lumina has ONE personality that adjusts how she relates to different
//! people — not a separate personality per user.  This is modelled as a
//! two-level trait system layered on top of the single-user [`TraitVector`]
//! contract (see [`super::traits`], which this module composes and never
//! modifies):
//!
//! * **`base_traits`** (shared): Lumina's core personality.  Starts at the
//!   locked-in [`TraitVector::default`] values.  Self-tunes from the **admin**
//!   user's engagement signals — the admin defines who Lumina *is*.  Stored
//!   once, shared by everyone, at `{root}/base-traits.json`.
//! * **`user_modifier`** (per-user): a small per-field offset applied to the
//!   base for one user.  Starts at all-zero (no modification).  Self-tunes
//!   from that user's own engagement signals.  Stored at
//!   `{root}/{user_id}/trait-modifier.json`.
//! * **`effective_traits(user) = clamp(base + modifier, 0.15, 0.85)`** —
//!   computed per-turn for prompt assembly.
//!
//! The **Core Identity, Opinions, and the Knowledge Digest about Lumina
//! herself remain shared** — there are no per-user copies.  That contract is
//! already enforced by the assembler ([`super::PromptAssembler`] reads
//! `core-identity.txt` from the shared root and never writes a per-user copy;
//! see the `dprompt09_*` tests in `super`).  Only the *trait* layer becomes
//! two-level here; identity and opinions are untouched.
//!
//! ## Tuning split (admin vs. modifier)
//! The nightly trait tuner (DPROMPT-02, [`super::trait_tuner`]) currently
//! consolidates a day's engagement signals into a single [`TraitVector`].
//! Under this model that consolidation routes by user role:
//! * an **admin** user's consolidated deltas are applied to the shared base
//!   via [`SharedPersonality::apply_signal_deltas_to_base`];
//! * a **non-admin** user's consolidated deltas are applied only to that
//!   user's modifier via [`SharedPersonality::apply_signal_deltas_to_modifier`].
//!
//! No LLM, no network, no clock — every path is caller-supplied and the type
//! is fully testable with `tempfile::tempdir()`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::traits::{clamp_trait, TraitVector};

/// Filename of the shared base trait vector, stored at the layers root.
pub const BASE_TRAITS_FILE: &str = "base-traits.json";
/// Filename of a user's per-user trait modifier, stored in their layer dir.
pub const TRAIT_MODIFIER_FILE: &str = "trait-modifier.json";

/// A per-user offset applied to the shared base traits.
///
/// Every field starts at `0.0` (no modification) and shifts the corresponding
/// base trait up or down for one user.  The sum is clamped to the soft bounds
/// when computing [`SharedPersonality::effective_traits`], so a modifier can
/// never produce an out-of-range trait.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TraitModifier {
    /// Offset applied to [`TraitVector::flair`].
    pub flair: f32,
    /// Offset applied to [`TraitVector::spontaneity`].
    pub spontaneity: f32,
    /// Offset applied to [`TraitVector::humor`].
    pub humor: f32,
    /// Offset applied to [`TraitVector::focus`].
    pub focus: f32,
}

impl Default for TraitModifier {
    /// No modification — the user sees the shared base unchanged.
    fn default() -> Self {
        TraitModifier { flair: 0.0, spontaneity: 0.0, humor: 0.0, focus: 0.0 }
    }
}

impl TraitModifier {
    /// Component-wise sum of two modifiers (used when accumulating deltas).
    fn add(self, other: TraitModifier) -> TraitModifier {
        TraitModifier {
            flair: self.flair + other.flair,
            spontaneity: self.spontaneity + other.spontaneity,
            humor: self.humor + other.humor,
            focus: self.focus + other.focus,
        }
    }

    /// Load a modifier from `path`.
    ///
    /// On any failure (missing file, unreadable, corrupt JSON) returns the
    /// all-zero default — so a fresh or broken modifier file always degrades
    /// to "no modification" rather than failing prompt assembly.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<TraitModifier>(&s) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("trait-modifier.json corrupt ({e}); resetting to zero");
                    TraitModifier::default()
                }
            },
            Err(_) => TraitModifier::default(),
        }
    }

    /// Persist this modifier to `path`, creating parent dirs.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, json)
    }
}

/// Two-level personality: a shared base plus per-user modifiers.
///
/// This is a stateless namespace over the on-disk layout; every method takes
/// an explicit `root` (the layers root) so it is inherently per-deployment and
/// trivially testable.
pub struct SharedPersonality;

impl SharedPersonality {
    /// Path to the shared base trait vector: `{root}/base-traits.json`.
    pub fn base_path(root: &Path) -> PathBuf {
        root.join(BASE_TRAITS_FILE)
    }

    /// Path to a user's modifier: `{root}/{user_id}/trait-modifier.json`.
    ///
    /// `user_id` is sanitised exactly as the assembler sanitises per-user
    /// directories so the two always agree on the layout.
    pub fn modifier_path(root: &Path, user_id: &str) -> PathBuf {
        root.join(sanitize_user(user_id)).join(TRAIT_MODIFIER_FILE)
    }

    /// Load the shared base traits (default [`TraitVector`] when absent).
    ///
    /// Reuses [`TraitVector::load`], so values are clamped and a missing or
    /// corrupt file falls back to the locked-in personality.
    pub fn base_traits(root: &Path) -> TraitVector {
        TraitVector::load(&Self::base_path(root))
    }

    /// Persist the shared base traits (clamped) to `{root}/base-traits.json`.
    pub fn save_base(root: &Path, base: &TraitVector) -> std::io::Result<()> {
        base.save(&Self::base_path(root))
    }

    /// Load a user's modifier (all-zero when absent).
    pub fn user_modifier(root: &Path, user_id: &str) -> TraitModifier {
        TraitModifier::load(&Self::modifier_path(root, user_id))
    }

    /// Persist a user's modifier.
    pub fn save_modifier(root: &Path, user_id: &str, modifier: &TraitModifier) -> std::io::Result<()> {
        modifier.save(&Self::modifier_path(root, user_id))
    }

    /// Effective traits for `user_id`: `clamp(base + modifier)` per field.
    ///
    /// This is what the assembler's `[style]` layer renders for a given user.
    /// With no modifier file (single-user / brand-new user) the modifier is
    /// all-zero, so `effective == base`.
    pub fn effective_traits(root: &Path, user_id: &str) -> TraitVector {
        let base = Self::base_traits(root);
        let m = Self::user_modifier(root, user_id);
        TraitVector {
            flair: clamp_trait(base.flair + m.flair),
            spontaneity: clamp_trait(base.spontaneity + m.spontaneity),
            humor: clamp_trait(base.humor + m.humor),
            focus: clamp_trait(base.focus + m.focus),
        }
    }

    /// Apply consolidated engagement deltas to the **shared base** traits.
    ///
    /// Called by the trait tuner for the **admin** user — the admin's signals
    /// define who Lumina is for everyone.  The base is clamped via
    /// [`TraitVector::save`] and the persisted vector is returned.
    pub fn apply_signal_deltas_to_base(
        root: &Path,
        deltas: TraitModifier,
    ) -> std::io::Result<TraitVector> {
        let base = Self::base_traits(root);
        let tuned = TraitVector {
            flair: base.flair + deltas.flair,
            spontaneity: base.spontaneity + deltas.spontaneity,
            humor: base.humor + deltas.humor,
            focus: base.focus + deltas.focus,
        }
        .clamped();
        Self::save_base(root, &tuned)?;
        Ok(tuned)
    }

    /// Apply consolidated engagement deltas to a **user's modifier** only.
    ///
    /// Called by the trait tuner for **non-admin** users — their signals only
    /// adjust how Lumina relates to them, never the shared base.  The new
    /// modifier is persisted and returned (modifiers are not clamped; the
    /// soft bounds are enforced later, on the *sum*, in
    /// [`effective_traits`](Self::effective_traits)).
    pub fn apply_signal_deltas_to_modifier(
        root: &Path,
        user_id: &str,
        deltas: TraitModifier,
    ) -> std::io::Result<TraitModifier> {
        let updated = Self::user_modifier(root, user_id).add(deltas);
        Self::save_modifier(root, user_id, &updated)?;
        Ok(updated)
    }
}

/// Keep user ids filesystem-safe — identical rule to [`super::sanitize_user`]
/// so this module and the assembler always resolve the same directory.
fn sanitize_user(user_id: &str) -> String {
    let cleaned: String = user_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() { "default".to_string() } else { cleaned }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::traits::{TRAIT_MAX, TRAIT_MIN};
    use tempfile::tempdir;

    #[test]
    fn modifier_default_is_all_zero() {
        let m = TraitModifier::default();
        assert_eq!(m.flair, 0.0);
        assert_eq!(m.spontaneity, 0.0);
        assert_eq!(m.humor, 0.0);
        assert_eq!(m.focus, 0.0);
    }

    #[test]
    fn base_load_missing_returns_default() {
        let dir = tempdir().unwrap();
        assert_eq!(SharedPersonality::base_traits(dir.path()), TraitVector::default());
    }

    #[test]
    fn base_save_then_load_roundtrip_clamps() {
        let dir = tempdir().unwrap();
        let base = TraitVector { flair: 2.0, spontaneity: 0.55, humor: 0.65, focus: 0.75 };
        SharedPersonality::save_base(dir.path(), &base).unwrap();
        let loaded = SharedPersonality::base_traits(dir.path());
        assert_eq!(loaded.flair, TRAIT_MAX); // clamped on save
        assert_eq!(loaded.spontaneity, 0.55);
        assert!(dir.path().join(BASE_TRAITS_FILE).exists());
    }

    #[test]
    fn modifier_load_missing_returns_zero() {
        let dir = tempdir().unwrap();
        assert_eq!(
            SharedPersonality::user_modifier(dir.path(), "guest"),
            TraitModifier::default()
        );
    }

    #[test]
    fn modifier_save_then_load_roundtrip() {
        let dir = tempdir().unwrap();
        let m = TraitModifier { flair: -0.05, spontaneity: 0.0, humor: 0.10, focus: -0.15 };
        SharedPersonality::save_modifier(dir.path(), "partner", &m).unwrap();
        let loaded = SharedPersonality::user_modifier(dir.path(), "partner");
        assert_eq!(loaded, m);
        assert!(dir.path().join("partner").join(TRAIT_MODIFIER_FILE).exists());
    }

    #[test]
    fn modifier_load_corrupt_returns_zero() {
        let dir = tempdir().unwrap();
        let p = SharedPersonality::modifier_path(dir.path(), "x");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(TraitModifier::load(&p), TraitModifier::default());
    }

    #[test]
    fn effective_is_base_plus_modifier() {
        let dir = tempdir().unwrap();
        let base = TraitVector { flair: 0.70, spontaneity: 0.55, humor: 0.65, focus: 0.75 };
        SharedPersonality::save_base(dir.path(), &base).unwrap();
        let m = TraitModifier { flair: -0.10, spontaneity: 0.05, humor: 0.10, focus: 0.0 };
        SharedPersonality::save_modifier(dir.path(), "partner", &m).unwrap();
        let eff = SharedPersonality::effective_traits(dir.path(), "partner");
        assert!((eff.flair - 0.60).abs() < 1e-6);
        assert!((eff.spontaneity - 0.60).abs() < 1e-6);
        assert!((eff.humor - 0.75).abs() < 1e-6);
        assert!((eff.focus - 0.75).abs() < 1e-6);
    }

    #[test]
    fn single_user_no_modifier_effective_equals_base() {
        let dir = tempdir().unwrap();
        let base = TraitVector { flair: 0.42, spontaneity: 0.55, humor: 0.65, focus: 0.30 };
        SharedPersonality::save_base(dir.path(), &base).unwrap();
        // No modifier file written for "operator".
        let eff = SharedPersonality::effective_traits(dir.path(), "operator");
        assert_eq!(eff, base);
    }

    #[test]
    fn modifier_pushing_past_upper_bound_clamps() {
        let dir = tempdir().unwrap();
        let base = TraitVector { flair: 0.80, spontaneity: 0.80, humor: 0.80, focus: 0.80 };
        SharedPersonality::save_base(dir.path(), &base).unwrap();
        let m = TraitModifier { flair: 0.50, spontaneity: 0.50, humor: 0.50, focus: 0.50 };
        SharedPersonality::save_modifier(dir.path(), "loud", &m).unwrap();
        let eff = SharedPersonality::effective_traits(dir.path(), "loud");
        assert_eq!(eff.flair, TRAIT_MAX);
        assert_eq!(eff.humor, TRAIT_MAX);
        assert_eq!(eff.focus, TRAIT_MAX);
    }

    #[test]
    fn modifier_pushing_past_lower_bound_clamps() {
        let dir = tempdir().unwrap();
        let base = TraitVector { flair: 0.20, spontaneity: 0.20, humor: 0.20, focus: 0.50 };
        SharedPersonality::save_base(dir.path(), &base).unwrap();
        // Guest prefers a more professional tone → strong negative modifiers;
        // focus pushed past the upper bound (0.50 + 0.50 = 1.0 → clamp 0.85).
        let m = TraitModifier { flair: -0.50, spontaneity: -0.50, humor: -0.50, focus: 0.50 };
        SharedPersonality::save_modifier(dir.path(), "guest", &m).unwrap();
        let eff = SharedPersonality::effective_traits(dir.path(), "guest");
        assert_eq!(eff.flair, TRAIT_MIN);
        assert_eq!(eff.humor, TRAIT_MIN);
        assert_eq!(eff.focus, TRAIT_MAX);
    }

    #[test]
    fn per_user_modifiers_are_isolated() {
        let dir = tempdir().unwrap();
        let base = TraitVector::default();
        SharedPersonality::save_base(dir.path(), &base).unwrap();
        SharedPersonality::save_modifier(
            dir.path(),
            "partner",
            &TraitModifier { humor: 0.10, ..Default::default() },
        )
        .unwrap();
        SharedPersonality::save_modifier(
            dir.path(),
            "guest",
            &TraitModifier { humor: -0.15, ..Default::default() },
        )
        .unwrap();

        // Admin (the operator) has no modifier → effective humor = base humor.
        let operator = SharedPersonality::effective_traits(dir.path(), "operator");
        let partner = SharedPersonality::effective_traits(dir.path(), "partner");
        let guest = SharedPersonality::effective_traits(dir.path(), "guest");
        assert!((operator.humor - 0.65).abs() < 1e-6);
        assert!((partner.humor - 0.75).abs() < 1e-6);
        assert!((guest.humor - 0.50).abs() < 1e-6);
        // One user's modifier never bleeds into another.
        assert!((partner.flair - base.flair).abs() < 1e-6);
    }

    #[test]
    fn admin_deltas_update_shared_base() {
        let dir = tempdir().unwrap();
        // Admin engagement pushes humor up over time.
        let tuned = SharedPersonality::apply_signal_deltas_to_base(
            dir.path(),
            TraitModifier { humor: 0.05, ..Default::default() },
        )
        .unwrap();
        assert!((tuned.humor - 0.70).abs() < 1e-6); // 0.65 base + 0.05
        // Persisted: a later non-admin read sees the new base.
        let reloaded = SharedPersonality::base_traits(dir.path());
        assert!((reloaded.humor - 0.70).abs() < 1e-6);
    }

    #[test]
    fn base_deltas_clamp_to_bounds() {
        let dir = tempdir().unwrap();
        SharedPersonality::save_base(
            dir.path(),
            &TraitVector { flair: 0.84, spontaneity: 0.55, humor: 0.65, focus: 0.75 },
        )
        .unwrap();
        let tuned = SharedPersonality::apply_signal_deltas_to_base(
            dir.path(),
            TraitModifier { flair: 0.50, ..Default::default() },
        )
        .unwrap();
        assert_eq!(tuned.flair, TRAIT_MAX);
    }

    #[test]
    fn nonadmin_deltas_update_only_modifier_not_base() {
        let dir = tempdir().unwrap();
        let base_before = SharedPersonality::base_traits(dir.path());
        let updated = SharedPersonality::apply_signal_deltas_to_modifier(
            dir.path(),
            "partner",
            TraitModifier { humor: 0.10, ..Default::default() },
        )
        .unwrap();
        assert!((updated.humor - 0.10).abs() < 1e-6);
        // Applying again accumulates onto the existing modifier.
        let updated2 = SharedPersonality::apply_signal_deltas_to_modifier(
            dir.path(),
            "partner",
            TraitModifier { humor: 0.05, ..Default::default() },
        )
        .unwrap();
        assert!((updated2.humor - 0.15).abs() < 1e-6);
        // Base is untouched by non-admin signals.
        assert_eq!(SharedPersonality::base_traits(dir.path()), base_before);
    }

    #[test]
    fn modifier_path_matches_assembler_sanitisation() {
        let dir = tempdir().unwrap();
        // Matrix-style id must resolve under the same sanitised dir name the
        // assembler uses (see super::sanitize_user tests).
        let p = SharedPersonality::modifier_path(dir.path(), "@operator:example.com");
        assert!(p.ends_with(
            std::path::Path::new("_operator_example_com").join(TRAIT_MODIFIER_FILE)
        ));
    }
}
