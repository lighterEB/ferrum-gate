use axum::http::{HeaderValue, Method, header};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Build a CORS layer from a comma-separated list of allowed origins.
///
/// Returns `None` if the origin list is empty or cannot be parsed.
#[must_use]
pub fn build_cors_layer(origins: &str) -> Option<CorsLayer> {
    let origins: Vec<HeaderValue> = origins
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| HeaderValue::from_str(origin).ok())
        .collect::<Option<Vec<_>>>()?;

    if origins.is_empty() {
        return None;
    }

    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([Method::GET, Method::POST])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
    )
}

/// Read allowed origins from the first environment variable that is set,
/// falling back through the provided list of env var names.
///
/// Returns `None` if none of the environment variables are set.
#[must_use]
pub fn cors_layer_from_env(env_vars: &[&str]) -> Option<CorsLayer> {
    let allowed = env_vars.iter().find_map(|var| std::env::var(var).ok())?;
    build_cors_layer(&allowed)
}

/// Convenience: read `FERRUMGATE_CONSOLE_ALLOWED_ORIGINS` with fallback to
/// `FERRUMGATE_TENANT_API_ALLOWED_ORIGINS` (matching the gateway/control-plane behavior).
#[must_use]
pub fn console_cors_layer_from_env() -> Option<CorsLayer> {
    cors_layer_from_env(&[
        "FERRUMGATE_CONSOLE_ALLOWED_ORIGINS",
        "FERRUMGATE_TENANT_API_ALLOWED_ORIGINS",
    ])
}

/// Convenience: read `FERRUMGATE_TENANT_API_ALLOWED_ORIGINS` (matching the tenant-api behavior).
#[must_use]
pub fn tenant_api_cors_layer_from_env() -> Option<CorsLayer> {
    cors_layer_from_env(&["FERRUMGATE_TENANT_API_ALLOWED_ORIGINS"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cors_layer_returns_some_for_valid_origins() {
        let layer = build_cors_layer("http://localhost:5173").expect("layer");
        // Layer exists; we trust tower-http's CorsLayer
        drop(layer);
    }

    #[test]
    fn build_cors_layer_returns_none_for_empty_string() {
        assert!(build_cors_layer("").is_none());
    }

    #[test]
    fn build_cors_layer_handles_multiple_origins() {
        let layer = build_cors_layer("http://localhost:5173,http://localhost:3000").expect("layer");
        drop(layer);
    }

    #[test]
    fn build_cors_layer_filters_empty_entries() {
        // ",," should result in no valid origins → None
        assert!(build_cors_layer(",,,").is_none());
    }

    #[test]
    fn cors_layer_from_env_reads_first_set_variable() {
        // SAFETY: test-only env manipulation, no concurrent threads
        unsafe {
            std::env::set_var("TEST_CORS_ORIGIN_A", "http://a.example");
            std::env::set_var("TEST_CORS_ORIGIN_B", "http://b.example");
        }

        let layer =
            cors_layer_from_env(&["TEST_CORS_ORIGIN_A", "TEST_CORS_ORIGIN_B"]).expect("layer");
        drop(layer);
    }

    #[test]
    fn cors_layer_from_env_returns_none_when_unset() {
        let layer = cors_layer_from_env(&["TEST_NONEXISTENT_CORS_ORIGIN"]);
        assert!(layer.is_none());
    }
}
