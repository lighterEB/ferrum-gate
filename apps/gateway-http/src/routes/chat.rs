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
use std::{collections::BTreeMap, convert::Infallible};

use crate::{
    GatewayAppState,
    core::{
        execution_engine::ExecutionEngine,
        types::{ExecutionError, ExecutionOutput},
    },
    middleware::auth::authenticate_gateway,
    openai_http::{
        ChatCompletionRequest, chat_completion_json, chat_stream_content_delta_json,
        chat_stream_done_json, chat_stream_error_json, chat_stream_tool_calls_delta_json,
        internal_error, openai_error, openai_message_to_canonical_message,
        openai_tools_to_canonical_tools, provider_error_response,
    },
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

    let canonical = InferenceRequest {
        protocol: FrontendProtocol::OpenAi,
        public_model: request.model.clone(),
        upstream_model: None,
        previous_response_id: None,
        reasoning: request.reasoning.clone(),
        stream: request.stream,
        messages: request
            .messages
            .iter()
            .map(openai_message_to_canonical_message)
            .collect(),
        tools: openai_tools_to_canonical_tools(&request.tools),
        metadata: BTreeMap::new(),
    };

    match ExecutionEngine::execute(
        &state,
        &auth,
        canonical,
        crate::core::types::RequestedCapability::Chat,
    )
    .await
    {
        Ok(result) => match result.output {
            ExecutionOutput::Response(response) => {
                Json(chat_completion_json(response)).into_response()
            }
            ExecutionOutput::Stream(mut provider_stream) => {
                let stream_public_model = request.model.clone();
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
                                let payload = chat_stream_error_json(&error);
                                yield Ok(Event::default().data(payload.to_string()));
                                yield Ok(Event::default().data("[DONE]"));
                            }
                        }
                    }
                };

                Sse::new(stream).into_response()
            }
        },
        Err(ExecutionError::UnknownModel) => openai_error(StatusCode::NOT_FOUND, "Unknown model"),
        Err(ExecutionError::NoHealthyCandidate) => openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No healthy provider candidate",
        ),
        Err(ExecutionError::ProviderNotRegistered(_)) => {
            openai_error(StatusCode::BAD_GATEWAY, "Provider adapter not registered")
        }
        Err(ExecutionError::Internal(message)) => internal_error(&message),
        Err(ExecutionError::Provider(error)) => provider_error_response(error),
    }
}
