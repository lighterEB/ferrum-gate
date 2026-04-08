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
use protocol_core::{FrontendProtocol, InferenceRequest, StreamEventKind};
use scheduler::ProviderOutcome;
use std::{collections::BTreeMap, convert::Infallible};

use crate::{
    GatewayAppState,
    middleware::{auth::authenticate_gateway, request_id::new_openai_object_id},
    openai_http::{
        ResponsesRequest, internal_error, openai_error, provider_error_response,
        responses_input_to_messages, responses_json, responses_stream_completed_json,
        responses_stream_content_part_added_json, responses_stream_content_part_done_json,
        responses_stream_created_json, responses_stream_delta_json, responses_stream_done_json,
        responses_stream_failed_json, responses_stream_function_call_arguments_delta_json,
        responses_stream_function_call_arguments_done_json,
        responses_stream_function_call_output_item_added_json,
        responses_stream_function_call_output_item_done_json,
        responses_stream_output_item_added_json, responses_stream_output_item_done_json,
        responses_tools_to_canonical_tools,
    },
    provider_outcome_for_error,
};

pub(crate) async fn responses(
    State(state): State<GatewayAppState>,
    headers: HeaderMap,
    Json(request): Json<ResponsesRequest>,
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
        previous_response_id: request.previous_response_id.clone(),
        reasoning: request.reasoning.clone(),
        stream: request.stream,
        messages: responses_input_to_messages(request.input),
        tools: responses_tools_to_canonical_tools(&request.tools),
        metadata: BTreeMap::from([
            (
                "provider_account_id".to_string(),
                candidate_account_id.to_string(),
            ),
            ("route_group_id".to_string(), route_group_id.to_string()),
        ]),
    };

    if request.stream {
        match provider.stream_responses(canonical).await {
            Ok(mut provider_stream) => {
                let store = state.store.clone();
                let tenant_id = auth.tenant.id;
                let api_key_id = auth.api_key_id;
                let stream_public_model = request.model.clone();
                let stream_provider_kind = provider_kind.clone();
                let stream_response_id = new_openai_object_id("resp");
                let stream_message_item_id = new_openai_object_id("msg");
                let stream = stream! {
                    let mut streamed_text = String::new();
                    let mut text_stream_started = false;
                    let created_payload = responses_stream_created_json(&stream_response_id, &stream_public_model);
                    yield Ok::<Event, Infallible>(
                        Event::default()
                            .event("response.created")
                            .data(created_payload.to_string())
                    );
                    while let Some(item) = provider_stream.next().await {
                        match item {
                            Ok(event) => match event.kind {
                                StreamEventKind::ContentDelta => {
                                    if !text_stream_started {
                                        text_stream_started = true;
                                        let output_item_added_payload = responses_stream_output_item_added_json(
                                            &stream_response_id,
                                            &stream_message_item_id,
                                        );
                                        yield Ok(
                                            Event::default()
                                                .event("response.output_item.added")
                                                .data(output_item_added_payload.to_string())
                                        );
                                        let content_part_added_payload = responses_stream_content_part_added_json(
                                            &stream_response_id,
                                            &stream_message_item_id,
                                        );
                                        yield Ok(
                                            Event::default()
                                                .event("response.content_part.added")
                                                .data(content_part_added_payload.to_string())
                                        );
                                    }
                                    let delta = event.delta.unwrap_or_default();
                                    streamed_text.push_str(&delta);
                                    let payload = responses_stream_delta_json(
                                        &stream_response_id,
                                        &stream_message_item_id,
                                        &delta,
                                    );
                                    yield Ok::<Event, Infallible>(
                                        Event::default()
                                            .event("response.output_text.delta")
                                            .data(payload.to_string())
                                    );
                                }
                                StreamEventKind::Done => {
                                    if let Some(response) = event.response {
                                        store
                                            .record_request(
                                                tenant_id,
                                                Some(api_key_id),
                                                stream_public_model.clone(),
                                                stream_provider_kind.clone(),
                                                200,
                                                10,
                                                response.usage.clone(),
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
                                        let final_text = if response.output_text.is_empty() {
                                            streamed_text.clone()
                                        } else {
                                            response.output_text.clone()
                                        };
                                        if text_stream_started && !final_text.is_empty() {
                                            let done_payload = responses_stream_done_json(
                                                &stream_response_id,
                                                &stream_message_item_id,
                                                &final_text,
                                            );
                                            yield Ok(Event::default().event("response.output_text.done").data(done_payload.to_string()));
                                            let content_part_done_payload = responses_stream_content_part_done_json(
                                                &stream_response_id,
                                                &stream_message_item_id,
                                                &final_text,
                                            );
                                            yield Ok(Event::default().event("response.content_part.done").data(content_part_done_payload.to_string()));
                                            let output_item_done_payload = responses_stream_output_item_done_json(
                                                &stream_response_id,
                                                &stream_message_item_id,
                                                &final_text,
                                            );
                                            yield Ok(Event::default().event("response.output_item.done").data(output_item_done_payload.to_string()));
                                        }
                                        let mut tool_call_item_ids = BTreeMap::new();
                                        let tool_call_output_index_base = usize::from(text_stream_started && !final_text.is_empty());
                                        for (index, tool_call) in response.tool_calls.iter().enumerate() {
                                            let item_id = new_openai_object_id("fc");
                                            tool_call_item_ids.insert(tool_call.id.clone(), item_id.clone());
                                            let output_index = tool_call_output_index_base + index;
                                            let added_payload = responses_stream_function_call_output_item_added_json(
                                                &stream_response_id,
                                                &item_id,
                                                output_index,
                                                tool_call,
                                            );
                                            yield Ok(Event::default().event("response.output_item.added").data(added_payload.to_string()));
                                            if !tool_call.arguments.is_empty() {
                                                let delta_payload = responses_stream_function_call_arguments_delta_json(
                                                    &stream_response_id,
                                                    &item_id,
                                                    output_index,
                                                    tool_call,
                                                );
                                                yield Ok(Event::default().event("response.function_call_arguments.delta").data(delta_payload.to_string()));
                                            }
                                            let arguments_done_payload = responses_stream_function_call_arguments_done_json(
                                                &stream_response_id,
                                                &item_id,
                                                output_index,
                                                tool_call,
                                            );
                                            yield Ok(Event::default().event("response.function_call_arguments.done").data(arguments_done_payload.to_string()));
                                            let output_item_done_payload = responses_stream_function_call_output_item_done_json(
                                                &stream_response_id,
                                                &item_id,
                                                output_index,
                                                tool_call,
                                            );
                                            yield Ok(Event::default().event("response.output_item.done").data(output_item_done_payload.to_string()));
                                        }
                                        let completed_payload = responses_stream_completed_json(
                                            &stream_response_id,
                                            text_stream_started.then_some(stream_message_item_id.as_str()),
                                            &tool_call_item_ids,
                                            response,
                                        );
                                        yield Ok(Event::default().event("response.completed").data(completed_payload.to_string()));
                                    }
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
                                let payload = responses_stream_failed_json(&stream_response_id, &error);
                                yield Ok(Event::default().event("response.failed").data(payload.to_string()));
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

    match provider.responses(canonical).await {
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
            Json(responses_json(response)).into_response()
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
