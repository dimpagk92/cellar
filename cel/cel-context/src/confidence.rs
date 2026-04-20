use serde::{Deserialize, Serialize};

/// Confidence thresholds for agent behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceThresholds {
    /// Above this: act immediately (default 0.9).
    pub act_immediately: f64,
    /// Above this: act and log for review (default 0.7).
    pub act_and_log: f64,
    /// Above this: act cautiously, verify result (default 0.5).
    /// Below this threshold: pause and notify user.
    pub act_cautiously: f64,
}

impl Default for ConfidenceThresholds {
    fn default() -> Self {
        Self {
            act_immediately: 0.9,
            act_and_log: 0.7,
            act_cautiously: 0.5,
        }
    }
}

/// What behavior the agent should exhibit given a confidence score.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfidenceBehavior {
    /// 0.9-1.0: Act immediately, no hesitation.
    ActImmediately,
    /// 0.7-0.9: Act and log for review.
    ActAndLog,
    /// 0.5-0.7: Act cautiously, verify result.
    ActCautiously,
    /// Below 0.5: Pause, notify user, wait for instruction.
    PauseAndNotify,
}

impl ConfidenceThresholds {
    /// Determine behavior for a given confidence score.
    pub fn behavior_for(&self, confidence: f64) -> ConfidenceBehavior {
        if confidence >= self.act_immediately {
            ConfidenceBehavior::ActImmediately
        } else if confidence >= self.act_and_log {
            ConfidenceBehavior::ActAndLog
        } else if confidence >= self.act_cautiously {
            ConfidenceBehavior::ActCautiously
        } else {
            ConfidenceBehavior::PauseAndNotify
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_thresholds_default_values() {
        let thresholds = ConfidenceThresholds::default();
        assert_eq!(thresholds.act_immediately, 0.9);
        assert_eq!(thresholds.act_and_log, 0.7);
        assert_eq!(thresholds.act_cautiously, 0.5);
    }

    #[test]
    fn test_confidence_behavior_mapping() {
        let thresholds = ConfidenceThresholds::default();

        assert_eq!(thresholds.behavior_for(0.95), ConfidenceBehavior::ActImmediately);
        assert_eq!(thresholds.behavior_for(0.9), ConfidenceBehavior::ActImmediately);
        assert_eq!(thresholds.behavior_for(0.85), ConfidenceBehavior::ActAndLog);
        assert_eq!(thresholds.behavior_for(0.7), ConfidenceBehavior::ActAndLog);
        assert_eq!(thresholds.behavior_for(0.6), ConfidenceBehavior::ActCautiously);
        assert_eq!(thresholds.behavior_for(0.5), ConfidenceBehavior::ActCautiously);
        assert_eq!(thresholds.behavior_for(0.3), ConfidenceBehavior::PauseAndNotify);
        assert_eq!(thresholds.behavior_for(0.0), ConfidenceBehavior::PauseAndNotify);
    }

    #[test]
    fn test_boundary_values_exact() {
        let thresholds = ConfidenceThresholds::default();

        // Exactly at boundaries
        assert_eq!(thresholds.behavior_for(0.9), ConfidenceBehavior::ActImmediately);
        assert_eq!(thresholds.behavior_for(0.7), ConfidenceBehavior::ActAndLog);
        assert_eq!(thresholds.behavior_for(0.5), ConfidenceBehavior::ActCautiously);

        // Just below boundaries
        assert_eq!(thresholds.behavior_for(0.8999), ConfidenceBehavior::ActAndLog);
        assert_eq!(thresholds.behavior_for(0.6999), ConfidenceBehavior::ActCautiously);
        assert_eq!(thresholds.behavior_for(0.4999), ConfidenceBehavior::PauseAndNotify);
    }

    #[test]
    fn test_extreme_values() {
        let thresholds = ConfidenceThresholds::default();

        assert_eq!(thresholds.behavior_for(1.0), ConfidenceBehavior::ActImmediately);
        assert_eq!(thresholds.behavior_for(0.0), ConfidenceBehavior::PauseAndNotify);
    }

    #[test]
    fn test_negative_confidence() {
        let thresholds = ConfidenceThresholds::default();
        // Negative values should result in PauseAndNotify (lowest tier)
        assert_eq!(thresholds.behavior_for(-0.1), ConfidenceBehavior::PauseAndNotify);
        assert_eq!(thresholds.behavior_for(-1.0), ConfidenceBehavior::PauseAndNotify);
    }

    #[test]
    fn test_over_one_confidence() {
        let thresholds = ConfidenceThresholds::default();
        // Values > 1.0 should still map to ActImmediately
        assert_eq!(thresholds.behavior_for(1.5), ConfidenceBehavior::ActImmediately);
        assert_eq!(thresholds.behavior_for(100.0), ConfidenceBehavior::ActImmediately);
    }

    #[test]
    fn test_custom_thresholds() {
        let thresholds = ConfidenceThresholds {
            act_immediately: 0.95,
            act_and_log: 0.8,
            act_cautiously: 0.6,
        };

        assert_eq!(thresholds.behavior_for(0.96), ConfidenceBehavior::ActImmediately);
        assert_eq!(thresholds.behavior_for(0.94), ConfidenceBehavior::ActAndLog);
        assert_eq!(thresholds.behavior_for(0.79), ConfidenceBehavior::ActCautiously);
        assert_eq!(thresholds.behavior_for(0.59), ConfidenceBehavior::PauseAndNotify);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let thresholds = ConfidenceThresholds::default();
        let json = serde_json::to_string(&thresholds).unwrap();
        let deserialized: ConfidenceThresholds = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.act_immediately, thresholds.act_immediately);
        assert_eq!(deserialized.act_and_log, thresholds.act_and_log);
        assert_eq!(deserialized.act_cautiously, thresholds.act_cautiously);
    }

    #[test]
    fn test_behavior_is_monotonic() {
        // As confidence increases, behavior should only get more permissive (or stay same)
        let thresholds = ConfidenceThresholds::default();
        let behaviors: Vec<ConfidenceBehavior> = (0..=100)
            .map(|i| thresholds.behavior_for(i as f64 / 100.0))
            .collect();

        let rank = |b: &ConfidenceBehavior| -> u8 {
            match b {
                ConfidenceBehavior::PauseAndNotify => 0,
                ConfidenceBehavior::ActCautiously => 1,
                ConfidenceBehavior::ActAndLog => 2,
                ConfidenceBehavior::ActImmediately => 3,
            }
        };

        for i in 1..behaviors.len() {
            assert!(
                rank(&behaviors[i]) >= rank(&behaviors[i - 1]),
                "Behavior should be monotonically non-decreasing with confidence. \
                 At {}/100: {:?}, at {}/100: {:?}",
                i - 1, behaviors[i - 1], i, behaviors[i]
            );
        }
    }
}
