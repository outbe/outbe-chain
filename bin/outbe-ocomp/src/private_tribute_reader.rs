//! Reuse the co-hosted node's committed private session for calculation reads.

use alloy_primitives::U256;
use outbe_tee::TransportError;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::errors::OcompCliError;

#[derive(Debug, PartialEq, Eq)]
pub enum TeeReaderConfig {
    Unconfigured,
    Development {
        endpoint: String,
    },
    Production {
        endpoint: String,
        node_data_dir: PathBuf,
    },
}

impl TeeReaderConfig {
    /// An omitted mode retains the existing configured production-session contract.
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> Result<Self, OcompCliError> {
        let mode = lookup("OUTBE_OCOMP_TEE_SESSION_MODE");
        let mode = mode
            .as_deref()
            .map(|value| {
                value
                    .to_str()
                    .ok_or(OcompCliError::InvalidTeeSessionModeUtf8)
            })
            .transpose()?;
        if !matches!(mode, None | Some("development" | "production-node-host")) {
            return Err(OcompCliError::InvalidTeeSessionMode);
        }
        let endpoint = lookup("OUTBE_OCOMP_TEE_ENDPOINT");
        let directory = lookup("OUTBE_OCOMP_NODE_DATA_DIR");
        if mode.is_none() && endpoint.is_none() && directory.is_none() {
            return Ok(Self::Unconfigured);
        }
        if mode == Some("development") && directory.is_some() {
            return Err(OcompCliError::DevelopmentTeeReaderHasNodeDataDir);
        }
        let endpoint = endpoint
            .ok_or(OcompCliError::IncompleteTeeReaderConfig)?
            .into_string()
            .map_err(|_| OcompCliError::InvalidTeeEndpointUtf8)?;
        if endpoint.is_empty() || endpoint.trim() != endpoint {
            return Err(OcompCliError::InvalidTeeReaderEndpoint);
        }
        if mode == Some("development") {
            return Ok(Self::Development { endpoint });
        }
        let directory = directory.ok_or(OcompCliError::IncompleteTeeReaderConfig)?;
        if directory.is_empty() {
            return Err(OcompCliError::InvalidTeeReaderNodeDataDir);
        }
        Ok(Self::Production {
            endpoint,
            node_data_dir: PathBuf::from(directory),
        })
    }
}

/// Explicit development transport; never attempts a production session or creates identity state.
pub fn install_development_private_tribute_reader(
    endpoint: &str,
    chain_id: u64,
) -> Result<(), TransportError> {
    outbe_tee::call_context::set_snapshot(outbe_tee::call_context::EnclaveCallContextV1 {
        chain_id,
        ..Default::default()
    })
    .map_err(TransportError::codec)?;
    let client = outbe_tee::EnclaveClient::connect_endpoint(endpoint)?;
    outbe_tee::install_enclave_client(client, endpoint.to_owned())
        .map_err(crate::errors::transport_enclave_error)
}

/// This path reads the existing owner-only NodeHost identity. It cannot create
/// another identity, install a network key, or expose a plaintext RPC method.
pub fn install_private_tribute_reader(
    endpoint: &str,
    node_data_dir: &Path,
    chain_id: u64,
) -> Result<(), TransportError> {
    let (manifest, node_host) =
        outbe_tee::node_host::committed_node_host_session_material(node_data_dir)?;
    if manifest.chain_id != U256::from(chain_id).to_be_bytes::<32>() {
        return Err(TransportError::Codec(
            "private Tribute reader chain differs from committed NodeHost".into(),
        ));
    }
    let client = outbe_tee::connect_committed_node_host_enclave(endpoint, node_data_dir)?;
    outbe_tee::install_authorized_enclave_client(
        client,
        endpoint.to_owned(),
        node_data_dir.to_owned(),
        manifest,
        node_host,
    )
    .map_err(crate::errors::transport_enclave_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(
        mode: Option<&str>,
        endpoint: Option<&str>,
        directory: Option<&str>,
    ) -> Result<TeeReaderConfig, OcompCliError> {
        TeeReaderConfig::from_lookup(|name| match name {
            "OUTBE_OCOMP_TEE_SESSION_MODE" => mode.map(OsString::from),
            "OUTBE_OCOMP_TEE_ENDPOINT" => endpoint.map(OsString::from),
            "OUTBE_OCOMP_NODE_DATA_DIR" => directory.map(OsString::from),
            _ => None,
        })
    }

    #[test]
    fn private_reader_config_requires_explicit_development_and_preserves_production_default() {
        assert_eq!(
            config(None, None, None).unwrap(),
            TeeReaderConfig::Unconfigured
        );
        for mode in [None, Some("production-node-host")] {
            assert_eq!(
                config(mode, Some("127.0.0.1:1234"), Some("/node/data")).unwrap(),
                TeeReaderConfig::Production {
                    endpoint: "127.0.0.1:1234".into(),
                    node_data_dir: "/node/data".into()
                }
            );
        }
        assert_eq!(
            config(Some("development"), Some("127.0.0.1:1234"), None).unwrap(),
            TeeReaderConfig::Development {
                endpoint: "127.0.0.1:1234".into()
            }
        );
    }

    #[test]
    fn private_reader_config_rejects_unknown_partial_and_inconsistent_inputs() {
        assert!(matches!(
            config(Some("auto"), Some("endpoint"), Some("/data")),
            Err(OcompCliError::InvalidTeeSessionMode)
        ));
        assert!(matches!(
            config(Some("development"), Some("endpoint"), Some("/data")),
            Err(OcompCliError::DevelopmentTeeReaderHasNodeDataDir)
        ));
        for mode in [None, Some("production-node-host")] {
            assert!(matches!(
                config(mode, Some("endpoint"), None),
                Err(OcompCliError::IncompleteTeeReaderConfig)
            ));
            assert!(matches!(
                config(mode, None, Some("/data")),
                Err(OcompCliError::IncompleteTeeReaderConfig)
            ));
        }
        for mode in [Some("development"), Some("production-node-host")] {
            assert!(matches!(
                config(mode, None, None),
                Err(OcompCliError::IncompleteTeeReaderConfig)
            ));
        }
        assert!(matches!(
            config(Some("development"), Some(""), None),
            Err(OcompCliError::InvalidTeeReaderEndpoint)
        ));
        assert!(matches!(
            config(Some("development"), Some(" endpoint "), None),
            Err(OcompCliError::InvalidTeeReaderEndpoint)
        ));
        assert!(matches!(
            config(None, Some("endpoint"), Some("")),
            Err(OcompCliError::InvalidTeeReaderNodeDataDir)
        ));
        use std::os::unix::ffi::OsStringExt;
        assert!(matches!(
            TeeReaderConfig::from_lookup(|name| (name == "OUTBE_OCOMP_TEE_SESSION_MODE")
                .then(|| OsString::from_vec(vec![0xff]))),
            Err(OcompCliError::InvalidTeeSessionModeUtf8)
        ));
    }

    #[test]
    fn production_private_reader_requires_committed_manifest_without_connection_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let error = install_private_tribute_reader(
            &listener.local_addr().unwrap().to_string(),
            directory.path(),
            42,
        )
        .unwrap_err();
        assert!(matches!(error, TransportError::Codec(_)));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn development_private_reader_installs_noise_session_with_attestation_pin_and_chain_context() {
        use outbe_tee_enclave::{keys::EnclaveKeys, transport::serve_connection};
        use std::{
            os::unix::net::UnixListener,
            sync::{Arc, OnceLock},
        };

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("reader.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let keys = EnclaveKeys::new([0x51; 32], Some([0x51; 32])).unwrap();
        let attestation = keys.attestation_pub();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let offer_key = Arc::new(OnceLock::new());
            let _ = serve_connection(stream, &keys, &offer_key);
        });
        install_development_private_tribute_reader(socket.to_str().unwrap(), 42).unwrap();
        assert!(outbe_tee::is_enclave_configured());
        assert_eq!(
            outbe_tee::try_with_enclave(|session| session.attestation_pub()),
            Some(attestation)
        );
        assert_eq!(outbe_tee::call_context::resolve().unwrap().chain_id, 42);
        assert!(matches!(
            outbe_tee::try_with_enclave(
                |session| session.request(&outbe_tee::protocol::EnclaveRequest::GetPublicKeys)
            )
            .unwrap()
            .unwrap(),
            outbe_tee::protocol::EnclaveResponse::PublicKeys { .. }
        ));
        // The real private read reaches the enclave, which remains fail-closed without an offer key.
        assert!(matches!(
            outbe_tee::tribute_client::read_tribute_amounts(&[]),
            Err(TransportError::EnclaveError(_))
        ));
    }
}
