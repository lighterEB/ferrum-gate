use anyhow::Result;
use control_plane::{ControlPlaneState, run};
use provider_core::ProviderRegistry;
use provider_openai_codex::OpenAiCodexProvider;
use std::sync::Arc;
use storage::PlatformStore;

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("control_plane")?;
    let addr = std::env::var("FERRUMGATE_CONTROL_PLANE_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3007".to_string())
        .parse()?;
    let store = PlatformStore::from_env_or_demo().await?;
    let mut registry = ProviderRegistry::new();
    registry.register(OpenAiCodexProvider::shared(Arc::new(store.clone())));
    let state = ControlPlaneState { store, registry };
    run(addr, state).await
}
