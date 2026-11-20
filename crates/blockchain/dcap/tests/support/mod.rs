use serde::Deserialize;

pub(super) const QUOTE: &[u8] =
    include_bytes!("../fixtures/intel-dcap-1.26/sgx-processor-quote-v3.bin");
const COLLATERAL_WRAPPER: &str =
    include_str!("../fixtures/intel-dcap-1.26/sgx-processor-collateral-wrapper.json");

#[derive(Deserialize)]
pub(super) struct FixtureCollateral {
    pub(super) pck_crl_issuer_chain: String,
    pub(super) root_ca_crl: String,
    pub(super) pck_crl: String,
    pub(super) tcb_info_issuer_chain: String,
    pub(super) tcb_info: String,
    pub(super) tcb_info_signature: String,
    pub(super) qe_identity_issuer_chain: String,
    pub(super) qe_identity: String,
    pub(super) qe_identity_signature: String,
}

impl FixtureCollateral {
    pub(super) fn load() -> Self {
        serde_json::from_str(COLLATERAL_WRAPPER).unwrap()
    }
}

pub(super) fn signed_document(field: &str, body: &str, signature: &str) -> Vec<u8> {
    format!(r#"{{"{field}":{body},"signature":"{signature}"}}"#).into_bytes()
}
