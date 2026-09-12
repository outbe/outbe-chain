use crate::world::rpc::*;

impl Rpc {
    /// Read and verify the Metadosis and Nod generation projections at the
    /// exact finalized activation block.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_certified_generation_on(
        &self,
        port: u16,
        activation: &OcompPublicActivationV1,
    ) -> Option<OcompCertifiedGenerationV1> {
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        if finalized_height < activation.block_number {
            return None;
        }
        let block_hash = eth::block_hash(&rpc_url, activation.block_number)?
            .parse::<B256>()
            .ok()?;
        if block_hash != activation.block_hash {
            return None;
        }

        let active_bytes = eth::read_call_at(
            &rpc_url,
            addresses::WWD_ADDR,
            &IMetadosis::getActiveLysisGenerationCall {
                wwd: activation.worldwide_day,
            },
            activation.block_number,
        )?;
        let limits = poc_schema_limits();
        let active = ActiveGenerationV1::decode_canonical(active_bytes.as_ref(), &limits).ok()?;

        let nod = eth::read_call_at(
            &rpc_url,
            addresses::NOD_ADDR,
            &INod::certifiedGenerationCall {
                worldwideDay: activation.worldwide_day,
            },
            activation.block_number,
        )?;
        if !nod.exists || nod.worldwideDay != activation.worldwide_day {
            return None;
        }
        let nod = NodCertifiedGenerationProjection {
            worldwide_day: WorldwideDay::new(nod.worldwideDay),
            generation: nod.generation,
            job_id: active.job_id,
            // The public read does not surface it, and no assertion here looks at it.
            protocol_bundle_hash: B256::ZERO,
            program_semantics_hash: active.program_semantics_hash,
            nod_root: nod.nodRoot,
            bucket_root: nod.bucketRoot,
            output_manifest_root: nod.outputManifestRoot,
            tribute_count: nod.tributeCount,
            nod_count: nod.nodCount,
            bucket_count: nod.bucketCount,
            nod_amount_total: nod.nodAmountTotal,
            nod_gratis_consumed: nod.nodGratisConsumed,
            issued_at: nod.issuedAt,
            next_nod_ordinal: 0,
            last_progress_height: activation.block_number,
        };
        let authority = active_nod_set(&active, &nod).ok()?;
        if authority.job_id != activation.job_id {
            return None;
        }

        Some(OcompCertifiedGenerationV1 {
            worldwide_day: authority.worldwide_day,
            generation: authority.generation,
            job_id: authority.job_id,
            program_semantics_hash: authority.program_semantics_hash,
            nod_root: authority.nod_root,
            bucket_root: nod.bucket_root,
            output_manifest_root: nod.output_manifest_root,
            tribute_count: nod.tribute_count,
            nod_count: authority.nod_count,
            bucket_count: nod.bucket_count,
            nod_amount_total: nod.nod_amount_total,
            nod_gratis_consumed: nod.nod_gratis_consumed,
            issued_at: nod.issued_at,
            result_evidence_hash: active.result_evidence_hash,
            block_number: activation.block_number,
            block_hash,
        })
    }

    /// Observe whether the Nod owner already had a certified generation at an
    /// exact block. A malformed projection fails closed as `None`.
    #[cfg(feature = "ocomp-integration")]
    pub fn nod_certified_generation_exists_on(
        &self,
        port: u16,
        worldwide_day: u32,
        block_number: u64,
    ) -> Option<bool> {
        eth::read_call_at(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::certifiedGenerationCall {
                worldwideDay: worldwide_day,
            },
            block_number,
        )
        .map(|generation| generation.exists)
    }
}

/// Finalized, cross-owner authority for one proof-backed Nod generation.
///
/// Both owner projections are read at `block_number`; off-chain storage never supplies
/// any field in this record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompCertifiedGenerationV1 {
    pub worldwide_day: u32,
    pub generation: u64,
    pub job_id: B256,
    pub program_semantics_hash: B256,
    pub nod_root: B256,
    pub bucket_root: B256,
    pub output_manifest_root: B256,
    pub tribute_count: u32,
    pub nod_count: u32,
    pub bucket_count: u32,
    pub nod_amount_total: U256,
    pub nod_gratis_consumed: U256,
    pub issued_at: u64,
    pub result_evidence_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
}
