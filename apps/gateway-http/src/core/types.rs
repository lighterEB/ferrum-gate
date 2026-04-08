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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_error_from_provider_error_wraps_as_provider_variant() {
        let provider_error = ProviderError::new(
            provider_core::ProviderErrorKind::UpstreamUnavailable,
            500,
            "test error".to_string(),
        );
        let execution_error: ExecutionError = provider_error.clone().into();

        assert!(matches!(execution_error, ExecutionError::Provider(_)));
    }

    #[test]
    fn execution_error_variants_are_distinct() {
        let errors = [
            ExecutionError::UnknownModel,
            ExecutionError::NoHealthyCandidate,
            ExecutionError::ProviderNotRegistered("test".to_string()),
            ExecutionError::Internal("something went wrong".to_string()),
        ];

        // Each variant should be distinct
        for (i, a) in errors.iter().enumerate() {
            for (j, b) in errors.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        format!("{a:?}"),
                        format!("{b:?}"),
                        "Error variants {:?} and {:?} should differ",
                        a,
                        b
                    );
                }
            }
        }
    }
}
