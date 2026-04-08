use protocol_core::{InferenceResponse, ModelCapability};
use provider_core::{ProviderError, ProviderStream};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestedCapability {
    Chat,
    Responses,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RouteTarget {
    pub(crate) route_group_id: Uuid,
    pub(crate) provider_kind: String,
    pub(crate) upstream_model: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedRoute {
    pub(crate) route_group_id: Uuid,
    pub(crate) public_model: String,
    pub(crate) provider_kind: String,
    pub(crate) upstream_model: String,
    pub(crate) fallback_chain: Vec<RouteTarget>,
    pub(crate) capability_contract: Vec<ModelCapability>,
}

pub(crate) enum ExecutionOutput {
    Response(InferenceResponse),
    Stream(ProviderStream),
}

pub(crate) struct ExecutionResult {
    pub(crate) output: ExecutionOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionError {
    UnknownModel,
    NoHealthyCandidate,
    ProviderNotRegistered(String),
    Internal(String),
    Provider(ProviderError),
}

impl From<ProviderError> for ExecutionError {
    fn from(value: ProviderError) -> Self {
        Self::Provider(value)
    }
}
