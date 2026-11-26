use serde::{Deserialize, Serialize};
use strum::Display;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Display)]
pub enum OddsType {
    #[strum(serialize = "1")]
    Home,
    #[strum(serialize = "X")]
    Draw,
    #[strum(serialize = "2")]
    Away,
    #[strum(serialize = "over")]
    Over,
    #[strum(serialize = "under")]
    Under,
    #[strum(serialize = "handicap")]
    Handicap,
    #[strum(serialize = "asian_handicap")]
    AsianHandicap,
    #[strum(serialize = "both_teams_score_yes")]
    BothTeamsScoreYes,
    #[strum(serialize = "both_teams_score_no")]
    BothTeamsScoreNo,
    #[strum(serialize = "total")]
    Total,
    #[strum(serialize = "individual_total")]
    IndividualTotal,
    #[strum(serialize = "even")]
    Even,
    #[strum(serialize = "odd")]
    Odd,
    #[strum(serialize = "custom")]
    Custom,
}

impl OddsType {
    pub fn is_two_way(&self) -> bool {
        matches!(
            self,
            OddsType::Over
                | OddsType::Under
                | OddsType::Handicap
                | OddsType::AsianHandicap
                | OddsType::BothTeamsScoreYes
                | OddsType::BothTeamsScoreNo
                | OddsType::Even
                | OddsType::Odd
        )
    }

    pub fn is_three_way(&self) -> bool {
        matches!(self, OddsType::Home | OddsType::Draw | OddsType::Away)
    }

    pub fn opposite(&self) -> Option<Self> {
        match self {
            OddsType::Home => Some(OddsType::Away),
            OddsType::Away => Some(OddsType::Home),
            OddsType::Over => Some(OddsType::Under),
            OddsType::Under => Some(OddsType::Over),
            OddsType::BothTeamsScoreYes => Some(OddsType::BothTeamsScoreNo),
            OddsType::BothTeamsScoreNo => Some(OddsType::BothTeamsScoreYes),
            _ => None,
        }
    }
}

pub fn decimal_to_implied_probability(odds: f64) -> f64 {
    if !odds.is_finite() || odds <= 1.0 {
        return 0.0;
    }
    1.0 / odds
}

pub fn calculate_margin(odds: &[f64]) -> f64 {
    if odds.is_empty() {
        return 0.0;
    }
    let sum: f64 = odds
        .iter()
        .map(|&odd| decimal_to_implied_probability(odd))
        .sum();
    if !sum.is_finite() {
        return 0.0;
    }
    (sum - 1.0) * 100.0
}

/// Returns guaranteed ROI in percent for a complete set of valid decimal odds.
/// Invalid odds are rejected rather than silently contributing zero probability.
pub fn calculate_surebet_profit(odds: &[f64]) -> Option<f64> {
    if odds.len() < 2 || odds.iter().any(|odd| !odd.is_finite() || *odd <= 1.0) {
        return None;
    }

    let inverse_sum: f64 = odds.iter().map(|odd| 1.0 / odd).sum();
    if !inverse_sum.is_finite() || inverse_sum <= 0.0 || inverse_sum >= 1.0 {
        return None;
    }

    let profit = (1.0 - inverse_sum) * 100.0;
    profit.is_finite().then_some(profit)
}

/// Calculate equalized-return stakes. Invalid input returns an empty vector.
pub fn calculate_stakes(odds: &[f64], total_stake: f64) -> Vec<f64> {
    if odds.is_empty()
        || !total_stake.is_finite()
        || total_stake <= 0.0
        || odds.iter().any(|odd| !odd.is_finite() || *odd <= 1.0)
    {
        return Vec::new();
    }

    let inverse_probabilities: Vec<f64> = odds.iter().map(|odd| 1.0 / odd).collect();
    let inverse_sum: f64 = inverse_probabilities.iter().sum();
    if !inverse_sum.is_finite() || inverse_sum <= 0.0 {
        return Vec::new();
    }

    inverse_probabilities
        .iter()
        .map(|probability| (total_stake * probability) / inverse_sum)
        .collect()
}

pub fn calculate_payout(stake: f64, odds: f64) -> f64 {
    stake * odds
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn rejects_invalid_decimal_odds_for_surebet_math() {
        assert_eq!(calculate_surebet_profit(&[2.0, 1.0]), None);
        assert_eq!(calculate_surebet_profit(&[2.0, 0.0]), None);
        assert_eq!(calculate_surebet_profit(&[2.0, f64::NAN]), None);
        assert_eq!(calculate_surebet_profit(&[2.0, f64::INFINITY]), None);
        assert_eq!(calculate_surebet_profit(&[2.0]), None);
    }

    #[test]
    fn calculates_valid_surebet_profit_and_stakes() {
        let profit = calculate_surebet_profit(&[2.2, 2.2]).unwrap();
        assert!((profit - 9.0909090909).abs() < 1e-8);

        let stakes = calculate_stakes(&[2.2, 2.2], 1_000.0);
        assert_eq!(stakes.len(), 2);
        assert!((stakes.iter().sum::<f64>() - 1_000.0).abs() < 1e-8);
        assert!((stakes[0] - stakes[1]).abs() < 1e-8);
    }

    #[test]
    fn rejects_invalid_stake_inputs() {
        assert!(calculate_stakes(&[], 100.0).is_empty());
        assert!(calculate_stakes(&[2.0, f64::NAN], 100.0).is_empty());
        assert!(calculate_stakes(&[2.0, 2.0], f64::INFINITY).is_empty());
        assert!(calculate_stakes(&[2.0, 2.0], 0.0).is_empty());
    }
}
