use async_stream::stream;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
};
use futures::StreamExt;
use protocol_core::{FinishReason, FrontendProtocol, InferenceRequest, StreamEventKind};
use scheduler::ProviderOutcome;
use std::{collections::BTreeMap, convert::Infallible};

use crate::{
    GatewayAppState,
    middleware::auth::authenticate_gateway,
    openai_http::{
        ChatCompletionRequest, chat_completion_json, chat_stream_content_delta_json,
        chat_stream_done_json, chat_stream_error_json, chat_stream_tool_calls_delta_json,
        internal_error, openai_error, openai_message_to_canonical_message,
        openai_tools_to_canonical_tools, provider_error_response,
    },
    provider_outcome_for_error,
};

pub(crate) async fn chat_completions(
    State(state): State<GatewayAppState>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let auth = match authenticate_gateway(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };

    let candidate = match state.store.choose_candidate(&request.model).await {
        Ok(Some(candidate)) => candidate,
        Ok(None) => {
            return openai_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "No healthy provider candidate",
            );
        }
        Err(error) => return internal_error(&error.to_string()),
    };

    let route_group = match state.store.resolve_route_group(&request.model).await {
        Ok(Some(route_group)) => route_group,
        Ok(None) => return openai_error(StatusCode::NOT_FOUND, "Unknown model"),
        Err(error) => return internal_error(&error.to_string()),
    };

    let candidate_account_id = candidate.account_id;
    let provider_kind = candidate.provider_kind.clone();
    let route_group_id = route_group.id;

    let Some(provider) = state.registry.get(&provider_kind) else {
        return openai_error(StatusCode::BAD_GATEWAY, "Provider adapter not registered");
    };

    let canonical = InferenceRequest {
        protocol: FrontendProtocol::OpenAi,
        public_model: request.model.clone(),
        upstream_model: Some(route_group.upstream_model),
        previous_response_id: None,
        reasoning: request.reasoning.clone(),
        stream: request.stream,
        messages: request
            .messages
            .iter()
            .map(openai_message_to_canonical_message)
            .collect(),
        tools: openai_tools_to_canonical_tools(&request.tools),
        metadata: BTreeMap::from([
            (
                "provider_account_id".to_string(),
                candidate_account_id.to_string(),
            ),
            ("route_group_id".to_string(), route_group_id.to_string()),
        ]),
    };

    if request.stream {
        match provider.stream_chat(canonical).await {
            Ok(mut provider_stream) => {
                let tenant_id = auth.tenant.id;
                let api_key_id = auth.api_key_id;
                let stream_public_model = request.model.clone();
                let stream_provider_kind = provider_kind.clone();
                let store = state.store.clone();
                let stream = stream! {
                    while let Some(item) = provider_stream.next().await {
                        match item {
                            Ok(event) => match event.kind {
                                StreamEventKind::ContentDelta => {
                                    let payload = chat_stream_content_delta_json(
                                        &stream_public_model,
                                        &event.delta.unwrap_or_default(),
                                    );
                                    yield Ok::<Event, Infallible>(Event::default().data(payload.to_string()));
                                }
                                StreamEventKind::Done => {
                                    if let Some(response) = event.response {
                                        if !response.tool_calls.is_empty() {
                                            let payload = chat_stream_tool_calls_delta_json(
                                                &stream_public_model,
                                                &response.tool_calls,
                                            );
                                            yield Ok(Event::default().data(payload.to_string()));
                                        }
                                        let final_payload = chat_stream_done_json(
                                            &stream_public_model,
                                            &response.finish_reason,
                                        );
                                        store
                                            .record_request(
                                                tenant_id,
                                                Some(api_key_id),
                                                stream_public_model.clone(),
                                                stream_provider_kind.clone(),
                                                200,
                                                10,
                                                response.usage,
                                            )
                                            .await
                                            .ok();
                                        store
                                            .mark_scheduler_outcome(
                                                candidate_account_id,
                                                ProviderOutcome::Success,
                                            )
                                            .await
                                            .ok();
                                        yield Ok(Event::default().data(final_payload.to_string()));
                                    } else {
                                        let payload = chat_stream_done_json(
                                            &stream_public_model,
                                            &FinishReason::Stop,
                                        );
                                        yield Ok(Event::default().data(payload.to_string()));
                                    }
                                    yield Ok(Event::default().data("[DONE]"));
                                }
                                _ => {}
                            },
                            Err(error) => {
                                store
                                    .mark_scheduler_outcome(
                                        candidate_account_id,
                                        provider_outcome_for_error(&error),
                                    )
                                    .await
                                    .ok();
                                let payload = chat_stream_error_json(&error);
                                yield Ok(Event::default().data(payload.to_string()));
                                yield Ok(Event::default().data("[DONE]"));
                            }
                        }
                    }
                };

                return Sse::new(stream).into_response();
            }
            Err(error) => {
                state
                    .store
                    .mark_scheduler_outcome(
                        candidate_account_id,
                        provider_outcome_for_error(&error),
                    )
                    .await
                    .ok();
                return provider_error_response(error);
            }
        }
    }

    match provider.chat(canonical).await {
        Ok(response) => {
            state
                .store
                .mark_scheduler_outcome(candidate_account_id, ProviderOutcome::Success)
                .await
                .ok();
            state
                .store
                .record_request(
                    auth.tenant.id,
                    Some(auth.api_key_id),
                    request.model.clone(),
                    provider_kind,
                    200,
                    8,
                    response.usage.clone(),
                )
                .await
                .ok();
            Json(chat_completion_json(response)).into_response()
        }
        Err(error) => {
            state
                .store
                .mark_scheduler_outcome(candidate_account_id, provider_outcome_for_error(&error))
                .await
                .ok();
            provider_error_response(error)
        }
    }
}
