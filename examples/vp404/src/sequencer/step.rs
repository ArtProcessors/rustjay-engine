//! One sequencer step: active gate, velocity, probability, ratchet, gate length.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single step in a sequence.
///
/// Every field is defaulted and skipped when pristine, so an untouched step
/// serializes as `{}` — with 64 steps × 16 tracks × N patterns riding in
/// every preset's plugin_state blob, verbatim steps were ~2 MB of defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Whether this step is active (contains a trigger).
    #[serde(default, skip_serializing_if = "is_false")]
    pub active: bool,
    /// Velocity/intensity (0.0 - 1.0).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub velocity: f32,
    /// Probability of trigger (0.0 - 1.0).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub probability: f32,
    /// Number of ratchet repeats (1-8).
    #[serde(default = "one_u8", skip_serializing_if = "is_one_u8")]
    pub ratchet: u8,
    /// Time between ratchets as fraction of step duration.
    #[serde(default = "half", skip_serializing_if = "is_half")]
    pub ratchet_spacing: f32,
    /// Gate length measured in steps: values <1 are a short gate within one
    /// step, values >1 tie the gate across several steps (drag to extend).
    #[serde(default = "default_gate_length", skip_serializing_if = "is_default_gate")]
    pub gate_length: f32,
    /// Per-step parameter locks (reserved for future use).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parameter_locks: HashMap<String, f32>,
}

fn default_gate_length() -> f32 {
    0.25
}

fn one() -> f32 {
    1.0
}
fn one_u8() -> u8 {
    1
}
fn half() -> f32 {
    0.5
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(v: &bool) -> bool {
    !*v
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(v: &f32) -> bool {
    *v == 1.0
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one_u8(v: &u8) -> bool {
    *v == 1
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_half(v: &f32) -> bool {
    *v == 0.5
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_gate(v: &f32) -> bool {
    *v == default_gate_length()
}

impl Step {
    pub fn new() -> Self {
        Self {
            active: false,
            velocity: 1.0,
            probability: 1.0,
            ratchet: 1,
            ratchet_spacing: 0.5,
            gate_length: default_gate_length(),
            parameter_locks: HashMap::new(),
        }
    }

    pub fn active() -> Self {
        Self {
            active: true,
            ..Self::new()
        }
    }

    pub fn toggle(&mut self) {
        self.active = !self.active;
    }

    pub fn should_trigger(&self) -> bool {
        if !self.active {
            return false;
        }
        if self.probability >= 1.0 {
            return true;
        }
        rand::random::<f32>() < self.probability
    }
}

impl Default for Step {
    fn default() -> Self {
        Self::new()
    }
}

/// Tiny deterministic RNG for probability checks.
pub mod rand {
    use std::cell::Cell;

    thread_local! {
        static RNG: Cell<u64> = const { Cell::new(0x123456789abcdef0) };
    }

    pub fn random<T>() -> T
    where
        T: Random,
    {
        T::random()
    }

    pub trait Random {
        fn random() -> Self;
    }

    impl Random for f32 {
        fn random() -> Self {
            RNG.with(|rng| {
                let old = rng.get();
                let mut x = old;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                rng.set(x);
                (x as f64 / u64::MAX as f64) as f32
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_toggles() {
        let mut s = Step::new();
        assert!(!s.active);
        s.toggle();
        assert!(s.active);
        s.toggle();
        assert!(!s.active);
    }

    #[test]
    fn active_step_always_triggers_when_probability_one() {
        let s = Step::active();
        assert!(s.should_trigger());
    }

    #[test]
    fn inactive_step_never_triggers() {
        let s = Step::new();
        assert!(!s.should_trigger());
    }

    #[test]
    fn pristine_step_serializes_empty_and_legacy_fat_steps_parse() {
        assert_eq!(serde_json::to_string(&Step::new()).unwrap(), "{}");
        let s: Step = serde_json::from_str("{}").unwrap();
        assert!(!s.active && s.velocity == 1.0 && s.gate_length == 0.25);
        // Pre-compaction blobs spell out every field — must keep parsing.
        let legacy = r#"{"active":true,"velocity":0.8,"probability":1.0,
            "ratchet":2,"ratchet_spacing":0.5,"gate_length":0.25,"parameter_locks":{}}"#;
        let s: Step = serde_json::from_str(legacy).unwrap();
        assert!(s.active && s.ratchet == 2 && s.velocity == 0.8);
    }
}
