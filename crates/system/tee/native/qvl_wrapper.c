/*
 * Thin ABI adapter around the exact-pinned Intel DCAP QVL.
 *
 * Rust never depends on Intel struct layout directly. This file is compiled
 * against the headers from libsgx-dcap-quote-verify-dev 1.26.100.1 and copies
 * only stable values into an Outbe-owned result structure.
 */

#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <sgx_dcap_quoteverify.h>

struct outbe_qvl_collateral_v1 {
    const uint8_t *pck_crl_issuer_chain;
    uint32_t pck_crl_issuer_chain_size;
    const uint8_t *root_ca_crl;
    uint32_t root_ca_crl_size;
    const uint8_t *pck_crl;
    uint32_t pck_crl_size;
    const uint8_t *tcb_info_issuer_chain;
    uint32_t tcb_info_issuer_chain_size;
    const uint8_t *tcb_info;
    uint32_t tcb_info_size;
    const uint8_t *qe_identity_issuer_chain;
    uint32_t qe_identity_issuer_chain_size;
    const uint8_t *qe_identity;
    uint32_t qe_identity_size;
};

struct outbe_qvl_result_v1 {
    uint32_t aggregate_status;
    uint32_t collateral_expiration_status;
    uint16_t supplemental_major_version;
    uint16_t supplemental_minor_version;
    int64_t earliest_issue_date;
    int64_t latest_issue_date;
    int64_t earliest_expiration_date;
    uint32_t tcb_evaluation_data_number;
    uint8_t root_key_id[48];
    uint8_t tcb_cpusvn[16];
    uint16_t tcb_pce_isvsvn;
    uint16_t pce_id;
    uint32_t tee_type;
    uint8_t sgx_type;
    uint8_t platform_instance_id[16];
    int32_t dynamic_platform;
    int32_t cached_keys;
    int32_t smt_enabled;
    uint8_t advisory_ids[450];
    uint32_t qe_status;
    uint32_t qe_tcb_evaluation_data_number;
};

_Static_assert(sizeof(struct outbe_qvl_collateral_v1) == 112,
               "unexpected Outbe collateral ABI size");
_Static_assert(_Alignof(struct outbe_qvl_collateral_v1) == 8,
               "unexpected Outbe collateral ABI alignment");
_Static_assert(sizeof(struct outbe_qvl_result_v1) == 608,
               "unexpected Outbe result ABI size");
_Static_assert(_Alignof(struct outbe_qvl_result_v1) == 8,
               "unexpected Outbe result ABI alignment");
_Static_assert(offsetof(struct outbe_qvl_result_v1, earliest_issue_date) == 16,
               "unexpected Outbe result date offset");
_Static_assert(offsetof(struct outbe_qvl_result_v1, root_key_id) == 44,
               "unexpected Outbe result root-key offset");
_Static_assert(offsetof(struct outbe_qvl_result_v1, advisory_ids) == 148,
               "unexpected Outbe result advisory offset");
_Static_assert(offsetof(struct outbe_qvl_result_v1, qe_status) == 600,
               "unexpected Outbe result QE-status offset");
_Static_assert(sizeof(sgx_ql_qv_supplemental_t) == 672,
               "unexpected Intel QVL supplemental ABI");

enum outbe_qvl_wrapper_status {
    OUTBE_QVL_WRAPPER_OK = 0,
    OUTBE_QVL_WRAPPER_INVALID_PARAMETER = 1,
    OUTBE_QVL_WRAPPER_UNSUPPORTED_ABI = 2,
    OUTBE_QVL_WRAPPER_QVL_ERROR = 3,
};

static int has_invalid_component(const struct outbe_qvl_collateral_v1 *input) {
    return input->pck_crl_issuer_chain == NULL || input->pck_crl_issuer_chain_size == 0 ||
           input->root_ca_crl == NULL || input->root_ca_crl_size == 0 ||
           input->pck_crl == NULL || input->pck_crl_size == 0 ||
           input->tcb_info_issuer_chain == NULL ||
           input->tcb_info_issuer_chain_size == 0 || input->tcb_info == NULL ||
           input->tcb_info_size == 0 || input->qe_identity_issuer_chain == NULL ||
           input->qe_identity_issuer_chain_size == 0 || input->qe_identity == NULL ||
           input->qe_identity_size == 0;
}

int32_t outbe_qvl_verify_quote_v1(
    const uint8_t *quote,
    uint32_t quote_size,
    const struct outbe_qvl_collateral_v1 *input,
    int64_t expiration_check_date,
    struct outbe_qvl_result_v1 *output) {
    if (quote == NULL || quote_size == 0 || input == NULL || output == NULL ||
        has_invalid_component(input)) {
        return OUTBE_QVL_WRAPPER_INVALID_PARAMETER;
    }
    if (sizeof(time_t) != sizeof(int64_t)) {
        return OUTBE_QVL_WRAPPER_UNSUPPORTED_ABI;
    }

    memset(output, 0, sizeof(*output));

    sgx_ql_qve_collateral_t collateral = {
        .major_version = 3,
        .minor_version = 1,
        .tee_type = 0,
        .pck_crl_issuer_chain = (char *)input->pck_crl_issuer_chain,
        .pck_crl_issuer_chain_size = input->pck_crl_issuer_chain_size,
        .root_ca_crl = (char *)input->root_ca_crl,
        .root_ca_crl_size = input->root_ca_crl_size,
        .pck_crl = (char *)input->pck_crl,
        .pck_crl_size = input->pck_crl_size,
        .tcb_info_issuer_chain = (char *)input->tcb_info_issuer_chain,
        .tcb_info_issuer_chain_size = input->tcb_info_issuer_chain_size,
        .tcb_info = (char *)input->tcb_info,
        .tcb_info_size = input->tcb_info_size,
        .qe_identity_issuer_chain = (char *)input->qe_identity_issuer_chain,
        .qe_identity_issuer_chain_size = input->qe_identity_issuer_chain_size,
        .qe_identity = (char *)input->qe_identity,
        .qe_identity_size = input->qe_identity_size,
    };

    uint32_t supplemental_size = 0;
    quote3_error_t qvl_error = sgx_qv_get_quote_supplemental_data_size(&supplemental_size);
    if (qvl_error != SGX_QL_SUCCESS) {
        return OUTBE_QVL_WRAPPER_QVL_ERROR;
    }
    if (supplemental_size != sizeof(sgx_ql_qv_supplemental_t)) {
        return OUTBE_QVL_WRAPPER_UNSUPPORTED_ABI;
    }

    sgx_ql_qv_supplemental_t supplemental;
    memset(&supplemental, 0, sizeof(supplemental));
    sgx_ql_qv_result_t aggregate_status = SGX_QL_QV_RESULT_UNSPECIFIED;
    uint32_t collateral_expiration_status = UINT32_MAX;

    qvl_error = sgx_qv_verify_quote(
        quote,
        quote_size,
        &collateral,
        (time_t)expiration_check_date,
        &collateral_expiration_status,
        &aggregate_status,
        NULL,
        supplemental_size,
        (uint8_t *)&supplemental);

    output->aggregate_status = (uint32_t)aggregate_status;
    output->collateral_expiration_status = collateral_expiration_status;
    if (qvl_error != SGX_QL_SUCCESS) {
        return OUTBE_QVL_WRAPPER_QVL_ERROR;
    }

    output->supplemental_major_version = supplemental.major_version;
    output->supplemental_minor_version = supplemental.minor_version;
    output->earliest_issue_date = (int64_t)supplemental.earliest_issue_date;
    output->latest_issue_date = (int64_t)supplemental.latest_issue_date;
    output->earliest_expiration_date = (int64_t)supplemental.earliest_expiration_date;
    output->tcb_evaluation_data_number = supplemental.tcb_eval_ref_num;
    memcpy(output->root_key_id, supplemental.root_key_id, sizeof(output->root_key_id));
    memcpy(
        output->tcb_cpusvn,
        supplemental.tcb_cpusvn.svn,
        sizeof(output->tcb_cpusvn));
    output->tcb_pce_isvsvn = supplemental.tcb_pce_isvsvn;
    output->pce_id = supplemental.pce_id;
    output->tee_type = supplemental.tee_type;
    output->sgx_type = supplemental.sgx_type;
    memcpy(
        output->platform_instance_id,
        supplemental.platform_instance_id,
        sizeof(output->platform_instance_id));
    output->dynamic_platform = (int32_t)supplemental.dynamic_platform;
    output->cached_keys = (int32_t)supplemental.cached_keys;
    output->smt_enabled = (int32_t)supplemental.smt_enabled;
    memcpy(output->advisory_ids, supplemental.sa_list, sizeof(output->advisory_ids));
    output->qe_status = (uint32_t)supplemental.qe_iden_status;
    output->qe_tcb_evaluation_data_number = supplemental.qe_iden_tcb_eval_ref_num;
    return OUTBE_QVL_WRAPPER_OK;
}
