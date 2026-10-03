//! Finalized CE point reads and body-availability reporting.
use super::*;
use outbe_compressed_entities::{CeDomain, SelectedHeaderV1, WwdEntityId};

#[derive(Clone)]
pub(super) struct PointReadRuntime {
    pub(super) tree: Arc<CompressedTreeService>,
    pub(super) bodies: RuntimeBodyReaders,
    pub(super) chain_id: u64,
}

impl std::fmt::Debug for PointReadRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PointReadRuntime")
            .field("chain_id", &self.chain_id)
            .finish_non_exhaustive()
    }
}

pub(super) async fn serve<P>(
    provider: &Arc<P>,
    runtime: Option<PointReadRuntime>,
    request: PointReadRequestV1,
) -> RpcResult<PointReadResultV1>
where
    P: HeaderProvider<Header = OutbeHeader> + BlockIdReader + Send + Sync + 'static,
{
    let Some(runtime) = runtime else {
        return Ok(PointReadResultV1::Unavailable);
    };
    let provider = Arc::clone(provider);
    tokio::task::spawn_blocking(move || {
        runtime.tree.serve_point_read_v1(
            runtime.chain_id,
            request,
            |height, hash| finalized_header(&*provider, height, hash),
            |domain, raw_id| runtime.body(domain, raw_id),
        )
    })
    .await
    .map_err(|error| internal_err(format!("point-read worker failed: {error}")))?
    .map_err(|error| invalid_params(error.to_string()))
}

fn finalized_header<P: HeaderProvider<Header = OutbeHeader> + BlockIdReader>(
    provider: &P,
    height: u64,
    expected_hash: B256,
) -> Option<SelectedHeaderV1> {
    let finalized_height = provider
        .finalized_block_num_hash()
        .ok()
        .flatten()
        .map(|block| block.number);
    finalized_header_at(finalized_height, height, || {
        provider
            .sealed_header(height)
            .ok()
            .flatten()
            .filter(|header| header.hash() == expected_hash)
            .map(|header| SelectedHeaderV1 {
                block_number: height,
                block_hash: expected_hash,
                extra_data: header.header().inner.extra_data.to_vec(),
            })
    })
}

impl PointReadRuntime {
    fn body(&self, domain: CeDomain, raw_id: WwdEntityId) -> Option<Vec<u8>> {
        let encoded = match domain {
            CeDomain::Tribute => {
                encode_optional_body(self.bodies.tribute().get_stored_body(raw_id), |body| {
                    body.encode()
                })
            }
            CeDomain::NodItem => {
                encode_optional_body(self.bodies.nod().get_stored_item(raw_id), |body| {
                    body.encode()
                })
            }
            CeDomain::NodBucket => {
                encode_optional_body(self.bodies.nod().get_stored_bucket(raw_id), |body| {
                    body.encode()
                })
            }
        };
        if encoded.is_none() {
            self.bodies.report_unavailable();
        }
        encoded
    }
}

fn finalized_header_at(
    finalized_height: Option<u64>,
    height: u64,
    load: impl FnOnce() -> Option<SelectedHeaderV1>,
) -> Option<SelectedHeaderV1> {
    if finalized_height? < height {
        return None;
    }
    load()
}
fn encode_optional_body<T, E>(
    result: Result<Option<T>, E>,
    encode: impl FnOnce(T) -> Vec<u8>,
) -> Option<Vec<u8>> {
    result.ok().flatten().map(encode)
}
#[cfg(test)]
mod tests;
