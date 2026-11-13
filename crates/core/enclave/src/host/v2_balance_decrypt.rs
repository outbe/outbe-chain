//! Shared V2 context-bound balance opening helper for Gratis and Promis view keys.

use alloy_primitives::{Address, U256};
use outbe_tee::{
    tee_decrypt::{tee_decrypt_v2_envelope, V2EnvelopeDomain},
    TeeCryptoError,
};

use super::errors::LocalCryptoError;

#[derive(Clone, Copy)]
pub(super) enum V2BalanceLedger {
    Gratis,
    Promis,
}

impl V2BalanceLedger {
    const fn marker(self) -> &'static [u8; 4] {
        match self {
            Self::Gratis => b"GRA2",
            Self::Promis => b"PRO2",
        }
    }

    const fn key_info(self) -> &'static [u8] {
        match self {
            Self::Gratis => b"outbe/gratis/amount-key/v2",
            Self::Promis => b"outbe/promis/amount-key/v2",
        }
    }

    const fn nonce_info(self) -> &'static [u8] {
        match self {
            Self::Gratis => b"outbe/gratis/amount-nonce/v2",
            Self::Promis => b"outbe/promis/amount-nonce/v2",
        }
    }

    const fn invalid_balance(self) -> LocalCryptoError {
        match self {
            Self::Gratis => LocalCryptoError::InvalidGratisBalance,
            Self::Promis => LocalCryptoError::InvalidPromisBalance,
        }
    }

    const fn map_error(self, error: TeeCryptoError) -> LocalCryptoError {
        match (self, error) {
            (_, TeeCryptoError::InvalidEnvelope) => self.invalid_balance(),
            (_, TeeCryptoError::HkdfExpand) => LocalCryptoError::HkdfExpand,
            (_, TeeCryptoError::HkdfFill) => LocalCryptoError::HkdfFill,
            (Self::Gratis, TeeCryptoError::AeadKey) => LocalCryptoError::InvalidGratisViewKey,
            (Self::Promis, TeeCryptoError::AeadKey) => LocalCryptoError::InvalidPromisViewKey,
            (Self::Gratis, TeeCryptoError::InvalidPlaintextLength { .. }) => {
                LocalCryptoError::InvalidGratisAmountLength
            }
            (Self::Promis, TeeCryptoError::InvalidPlaintextLength { .. }) => {
                LocalCryptoError::InvalidPromisAmountLength
            }
            (
                Self::Gratis,
                TeeCryptoError::DecryptionFailed
                | TeeCryptoError::RngFailure
                | TeeCryptoError::AeadSeal,
            ) => LocalCryptoError::GratisBalanceDecryptionFailed,
            (
                Self::Promis,
                TeeCryptoError::DecryptionFailed
                | TeeCryptoError::RngFailure
                | TeeCryptoError::AeadSeal,
            ) => LocalCryptoError::PromisBalanceDecryptionFailed,
        }
    }
}

pub(super) fn decrypt_v2_balance(
    ledger: V2BalanceLedger,
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, LocalCryptoError> {
    if blob.is_empty() {
        return Ok(U256::ZERO);
    }
    if blob.len() != 92 {
        return Err(ledger.invalid_balance());
    }
    let opened = tee_decrypt_v2_envelope(
        view_key,
        account,
        V2EnvelopeDomain {
            scope: &[0],
            marker: ledger.marker(),
            key_info: ledger.key_info(),
            nonce_info: ledger.nonce_info(),
        },
        blob,
    )
    .map_err(|err| ledger.map_error(err))?;
    let amount = opened
        .try_into_array::<32>()
        .map_err(|err| ledger.map_error(err))?;
    Ok(U256::from_be_bytes(amount))
}
