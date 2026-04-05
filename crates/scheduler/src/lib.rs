use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    PendingValidation,
    Active,
    Cooling,
    Draining,
    QuotaExhausted,
    InvalidCredentials,
    Disabled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountRuntime {
    pub state: AccountState,
    pub health_score: u8,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub circuit_open_until: Option<DateTime<Utc>>,
    pub in_flight: u32,
    pub max_in_flight: u32,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl AccountRuntime {
    #[must_use]
    pub fn new(state: AccountState, max_in_flight: u32) -> Self {
        Self {
            state,
            health_score: 100,
            cooldown_until: None,
            circuit_open_until: None,
            in_flight: 0,
            max_in_flight,
            last_used_at: None,
        }
    }

    #[must_use]
    pub fn is_schedulable(&self, now: DateTime<Utc>) -> bool {
        self.state == AccountState::Active
            && self.cooldown_until.is_none_or(|until| until <= now)
            && self.circuit_open_until.is_none_or(|until| until <= now)
            && self.in_flight < self.max_in_flight
    }

    pub fn apply_outcome(&mut self, outcome: ProviderOutcome, now: DateTime<Utc>) {
        match outcome {
            ProviderOutcome::Success => {
                self.state = AccountState::Active;
                self.health_score = self.health_score.saturating_add(2);
                self.last_used_at = Some(now);
            }
            ProviderOutcome::RateLimited {
                retry_after_seconds,
            } => {
                self.state = AccountState::Cooling;
                let retry_after = retry_after_seconds.unwrap_or(30).clamp(1, 600);
                self.cooldown_until = Some(now + TimeDelta::seconds(retry_after));
                self.health_score = self.health_score.saturating_sub(10);
            }
            ProviderOutcome::UpstreamFailure | ProviderOutcome::TransportFailure => {
                self.circuit_open_until = Some(now + TimeDelta::seconds(10));
                self.health_score = self.health_score.saturating_sub(15);
            }
            ProviderOutcome::InvalidCredentials => {
                self.state = AccountState::InvalidCredentials;
                self.health_score = 0;
            }
            ProviderOutcome::QuotaExhausted => {
                self.state = AccountState::QuotaExhausted;
                self.health_score = self.health_score.saturating_sub(25);
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAccountCandidate {
    pub account_id: Uuid,
    pub route_group_id: Uuid,
    pub provider_kind: String,
    pub weight: u32,
    pub runtime: AccountRuntime,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedCandidate {
    pub account_id: Uuid,
    pub route_group_id: Uuid,
    pub provider_kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderOutcome {
    Success,
    RateLimited { retry_after_seconds: Option<i64> },
    UpstreamFailure,
    TransportFailure,
    InvalidCredentials,
    QuotaExhausted,
}

#[must_use]
pub fn select_candidate(
    now: DateTime<Utc>,
    candidates: &[ProviderAccountCandidate],
) -> Option<SelectedCandidate> {
    let mut schedulable: Vec<&ProviderAccountCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.runtime.is_schedulable(now))
        .collect();

    schedulable.sort_by(|left, right| {
        right
            .runtime
            .health_score
            .cmp(&left.runtime.health_score)
            .then(right.weight.cmp(&left.weight))
            .then(left.runtime.last_used_at.cmp(&right.runtime.last_used_at))
            .then(left.account_id.cmp(&right.account_id))
    });

    schedulable.first().map(|candidate| SelectedCandidate {
        account_id: candidate.account_id,
        route_group_id: candidate.route_group_id,
        provider_kind: candidate.provider_kind.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_moves_account_to_cooling() {
        let mut runtime = AccountRuntime::new(AccountState::Active, 8);
        let now = Utc::now();
        runtime.apply_outcome(
            ProviderOutcome::RateLimited {
                retry_after_seconds: Some(60),
            },
            now,
        );

        assert_eq!(runtime.state, AccountState::Cooling);
        assert!(runtime.cooldown_until.expect("cooldown") > now);
    }

    #[test]
    fn selection_prefers_health_then_weight_then_lru() {
        let now = Utc::now();
        let route_group_id = Uuid::new_v4();
        let first = ProviderAccountCandidate {
            account_id: Uuid::new_v4(),
            route_group_id,
            provider_kind: "openai_codex".to_string(),
            weight: 80,
            runtime: AccountRuntime {
                last_used_at: Some(now),
                ..AccountRuntime::new(AccountState::Active, 8)
            },
        };
        let second = ProviderAccountCandidate {
            account_id: Uuid::new_v4(),
            route_group_id,
            provider_kind: "openai_codex".to_string(),
            weight: 100,
            runtime: AccountRuntime {
                last_used_at: Some(now - TimeDelta::seconds(30)),
                ..AccountRuntime::new(AccountState::Active, 8)
            },
        };

        let expected_id = second.account_id;
        let selected = select_candidate(now, &[first, second]).expect("candidate");
        assert_eq!(selected.account_id, expected_id);
    }
}
