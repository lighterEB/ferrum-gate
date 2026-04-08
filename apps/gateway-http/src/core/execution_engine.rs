use async_stream::stream;
use chrono::Utc;
use futures::StreamExt;
use protocol_core::{InferenceRequest, StreamEventKind};
use provider_core::ProviderStream;
use scheduler::{ProviderAccountCandidate, ProviderOutcome, select_candidate};
use std::collections::BTreeMap;
use storage::GatewayAuthContext;

use crate::{
    GatewayAppState,
    core::{
        route_resolver::RouteResolver,
        types::{
            ExecutionError, ExecutionOutput, ExecutionResult, RequestedCapability, ResolvedRoute,
            RouteTarget,
        },
    },
    provider_outcome_for_error,
};

pub(crate) struct ExecutionEngine;

impl ExecutionEngine {
    pub(crate) async fn execute(
        state: &GatewayAppState,
        auth: &GatewayAuthContext,
        canonical: InferenceRequest,
        capability: RequestedCapability,
    ) -> Result<ExecutionResult, ExecutionError> {
        let route =
            RouteResolver::resolve(&state.store, &canonical.public_model, capability.clone())
                .await?;
        let mut route_targets = vec![RouteTarget {
            route_group_id: route.route_group_id,
            provider_kind: route.provider_kind.clone(),
            upstream_model: route.upstream_model.clone(),
        }];
        route_targets.extend(route.fallback_chain.clone());

        let mut last_error = None;

        for route_target in route_targets {
            let Some(provider) = state.registry.get(&route_target.provider_kind) else {
                return Err(ExecutionError::ProviderNotRegistered(
                    route_target.provider_kind.clone(),
                ));
            };
            let candidates = ordered_candidates(
                state,
                route_target.route_group_id,
                &route_target.provider_kind,
                &canonical.public_model,
            )
            .await?;

            for candidate in candidates.into_iter().take(3) {
                let request =
                    canonical_request_for_candidate(&canonical, &route_target, &candidate);
                let execution = match (&capability, canonical.stream) {
                    (RequestedCapability::Chat, true) => {
                        provider.stream_chat(request).await.map(|stream| {
                            ExecutionOutput::Stream(wrap_stream_with_bookkeeping(
                                state,
                                auth,
                                &route,
                                route_target.route_group_id,
                                &route_target.provider_kind,
                                &candidate,
                                stream,
                            ))
                        })
                    }
                    (RequestedCapability::Chat, false) => {
                        provider.chat(request).await.map(ExecutionOutput::Response)
                    }
                    (RequestedCapability::Responses, true) => {
                        provider.stream_responses(request).await.map(|stream| {
                            ExecutionOutput::Stream(wrap_stream_with_bookkeeping(
                                state,
                                auth,
                                &route,
                                route_target.route_group_id,
                                &route_target.provider_kind,
                                &candidate,
                                stream,
                            ))
                        })
                    }
                    (RequestedCapability::Responses, false) => provider
                        .responses(request)
                        .await
                        .map(ExecutionOutput::Response),
                };

                match execution {
                    Ok(ExecutionOutput::Response(response)) => {
                        record_success(
                            state,
                            auth,
                            &canonical.public_model,
                            route_target.route_group_id,
                            &route_target.provider_kind,
                            &candidate,
                            8,
                            &response,
                        )
                        .await;
                        return Ok(ExecutionResult {
                            output: ExecutionOutput::Response(response),
                        });
                    }
                    Ok(ExecutionOutput::Stream(stream)) => {
                        return Ok(ExecutionResult {
                            output: ExecutionOutput::Stream(stream),
                        });
                    }
                    Err(error) => {
                        record_failure(state, candidate.account_id, &error).await;
                        let eligible_for_fallback =
                            fallback_eligible(provider_outcome_for_error(&error));
                        last_error = Some(error);
                        if !eligible_for_fallback {
                            return Err(ExecutionError::Provider(last_error.expect("error")));
                        }
                    }
                }
            }
        }

        Err(last_error
            .map(ExecutionError::Provider)
            .unwrap_or(ExecutionError::NoHealthyCandidate))
    }
}

async fn ordered_candidates(
    state: &GatewayAppState,
    route_group_id: uuid::Uuid,
    provider_kind: &str,
    public_model: &str,
) -> Result<Vec<ProviderAccountCandidate>, ExecutionError> {
    let candidates = state
        .store
        .scheduler_candidates(public_model)
        .await
        .map_err(|error| ExecutionError::Internal(error.to_string()))?;
    let mut remaining = candidates
        .into_iter()
        .filter(|candidate| {
            candidate.route_group_id == route_group_id && candidate.provider_kind == provider_kind
        })
        .collect::<Vec<_>>();

    if remaining.is_empty() {
        return Err(ExecutionError::NoHealthyCandidate);
    }

    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let Some(selected) = select_candidate(Utc::now(), &remaining) else {
            break;
        };
        let index = remaining
            .iter()
            .position(|candidate| candidate.account_id == selected.account_id)
            .expect("selected candidate must exist");
        ordered.push(remaining.remove(index));
    }

    if ordered.is_empty() {
        return Err(ExecutionError::NoHealthyCandidate);
    }

    Ok(ordered)
}

fn canonical_request_for_candidate(
    canonical: &InferenceRequest,
    route_target: &RouteTarget,
    candidate: &ProviderAccountCandidate,
) -> InferenceRequest {
    let mut metadata = BTreeMap::new();
    metadata.extend(canonical.metadata.clone());
    metadata.insert(
        "provider_account_id".to_string(),
        candidate.account_id.to_string(),
    );
    metadata.insert(
        "route_group_id".to_string(),
        route_target.route_group_id.to_string(),
    );

    InferenceRequest {
        upstream_model: Some(route_target.upstream_model.clone()),
        metadata,
        ..canonical.clone()
    }
}

fn wrap_stream_with_bookkeeping(
    state: &GatewayAppState,
    auth: &GatewayAuthContext,
    route: &ResolvedRoute,
    _route_group_id: uuid::Uuid,
    provider_kind: &str,
    candidate: &ProviderAccountCandidate,
    provider_stream: ProviderStream,
) -> ProviderStream {
    let store = state.store.clone();
    let tenant_id = auth.tenant.id;
    let api_key_id = auth.api_key_id;
    let public_model = route.public_model.clone();
    let provider_kind = provider_kind.to_string();
    let account_id = candidate.account_id;

    Box::pin(stream! {
        let mut provider_stream = provider_stream;
        while let Some(item) = provider_stream.next().await {
            match item {
                Ok(event) => {
                    if matches!(event.kind, StreamEventKind::Done)
                        && let Some(response) = event.response.clone()
                    {
                        store
                            .record_request(
                                tenant_id,
                                Some(api_key_id),
                                public_model.clone(),
                                provider_kind.clone(),
                                200,
                                10,
                                response.usage,
                            )
                            .await
                            .ok();
                        store
                            .mark_scheduler_outcome(account_id, ProviderOutcome::Success)
                            .await
                            .ok();
                    }
                    yield Ok(event);
                }
                Err(error) => {
                    store
                        .mark_scheduler_outcome(account_id, provider_outcome_for_error(&error))
                        .await
                        .ok();
                    yield Err(error);
                    return;
                }
            }
        }
    })
}

async fn record_success(
    state: &GatewayAppState,
    auth: &GatewayAuthContext,
    public_model: &str,
    _route_group_id: uuid::Uuid,
    provider_kind: &str,
    candidate: &ProviderAccountCandidate,
    latency_ms: u64,
    response: &protocol_core::InferenceResponse,
) {
    state
        .store
        .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
        .await
        .ok();
    state
        .store
        .record_request(
            auth.tenant.id,
            Some(auth.api_key_id),
            public_model.to_string(),
            provider_kind.to_string(),
            200,
            latency_ms,
            response.usage.clone(),
        )
        .await
        .ok();
}

async fn record_failure(
    state: &GatewayAppState,
    account_id: uuid::Uuid,
    error: &provider_core::ProviderError,
) {
    state
        .store
        .mark_scheduler_outcome(account_id, provider_outcome_for_error(error))
        .await
        .ok();
}

fn fallback_eligible(outcome: ProviderOutcome) -> bool {
    matches!(
        outcome,
        ProviderOutcome::RateLimited { .. }
            | ProviderOutcome::UpstreamFailure
            | ProviderOutcome::TransportFailure
            | ProviderOutcome::QuotaExhausted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_eligible_rate_limited() {
        assert!(fallback_eligible(ProviderOutcome::RateLimited {
            retry_after_seconds: Some(30),
        }));
    }

    #[test]
    fn fallback_eligible_rate_limited_no_retry_after() {
        assert!(fallback_eligible(ProviderOutcome::RateLimited {
            retry_after_seconds: None,
        }));
    }

    #[test]
    fn fallback_eligible_upstream_failure() {
        assert!(fallback_eligible(ProviderOutcome::UpstreamFailure));
    }

    #[test]
    fn fallback_eligible_transport_failure() {
        assert!(fallback_eligible(ProviderOutcome::TransportFailure));
    }

    #[test]
    fn fallback_eligible_quota_exhausted() {
        assert!(fallback_eligible(ProviderOutcome::QuotaExhausted));
    }

    #[test]
    fn fallback_not_eligible_success() {
        assert!(!fallback_eligible(ProviderOutcome::Success));
    }

    #[test]
    fn fallback_not_eligible_invalid_credentials() {
        assert!(!fallback_eligible(ProviderOutcome::InvalidCredentials));
    }

    #[test]
    fn canonical_request_for_candidate_injects_metadata() {
        use protocol_core::{CanonicalMessage, FrontendProtocol, InferenceRequest, MessageRole};
        use std::collections::BTreeMap;
        use uuid::Uuid;

        let route_group_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let route_target = RouteTarget {
            route_group_id,
            provider_kind: "openai_codex".to_string(),
            upstream_model: "gpt-5-codex".to_string(),
        };
        let candidate = ProviderAccountCandidate {
            account_id,
            route_group_id,
            provider_kind: "openai_codex".to_string(),
            weight: 100,
            runtime: scheduler::AccountRuntime::new(scheduler::AccountState::Active, 8),
        };
        let mut metadata = BTreeMap::new();
        metadata.insert("custom_key".to_string(), "custom_value".to_string());
        let original = InferenceRequest {
            protocol: FrontendProtocol::OpenAi,
            public_model: "gpt-5-codex".to_string(),
            upstream_model: None,
            previous_response_id: None,
            reasoning: None,
            stream: false,
            messages: vec![CanonicalMessage {
                role: MessageRole::User,
                content: "hello".to_string(),
                parts: vec![],
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![],
            metadata,
        };

        let result = canonical_request_for_candidate(&original, &route_target, &candidate);

        assert_eq!(result.upstream_model.as_deref(), Some("gpt-5-codex"));
        let meta = &result.metadata;
        assert_eq!(
            meta.get("custom_key").map(|s| s.as_str()),
            Some("custom_value")
        );
        assert_eq!(
            meta.get("provider_account_id").map(|s| s.as_str()),
            Some(account_id.to_string().as_str())
        );
        assert_eq!(
            meta.get("route_group_id").map(|s| s.as_str()),
            Some(route_group_id.to_string().as_str())
        );
    }
}
