use chrono::{DateTime, TimeDelta, Utc};
use scheduler::AccountState;
use std::collections::HashMap;

/// Returns true if an account in this state should be probed/refreshed by the poller.
#[allow(dead_code)]
pub fn is_eligible_for_poll(state: &AccountState) -> bool {
    matches!(
        state,
        AccountState::Active | AccountState::Cooling | AccountState::QuotaExhausted
    )
}

/// Poll interval configuration per provider type.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct PollConfig {
    /// Probe interval in seconds per provider kind.
    pub probe_interval_seconds: HashMap<String, u64>,
    /// Refresh look-ahead in seconds: refresh tokens expiring within this window.
    pub refresh_before_seconds: u64,
    /// Max accounts to probe per cycle (prevents API rate limiting).
    pub batch_size: usize,
}

#[allow(dead_code)]
impl PollConfig {
    pub fn probe_interval_for(&self, provider_kind: &str) -> TimeDelta {
        let seconds = self
            .probe_interval_seconds
            .get(provider_kind)
            .copied()
            .unwrap_or(1800);
        TimeDelta::seconds(seconds as i64)
    }

    #[must_use]
    pub fn default_for_openai_codex() -> Self {
        let mut m = HashMap::new();
        m.insert("openai_codex".to_string(), 1800);
        m.insert("qwen".to_string(), 1800);
        m.insert("anthropic".to_string(), 3600);
        Self {
            probe_interval_seconds: m,
            refresh_before_seconds: 1800,
            batch_size: 10,
        }
    }
}

// ─── Poller background task ────────────────────────────────────────────

use crate::{ControlPlaneState, execute_provider_account_probe, execute_provider_account_refresh};
use tracing::{info, warn};

/// Run the auto-poller in a loop until the shutdown signal is received.
#[allow(dead_code)]
pub async fn run_poller(
    state: ControlPlaneState,
    config: PollConfig,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    const PRINCIPAL: &str = "auto_poller";

    info!(
        "poller started: batch_size={}, refresh_before={}s, providers={:?}",
        config.batch_size,
        config.refresh_before_seconds,
        config.probe_interval_seconds.keys().collect::<Vec<_>>()
    );

    // Track next probe time per provider kind
    let mut next_probe: HashMap<String, DateTime<Utc>> = config
        .probe_interval_seconds
        .keys()
        .map(|k| (k.clone(), Utc::now()))
        .collect();

    let mut refresh_interval = tokio::time::interval(std::time::Duration::from_secs(
        config.refresh_before_seconds.max(60),
    ));
    refresh_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("poller received shutdown signal, exiting");
                return;
            }
            _ = refresh_interval.tick() => {
                // Refresh due: dispatch due refreshes
                match state
                    .store
                    .dispatch_due_provider_account_refreshes(
                        config.batch_size.max(1),
                        config.refresh_before_seconds as i64,
                    )
                    .await
                {
                    Ok(leases) => {
                        for lease in leases {
                            let account_id = lease.account_id;
                            info!(%account_id, "poller: dispatching refresh for account");
                            let _ = execute_provider_account_refresh(&state, PRINCIPAL, account_id).await;
                        }
                    }
                    Err(_e) => warn!("poller: failed to dispatch refreshes"),
                }

                // Probe due: probe all eligible accounts
                let now = Utc::now();
                let accounts = match state.store.list_provider_accounts().await {
                    Ok(accts) => accts,
                    Err(e) => {
                        warn!(error = %e, "poller: failed to list accounts for probe");
                        continue;
                    }
                };

                for account in &accounts {
                    if !is_eligible_for_poll(&account.state) {
                        continue;
                    }

                    let next = next_probe.entry(account.provider.clone()).or_insert_with(|| now);
                    if now < *next {
                        continue;
                    }

                    info!(
                        account_id = %account.id,
                        provider = %account.provider,
                        "poller: probing account"
                    );

                    match execute_provider_account_probe(&state, PRINCIPAL, account.id).await {
                        Ok(_) => {
                            *next = now + config.probe_interval_for(&account.provider);
                        }
                        Err(_resp) => {
                            warn!(
                                account_id = %account.id,
                                "poller: probe failed"
                            );
                            *next = now + config.probe_interval_for(&account.provider);
                        }
                    }
                }
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_config_returns_provider_specific_intervals() {
        let config = PollConfig::default_for_openai_codex();
        assert_eq!(
            config.probe_interval_for("openai_codex"),
            TimeDelta::seconds(1800)
        );
        assert_eq!(config.probe_interval_for("qwen"), TimeDelta::seconds(1800));
        assert_eq!(
            config.probe_interval_for("anthropic"),
            TimeDelta::seconds(3600)
        );
    }

    #[test]
    fn poll_config_unknown_provider_gets_default_interval() {
        let config = PollConfig::default_for_openai_codex();
        assert_eq!(
            config.probe_interval_for("unknown_provider"),
            TimeDelta::seconds(1800)
        );
    }

    #[test]
    fn poll_config_custom_overrides_defaults() {
        let mut m = HashMap::new();
        m.insert("qwen".to_string(), 600);
        let config = PollConfig {
            probe_interval_seconds: m,
            refresh_before_seconds: 900,
            batch_size: 5,
        };
        assert_eq!(config.probe_interval_for("qwen"), TimeDelta::seconds(600));
        assert_eq!(
            config.probe_interval_for("openai_codex"),
            TimeDelta::seconds(1800)
        );
        assert_eq!(config.refresh_before_seconds, 900);
        assert_eq!(config.batch_size, 5);
    }

    #[test]
    fn poller_eligible_states_include_active_cooling_quota_exhausted() {
        let eligible = [
            AccountState::Active,
            AccountState::Cooling,
            AccountState::QuotaExhausted,
        ];
        for state in eligible {
            assert!(is_eligible_for_poll(&state));
        }
    }

    #[test]
    fn poller_ineligible_states_exclude_disabled_draining_invalid() {
        let ineligible = [
            AccountState::Disabled,
            AccountState::Draining,
            AccountState::InvalidCredentials,
            AccountState::PendingValidation,
        ];
        for state in ineligible {
            assert!(!is_eligible_for_poll(&state));
        }
    }
}
