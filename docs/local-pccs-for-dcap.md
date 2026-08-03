# Local PCCS for DCAP quote generation

This runbook starts an Intel PCCS v4 on `localhost:8081` so AESM and Gramine
can generate a real SGX DCAP quote on the Outbe development host.

PCCS is used only to acquire or renew quote collateral. It is **not** part of
consensus verification: consensus must use the submitted evidence, active
policy and block timestamp without QPL/PCCS, network access, host verdicts or
the wall clock.

## Prerequisites

- Ubuntu 24.04 on `x86_64` with `/dev/sgx_enclave` and
  `/dev/sgx_provision`.
- Active `aesmd` and the Intel SGX apt repository.
- Outbound HTTPS access to Intel PCS.
- A dedicated 32-character key from the
  [Intel PCS subscription portal][intel-pcs-subscription].
- Two strong, distinct PCCS admin/user passwords.

Do not put the PCS key, passwords, TLS private key or platform identifiers in
the repository, shell history, logs or Telegram.

## 1. Check the host and install PCCS

Run from the repository root:

```sh
test "$(uname -m)" = x86_64
test -e /dev/sgx_enclave
test -e /dev/sgx_provision
systemctl is-active --quiet aesmd

dcap_version="$(
  jq -r '.system_packages["libsgx-dcap-quote-verify"]' \
    release/project-toolchain-v1.json
)"
test "$dcap_version" = "1.26.100.1-noble1"
apt-cache policy sgx-dcap-pccs
sudo apt-get install "sgx-dcap-pccs=$dcap_version"
```

The package candidate must equal `dcap_version`. If no candidate exists,
configure Intel's Ubuntu 24.04 repository using the
[Intel SGX installation guide][intel-sgx-install].

The installer is interactive. Use these answers:

| Prompt | Answer |
|---|---|
| Run database migrations | `Y` |
| HTTP/HTTPS proxy | blank unless required |
| Configure PCCS now | `Y` |
| HTTPS port | `8081` |
| Local connections only | `Y` |
| Intel PCS API key | the dedicated key |
| Cache fill method | `LAZY` |
| Administrator/user passwords | distinct strong secrets |
| Generate insecure HTTPS certificate | `Y` for this loopback-only capture host |

If `npm audit` offers `audit fix` or `audit fix --force`, do not select them:
they rewrite Intel's packaged dependency set. Review the finding and either
continue with the package lockfile or abort for a package update.

The PCS key is stored in
`/opt/intel/sgx-dcap-pccs/config/default.json`, owned by `pccs` with mode
`0640`. Never copy this file into diagnostics or source control.

## 2. Configure the local QPL client

Edit `/etc/sgx_default_qcnl.conf` with `sudoedit` and set:

```json
{
  "pccs_url": "https://localhost:8081/sgx/certification/v4/",
  "use_secure_cert": false,
  "local_cache_only": false
}
```

Keep PCCS bound to `127.0.0.1`. Disabling certificate verification is allowed
here only because this is a disposable, loopback-only evidence-capture setup
using Intel's installer-generated self-signed certificate. For any shared or
production PCCS, install a certificate trusted by the host, set
`use_secure_cert` to `true`, bind only the required interface and apply normal
firewall/access controls.

Restart the services:

```sh
sudo systemctl restart pccs
sudo systemctl restart aesmd
sudo systemctl status pccs aesmd --no-pager
ss -ltn '( sport = :8081 )'
```

## 3. Verify PCCS and generate a real quote

First test a real PCCS/PCS request:

```sh
curl --insecure --fail --silent --show-error \
  --output /tmp/outbe-pccs-root-ca.crl \
  https://localhost:8081/sgx/certification/v4/rootcacrl
test -s /tmp/outbe-pccs-root-ca.crl
```

Then run the repository hardware smoke:

```sh
cd /home/ubuntu/outbe-chain
cargo build --release --locked --bin outbe-tee-enclave
scripts/sgx-smoke.sh target/release/outbe-tee-enclave
```

Success includes:

```text
PASS: real DCAP quote generated
SGX SMOKE: PASS (real SGX execution + EGETKEY sealing + real DCAP quote)
```

This proves that PCK provisioning works. It does **not** complete I1: the
smoke uses probe report data. The next implementation step must generate a
quote from the exact `RegistrationIntentV1::report_data()`, capture complete
canonical collateral and verify the frozen fixture through
`verify_dcap_evidence`.

## Troubleshooting

```sh
sudo journalctl -u pccs -n 200 --no-pager
sudo journalctl -u aesmd -n 200 --no-pager
```

- Connection refused on 8081: PCCS is not running or not bound to loopback.
- Intel PCS HTTP 401/403: the subscription or API key is invalid.
- `SGX_QL_NETWORK_ERROR` (`0xe019`): QPL could not obtain PCK/collateral from
  PCCS/PCS.
- `AESM service returned error 12`: inspect both logs; on this host it was the
  downstream symptom of the unavailable PCCS.
- Quote generated but policy rejects TCB status: confirm it is not one of the
  accepted testnet Platform results (`UpToDate`, `SWHardeningNeeded`, or
  `ConfigurationAndSWHardeningNeeded`), then update BIOS,
  microcode/platform configuration or use another supported SGX host. QE still
  must be exactly `UpToDate`.

[intel-pcs-subscription]: https://api.portal.trustedservices.intel.com/products#product=liv-intel-software-guard-extensions-provisioning-certification-service
[intel-sgx-install]: https://cc-enabling.trustedservices.intel.com/intel-sgx-sw-installation-guide-linux/02/installation_instructions/
