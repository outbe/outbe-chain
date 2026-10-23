# Validator key custody

Each validator role selects its own custody method. The roles are BLS consensus, validator EVM, and Reth P2P / NodeHost. The methods are plaintext, passphrase-encrypted file, and OS keychain. The default for each role is plaintext, including production.

Use these flags:

- `--bls-key-backend` and `--bls-passphrase` (`BLS_PASSPHRASE`)
- `--evm-key-backend` and `--evm-passphrase` (`EVM_PASSPHRASE`)
- `--reth-key-backend` and `--reth-passphrase` (`RETH_P2P_PASSPHRASE`)

The accepted backend names are `plaintext`, `encrypted`, and `os-level`.

Generation writes `validator-N/evm-key.hex` and `validator-N/reth-p2p-secret.hex`. The live node reads the Reth identity from the datadir file `discovery-secret`. Place the generated Reth custody file at that datadir path before start.

Encrypted and OS-keychain modes do not write a second plaintext copy. A missing protected Reth file stops startup. The node does not generate a replacement key.

The OS keychain uses the service name `outbe-chain`. Availability depends on the host keychain service.

Passphrases do not appear in logs or error text.
