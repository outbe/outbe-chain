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
        $crate::__tee_registry_binding_v1_fields! { @mapping $($request)+ }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __impl_tee_registry_binding_v1_mapping {
    (@default_view; $($field:ident : $ty:ty => $abi:ident),+ $(,)?) => {
        impl ::core::default::Default
            for $crate::tee_registry_abi_v1::NodeEnclaveBindingV1View
        {
            fn default() -> Self {
                Self { exists: false, $($abi: ::core::default::Default::default()),+ }
            }
        }
    };
    ($record:ty, decode_fields; $($field:ident : $ty:ty => $abi:ident),+ $(,)?) => {
        $crate::__impl_tee_registry_binding_v1_mapping! {
            $record; $($field : $ty => $abi),+
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
    ($record:ty; $($field:ident : $ty:ty => $abi:ident),+ $(,)?) => {
        impl ::core::convert::From<&$record>
            for $crate::tee_registry_abi_v1::NodeEnclaveBindingV1View
        {
            fn from(record: &$record) -> Self {
                Self { exists: true, $($abi: record.$field),+ }
            }
        }
    };
}

/// Define a domain-owned Registry record using the canonical V1 field schema.
/// Attributes, ownership and validation remain with the caller; this does not
/// unify the resulting Rust types or change their wire representations.
#[macro_export]
macro_rules! define_tee_registry_binding_v1 {
    ($(#[$attr:meta])* $visibility:vis struct $name:ident) => {
        $crate::__tee_registry_binding_v1_fields! {
            @record ($(#[$attr])*) ($visibility) ($name)
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __tee_registry_binding_v1_fields {
    ($($request:tt)+) => {
        $crate::__apply_tee_registry_binding_v1_fields! {
            $($request)+;
            node_id_hash: ::alloy_primitives::B256 => nodeIdHash,
            enclave_id: ::alloy_primitives::B256 => enclaveId,
            binding_id: ::alloy_primitives::B256 => bindingId,
            intent_hash: ::alloy_primitives::B256 => intentHash,
            evidence_hash: ::alloy_primitives::B256 => evidenceHash,
            policy_hash: ::alloy_primitives::B256 => policyHash,
            binding_version: u64 => bindingVersion,
            registration_version: u64 => registrationVersion,
            renewal_nonce: u64 => renewalNonce,
            transition_nonce: u64 => transitionNonce,
            lease_started_at: u64 => leaseStartedAt,
            valid_until: u64 => validUntil,
            collateral_valid_until: u64 => collateralValidUntil,
            recipient_x25519: ::alloy_primitives::B256 => recipientX25519,
            attestation_ed25519: ::alloy_primitives::B256 => attestationEd25519,
            noise_responder_x25519: ::alloy_primitives::B256 => noiseResponderX25519,
            mrenclave: ::alloy_primitives::B256 => mrenclave,
            mrsigner: ::alloy_primitives::B256 => mrsigner,
            isv_prod_id: u16 => isvProdId,
            isv_svn: u16 => isvSvn,
            platform_tcb_status: u8 => platformTcbStatus,
            verdict_hash: ::alloy_primitives::B256 => verdictHash,
            node_host_authorization_hash: ::alloy_primitives::B256 => nodeHostAuthorizationHash,
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __apply_tee_registry_binding_v1_fields {
    (@record ($(#[$attr:meta])*) ($visibility:vis) ($name:ident);
        $($field:ident : $ty:ty => $abi:ident),+ $(,)?) => {
        $(#[$attr])*
        $visibility struct $name { $(pub $field: $ty),+ }
    };
    (@mapping $($request:tt)+) => {
        $crate::__impl_tee_registry_binding_v1_mapping! { $($request)+ }
    };
}
