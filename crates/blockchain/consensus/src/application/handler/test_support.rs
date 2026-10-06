#[cfg(test)]
use super::validate_header_consensus_artifacts_for_activation;
use super::verification::{HeaderArtifactRequest, HeaderArtifactValidationDeps};
use crate::dkg_manager::AncestryReader;

#[cfg(test)]
pub(super) async fn validate_header_consensus_artifacts(
    request: HeaderArtifactRequest<'_>,
    deps: HeaderArtifactValidationDeps<'_, impl AncestryReader>,
) -> Result<(), String> {
    validate_header_consensus_artifacts_for_activation(request, deps)
        .await
        .map_err(|error| error.to_string())
}
