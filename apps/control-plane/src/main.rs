use anyhow::Result;
use control_plane::{ControlPlaneState, run};

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("control_plane")?;
    let addr = std::env::var("FERRUMGATE_CONTROL_PLANE_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3007".to_string())
        .parse()?;
    run(addr, ControlPlaneState::demo()).await
}
