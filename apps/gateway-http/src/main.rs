use anyhow::Result;
use gateway_http::{GatewayAppState, run};
use provider_core::ProviderRegistry;
use provider_openai_codex::OpenAiCodexProvider;
use std::sync::Arc;
use storage::PlatformStore;

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("gateway_http")?;
    let addr = std::env::var("FERRUMGATE_GATEWAY_HTTP_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3005".to_string())
        .parse()?;
    let store = PlatformStore::from_env_or_demo().await?;
    let mut registry = ProviderRegistry::new();
    registry.register(OpenAiCodexProvider::shared(Arc::new(store.clone())));
    let state = GatewayAppState { store, registry };
    run(addr, state).await
}
