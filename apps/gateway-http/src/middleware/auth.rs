use axum::{
    http::{self, HeaderMap, StatusCode},
    response::Response,
};
use storage::GatewayAuthContext;

use crate::{
    GatewayAppState,
    openai_http::{internal_error, openai_error},
};

pub(crate) async fn authenticate_gateway(
    state: &GatewayAppState,
    headers: &HeaderMap,
) -> Result<GatewayAuthContext, Response> {
    let Some(token) = parse_bearer_token(headers) else {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "Missing bearer token",
        ));
    };

    state
        .store
        .validate_gateway_api_key(&token)
        .await
        .map_err(|error| internal_error(&error.to_string()))?
        .ok_or_else(|| openai_error(StatusCode::UNAUTHORIZED, "Invalid API key"))
}

fn parse_bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(ToString::to_string)
}
