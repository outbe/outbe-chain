# Gramine runtime image for outbe-tee-enclave.
#
# Runs the enclave binary under `gramine-direct` (Gramine LibOS WITHOUT SGX
# hardware — validates the Gramine manifest/syscall integration; it is NOT
# confidential. Real memory encryption + attestation require `gramine-sgx` on an
# SGX CPU). The binary is mounted at runtime (not baked in) so the same image
# serves any build; it only needs glibc <= the base image's (the binary requires
# GLIBC_2.38, ubuntu:24.04 ships 2.39).
#
# Build:  docker build -t outbe-tee-enclave-gramine bin/outbe-tee-enclave/gramine
# Run:    see bin/outbe-tee-enclave/gramine/run.sh
FROM --platform=linux/amd64 ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg lsb-release \
    && curl -fsSLo /etc/apt/keyrings/gramine-keyring-$(lsb_release -sc).gpg \
        https://packages.gramineproject.io/gramine-keyring-$(lsb_release -sc).gpg \
    && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/gramine-keyring-$(lsb_release -sc).gpg] https://packages.gramineproject.io/ $(lsb_release -sc) main" \
        > /etc/apt/sources.list.d/gramine.list \
    && curl -fsSLo /etc/apt/keyrings/intel-sgx-deb.asc \
        https://download.01.org/intel-sgx/sgx_repo/ubuntu/intel-sgx-deb.key \
    && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/intel-sgx-deb.asc] https://download.01.org/intel-sgx/sgx_repo/ubuntu $(lsb_release -sc) main" \
        > /etc/apt/sources.list.d/intel-sgx.list \
    && apt-get update && apt-get install -y --no-install-recommends gramine \
    && rm -rf /var/lib/apt/lists/*

COPY outbe-tee-enclave.manifest.template /app/
COPY entrypoint.sh /app/
RUN chmod +x /app/entrypoint.sh

# Bake a dev enclave signing key so every container from this image shares one
# MRSIGNER (and the entrypoint can sign the enclave at start without SGX). This
# is a DEV key — production must re-sign with the operator's own key before
# deploying to gramine-sgx (mount a key at ~/.config/gramine/enclave-key.pem or
# rebuild with your key). Regenerating the image rotates MRSIGNER.
RUN gramine-sgx-gen-private-key

WORKDIR /app
ENTRYPOINT ["/app/entrypoint.sh"]
