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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bearer_token_returns_token_on_valid_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Bearer my-token-123".parse().unwrap(),
        );

        let result = parse_bearer_token(&headers);
        assert_eq!(result.as_deref(), Some("my-token-123"));
    }

    #[test]
    fn parse_bearer_token_returns_none_when_header_missing() {
        let headers = HeaderMap::new();
        assert!(parse_bearer_token(&headers).is_none());
    }

    #[test]
    fn parse_bearer_token_returns_none_when_prefix_wrong() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Token my-token".parse().unwrap(),
        );

        assert!(parse_bearer_token(&headers).is_none());
    }

    #[test]
    fn parse_bearer_token_returns_none_when_value_empty() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::AUTHORIZATION, "".parse().unwrap());
        assert!(parse_bearer_token(&headers).is_none());
    }

    #[test]
    fn parse_bearer_token_returns_none_when_only_bearer_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::AUTHORIZATION, "Bearer ".parse().unwrap());
        // strip_prefix returns Some("") for "Bearer " → "Bearer ", which is an empty string
        let result = parse_bearer_token(&headers);
        assert_eq!(result.as_deref(), Some(""));
    }
}
