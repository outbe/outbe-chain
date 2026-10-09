use crate::world::rpc::*;

impl Rpc {
    /// Read a certified generation at the exact finalized activation block.
    /// This reader checks both owner projections at `activation.block_number`.
    /// It uses no off-chain storage field to construct the result.
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
            nod_amount_total: nod.totalSettlementCostMinor,
            lysis_allocation_minor: nod.lysisAllocationMinor,
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
            lysis_allocation_minor: nod.lysis_allocation_minor,
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

/// Reuse the canonical generation record in the harness.
pub use outbe_ocomp_protocol::capacity::CapacityRecoveredGenerationBindingV1 as OcompCertifiedGenerationV1;
