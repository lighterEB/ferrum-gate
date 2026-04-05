use anyhow::Result;
use gateway_http::{GatewayAppState, run};

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("gateway_http")?;
    let addr = std::env::var("FERRUMGATE_GATEWAY_HTTP_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3005".to_string())
        .parse()?;
    run(addr, GatewayAppState::demo()).await
}
