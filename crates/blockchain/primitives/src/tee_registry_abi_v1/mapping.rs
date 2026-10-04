//! Typed field projections for present Registry records owned by consumers.

/// Implements `From<&Record>` for the canonical V1 binding ABI view.
///
/// The local record must carry the named V1 fields with their canonical types.
/// Its domain validation and the treatment of an absent record remain with the
/// consumer. This projection sets `exists` to true and copies fields once.
///
/// Adding `decode_fields` also creates a crate-private field projection back
/// into the local record. The caller must check `exists` before using it.
#[macro_export]
macro_rules! impl_tee_registry_binding_v1_mapping {
    ($($request:tt)+) => {
        $crate::__impl_tee_registry_binding_v1_mapping! {
            $($request)+;
            node_id_hash => nodeIdHash,
            enclave_id => enclaveId,
            binding_id => bindingId,
            intent_hash => intentHash,
            evidence_hash => evidenceHash,
            policy_hash => policyHash,
            binding_version => bindingVersion,
            registration_version => registrationVersion,
            renewal_nonce => renewalNonce,
            transition_nonce => transitionNonce,
            lease_started_at => leaseStartedAt,
            valid_until => validUntil,
            collateral_valid_until => collateralValidUntil,
            recipient_x25519 => recipientX25519,
            attestation_ed25519 => attestationEd25519,
            noise_responder_x25519 => noiseResponderX25519,
            mrenclave => mrenclave,
            mrsigner => mrsigner,
            isv_prod_id => isvProdId,
            isv_svn => isvSvn,
            platform_tcb_status => platformTcbStatus,
            verdict_hash => verdictHash,
            node_host_authorization_hash => nodeHostAuthorizationHash,
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __impl_tee_registry_binding_v1_mapping {
    (@default_view; $($field:ident => $abi:ident),+ $(,)?) => {
        impl ::core::default::Default
            for $crate::tee_registry_abi_v1::NodeEnclaveBindingV1View
        {
            fn default() -> Self {
                Self { exists: false, $($abi: ::core::default::Default::default()),+ }
            }
        }
    };
    ($record:ty, decode_fields; $($field:ident => $abi:ident),+ $(,)?) => {
        $crate::__impl_tee_registry_binding_v1_mapping! {
            $record; $($field => $abi),+
        }
        impl $record {
            /// Copy V1 fields after the consumer has checked record existence.
            pub(crate) fn from_registry_binding_fields_v1(
                view: &$crate::tee_registry_abi_v1::NodeEnclaveBindingV1View,
            ) -> Self {
                Self { $($field: view.$abi),+ }
            }
        }
    };
    ($record:ty; $($field:ident => $abi:ident),+ $(,)?) => {
        impl ::core::convert::From<&$record>
            for $crate::tee_registry_abi_v1::NodeEnclaveBindingV1View
        {
            fn from(record: &$record) -> Self {
                Self { exists: true, $($abi: record.$field),+ }
            }
        }
    };
}
