use anyhow::Result;
use storage::PlatformStore;
use tenant_api::{TenantApiState, run};

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("tenant_api")?;
    let addr = std::env::var("FERRUMGATE_TENANT_API_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3006".to_string())
        .parse()?;
    let state = TenantApiState {
        store: PlatformStore::from_env_or_demo().await?,
    };
    run(addr, state).await
}
