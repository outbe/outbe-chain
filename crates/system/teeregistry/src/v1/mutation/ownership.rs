use super::*;

impl TeeRegistry<'_> {
    pub(super) fn validate_mutation_ownership_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
    ) -> Result<()> {
        let VerifiedClaimsMutationV1 {
            expected_operation,
            intent,
            ..
        } = *mutation;
        let MutationContextV1 { node_id_hash, .. } = *context;
        let enclave_owner = self.v1_enclave_node_hash.read(&intent.enclave_id)?;
        let binding_owner = self.v1_binding_node_hash.read(&intent.binding_id)?;
        match expected_operation {
            AttestationOperationV1::RegisterEnclave => {
                self.validate_registration_ownership_v1(
                    mutation,
                    context,
                    current,
                    ReverseOwnersV1 {
                        enclave: enclave_owner,
                        binding: binding_owner,
                    },
                )?;
            }
            AttestationOperationV1::RenewEnclave => {
                if enclave_owner != node_id_hash || binding_owner != node_id_hash {
                    return Err(PrecompileError::Fatal(
                        "stored V1 renewal reverse ownership is inconsistent".into(),
                    ));
                }
            }
            AttestationOperationV1::ReplaceEnclaveBinding
            | AttestationOperationV1::TransitionEnclaveMeasurement
            | AttestationOperationV1::PrepareEnclaveUpgrade => {
                if !enclave_owner.is_zero() || !binding_owner.is_zero() {
                    return Err(PrecompileError::Revert(
                        "successor enclave or binding id has already been used".into(),
                    ));
                }
            }
        }

        Ok(())
    }

    fn validate_registration_ownership_v1(
        &self,
        mutation: &VerifiedClaimsMutationV1<'_>,
        context: &MutationContextV1,
        current: &Option<NodeEnclaveBindingV1>,
        owners: ReverseOwnersV1,
    ) -> Result<()> {
        let intent = mutation.intent;
        let node_id_hash = context.node_id_hash;
        let ReverseOwnersV1 {
            enclave: enclave_owner,
            binding: binding_owner,
        } = owners;

        if let Some(current) = current.as_ref() {
            if self.v1_enclave_node_hash.read(&current.enclave_id)? != node_id_hash
                || self.v1_binding_node_hash.read(&current.binding_id)? != node_id_hash
            {
                return Err(PrecompileError::Fatal(
                    "expired rejoin found inconsistent current reverse ownership".into(),
                ));
            }
            let same_current_enclave = intent.enclave_id == current.enclave_id;
            if (same_current_enclave && enclave_owner != node_id_hash)
                || (!same_current_enclave && !enclave_owner.is_zero())
            {
                return Err(PrecompileError::Revert(
                    "expired rejoin enclave is not current or globally fresh".into(),
                ));
            }
            if !binding_owner.is_zero() {
                return Err(PrecompileError::Revert(
                    "expired rejoin binding id has already been used".into(),
                ));
            }
        } else {
            if !enclave_owner.is_zero() {
                return Err(PrecompileError::Revert(
                    "enclave is already bound to another node".into(),
                ));
            }
            if !binding_owner.is_zero() {
                return Err(PrecompileError::Revert(
                    "binding id has already been used by another node".into(),
                ));
            }
        }
        Ok(())
    }
}
