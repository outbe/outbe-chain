//! Exercise startup selection through its real disk loaders, before any DKG traffic.
use super::*;

#[derive(Clone, Debug)]
struct NoDkgTraffic;

impl LimitedSender for NoDkgTraffic {
    type PublicKey = bls12381::PublicKey;
    type Checked<'a> = Self;

    fn check(
        &mut self,
        _: Recipients<Self::PublicKey>,
    ) -> std::result::Result<Self::Checked<'_>, SystemTime> {
        panic!("material recovery must not start DKG traffic")
    }
}

impl CheckedSender for NoDkgTraffic {
    type PublicKey = bls12381::PublicKey;

    fn recipients(&self) -> Vec<Self::PublicKey> {
        panic!("material recovery must not start DKG traffic")
    }

    fn send(self, _: impl Into<IoBufs> + Send, _: bool) -> Unreliable<Feedback> {
        panic!("material recovery must not start DKG traffic")
    }
}

impl Receiver for NoDkgTraffic {
    type PublicKey = bls12381::PublicKey;
    type Error = Infallible;

    async fn recv(&mut self) -> std::result::Result<Message<Self::PublicKey>, Self::Error> {
        panic!("material recovery must not start DKG traffic")
    }
}

struct MaterialFixture {
    keys: Vec<bls12381::PrivateKey>,
    output: Output<MinSig, bls12381::PublicKey>,
    share: Share,
    polynomial: Sharing<MinSig>,
    validators: validators::ValidatorSet,
}

fn default_args() -> ConsensusArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        consensus: ConsensusArgs,
    }
    Cli::parse_from(["startup-material-test"]).consensus
}

fn fresh_context() -> StartupDkgContext {
    StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    }
}

impl MaterialFixture {
    fn new() -> Self {
        let (keys, _, output, share, polynomial) = run_test_dkg_complete();
        let validators = validators::ValidatorSet {
            public_keys: keys.iter().map(|key| key.public_key()).collect(),
            addresses: vec![
                Address::with_last_byte(1),
                Address::with_last_byte(2),
                Address::with_last_byte(3),
            ],
            p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
        };
        Self {
            keys,
            output,
            share,
            polynomial,
            validators,
        }
    }

    fn boundary_context(&self, finalized: bool) -> StartupDkgContext {
        StartupDkgContext {
            last_execution_height: 100,
            last_consensus_finalized_height: 100,
            recovered_boundary_finalized: finalized,
            recovered_vrf_group_public_key: Some(vrf_group_public_key_hash(&self.polynomial)),
            recovered_dkg_output_hash: Some(dkg_manager::dkg_output_hash(&self.output)),
            genesis_formation_proven: false,
        }
    }

    fn save(&self, path: &std::path::Path) {
        save_dkg_state(
            DkgStateStore::new(path, &bls::KeyBackend::Plaintext),
            DkgStateMaterial {
                share: &self.share,
                polynomial: &self.polynomial,
                output: &self.output,
            },
        )
        .unwrap();
    }

    fn save_pending(&self, path: &std::path::Path) {
        save_pending_dkg_state(
            DkgStateStore::new(path, &bls::KeyBackend::Plaintext),
            DkgStateMaterial {
                share: &self.share,
                polynomial: &self.polynomial,
                output: &self.output,
            },
        )
        .unwrap();
    }

    fn cli(&self, path: &std::path::Path) -> ConsensusArgs {
        self.save(path);
        ConsensusArgs {
            signing_share: Some(path.join(DKG_SHARE_FILE)),
            public_polynomial: Some(path.join(DKG_POLYNOMIAL_FILE)),
            dkg_output: Some(path.join(DKG_OUTPUT_FILE)),
            ..default_args()
        }
    }

    fn obtain(
        &self,
        args: &ConsensusArgs,
        context: StartupDkgContext,
    ) -> Result<ThresholdMaterial> {
        commonware_runtime::tokio::Runner::default().start(|clock| async move {
            obtain_threshold_material(
                clock,
                &bls::KeyBackend::Plaintext,
                super::super::dkg::startup::ThresholdMaterialRequest {
                    args,
                    signing_key: self.keys[0].clone(),
                    validator_set: &self.validators,
                    context,
                },
                NoDkgTraffic,
                NoDkgTraffic,
            )
            .await
        })
    }

    fn assert_ready(&self, material: ThresholdMaterial) {
        let ThresholdMaterial::Ready {
            signing_share,
            polynomial,
            last_dkg_output,
            bootstrap_from_live_dkg,
        } = material
        else {
            panic!("expected recovered signer material");
        };
        assert_eq!(
            commonware_codec::Encode::encode(&signing_share),
            commonware_codec::Encode::encode(&self.share)
        );
        assert_eq!(
            commonware_codec::Encode::encode(&polynomial),
            commonware_codec::Encode::encode(&self.polynomial)
        );
        assert_eq!(last_dkg_output.as_ref(), Some(&self.output));
        assert!(!bootstrap_from_live_dkg);
    }
}

#[test]
fn saved_material_wins_before_broken_pending_and_cli_inputs() {
    let fixture = MaterialFixture::new();
    let dir = tempfile::tempdir().unwrap();
    fixture.save(dir.path());
    std::fs::write(dir.path().join(DKG_PENDING_SHARE_FILE), b"corrupt").unwrap();
    let args = ConsensusArgs {
        keys_dir: Some(dir.path().to_path_buf()),
        signing_share: Some(dir.path().join("absent-share")),
        public_polynomial: Some(dir.path().join("absent-polynomial")),
        dkg_output: Some(dir.path().join("absent-output")),
        ..default_args()
    };
    fixture.assert_ready(
        fixture
            .obtain(&args, fixture.boundary_context(true))
            .unwrap(),
    );
    assert_eq!(
        std::fs::read(dir.path().join(DKG_PENDING_SHARE_FILE)).unwrap(),
        b"corrupt"
    );
}

#[test]
fn matching_pending_recovers_corrupt_saved_and_promotes_only_after_finalization() {
    let fixture = MaterialFixture::new();
    for finalized in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        fixture.save_pending(dir.path());
        std::fs::write(dir.path().join(DKG_SHARE_FILE), b"corrupt").unwrap();
        for name in [
            DKG_PENDING_BOUNDARY_FILE,
            DKG_PENDING_BOUNDARY_TMP_FILE,
            DKG_DEALER_RETRY_FILE,
            DKG_PLAYER_RETRY_FILE,
        ] {
            std::fs::write(dir.path().join(name), b"durable marker").unwrap();
        }
        let args = ConsensusArgs {
            keys_dir: Some(dir.path().to_path_buf()),
            ..default_args()
        };
        fixture.assert_ready(
            fixture
                .obtain(&args, fixture.boundary_context(finalized))
                .unwrap(),
        );
        for name in [
            DKG_PENDING_SHARE_FILE,
            DKG_PENDING_POLYNOMIAL_FILE,
            DKG_PENDING_OUTPUT_FILE,
            DKG_PENDING_BOUNDARY_FILE,
            DKG_PENDING_BOUNDARY_TMP_FILE,
            DKG_DEALER_RETRY_FILE,
            DKG_PLAYER_RETRY_FILE,
        ] {
            assert_eq!(dir.path().join(name).exists(), !finalized, "{name}");
        }
        if finalized {
            let (_, polynomial, output) =
                load_saved_dkg_state(dir.path(), &bls::KeyBackend::Plaintext)
                    .unwrap()
                    .unwrap();
            assert_eq!(
                commonware_codec::Encode::encode(&polynomial),
                commonware_codec::Encode::encode(&fixture.polynomial)
            );
            assert_eq!(output, fixture.output);
        } else {
            assert_eq!(
                std::fs::read(dir.path().join(DKG_SHARE_FILE)).unwrap(),
                b"corrupt"
            );
        }
    }
}

#[test]
fn stale_saved_material_is_replaced_by_matching_pending() {
    let stale = MaterialFixture::new();
    let current = MaterialFixture::new();
    let dir = tempfile::tempdir().unwrap();
    stale.save(dir.path());
    current.save_pending(dir.path());
    let args = ConsensusArgs {
        keys_dir: Some(dir.path().to_path_buf()),
        ..default_args()
    };
    current.assert_ready(
        current
            .obtain(&args, current.boundary_context(true))
            .unwrap(),
    );
    assert_eq!(
        load_saved_dkg_state(dir.path(), &bls::KeyBackend::Plaintext)
            .unwrap()
            .unwrap()
            .2,
        current.output
    );
}

#[test]
fn failed_pending_promotion_preserves_pending_and_retry_evidence() {
    let fixture = MaterialFixture::new();
    for fail_during_retry_cleanup in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        fixture.save_pending(dir.path());
        std::fs::write(dir.path().join(DKG_PENDING_BOUNDARY_FILE), b"boundary").unwrap();
        let obstruction = if fail_during_retry_cleanup {
            DKG_DEALER_RETRY_FILE
        } else {
            DKG_OUTPUT_FILE
        };
        std::fs::create_dir(dir.path().join(obstruction)).unwrap();
        let args = ConsensusArgs {
            keys_dir: Some(dir.path().to_path_buf()),
            ..default_args()
        };
        let error = fixture
            .obtain(&args, fixture.boundary_context(true))
            .err()
            .unwrap()
            .to_string();
        if fail_during_retry_cleanup {
            assert!(
                error.contains("failed to retire recovered DKG retry state"),
                "{error}"
            );
            assert_eq!(
                load_saved_dkg_state(dir.path(), &bls::KeyBackend::Plaintext)
                    .unwrap()
                    .unwrap()
                    .2,
                fixture.output
            );
        } else {
            assert!(
                error.contains("failed to promote pending DKG state after boundary finalization"),
                "{error}"
            );
        }
        for name in [
            DKG_PENDING_SHARE_FILE,
            DKG_PENDING_POLYNOMIAL_FILE,
            DKG_PENDING_OUTPUT_FILE,
            DKG_PENDING_BOUNDARY_FILE,
        ] {
            assert_eq!(
                dir.path().join(name).exists(),
                !fail_during_retry_cleanup,
                "{name}"
            );
        }
        assert!(dir.path().join(obstruction).is_dir());
    }
}

#[test]
fn corrupt_saved_without_eligible_pending_never_falls_through_to_cli_or_genesis() {
    let fixture = MaterialFixture::new();
    for pending in ["absent", "corrupt", "unanchored"] {
        let dir = tempfile::tempdir().unwrap();
        let mut args = fixture.cli(&dir.path().join("manual"));
        let local = dir.path().join("local");
        std::fs::create_dir(&local).unwrap();
        args.keys_dir = Some(local.clone());
        std::fs::write(local.join(DKG_SHARE_FILE), b"corrupt").unwrap();
        if pending == "corrupt" {
            std::fs::write(local.join(DKG_PENDING_SHARE_FILE), b"corrupt").unwrap();
        }
        if pending == "unanchored" {
            fixture.save_pending(&local);
        }
        for context in [fresh_context(), fixture.boundary_context(true)] {
            if pending == "unanchored" && context.recovered_dkg_output_hash.is_some() {
                continue;
            }
            let error = fixture.obtain(&args, context).err().unwrap().to_string();
            assert!(
                error.contains(
                    "saved DKG state failed to load and pending state could not be promoted"
                ),
                "{pending}: {error}"
            );
        }
    }
}

#[test]
fn local_boundary_recovery_requires_material_or_explicit_shareless_provisioning() {
    let fixture = MaterialFixture::new();
    let dir = tempfile::tempdir().unwrap();
    let mut args = fixture.cli(&dir.path().join("manual"));
    args.keys_dir = Some(dir.path().join("empty-local"));
    let error = fixture
        .obtain(&args, fixture.boundary_context(true))
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("saved and pending DKG material do not match"),
        "{error}"
    );
    args.signing_share = None;
    let error = fixture
        .obtain(&args, fixture.boundary_context(false))
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains(
            "pending DKG boundary snapshot was recovered but matching DKG material is unavailable"
        ),
        "{error}"
    );
    // Preserve the explicit verifier-join contract, including its original public-material policy.
    let context = StartupDkgContext {
        recovered_vrf_group_public_key: Some(B256::ZERO),
        recovered_dkg_output_hash: Some(B256::ZERO),
        ..fixture.boundary_context(true)
    };
    let ThresholdMaterial::VerifierOnly {
        polynomial,
        last_dkg_output,
    } = fixture.obtain(&args, context).unwrap()
    else {
        panic!("expected shareless verifier")
    };
    assert_eq!(
        commonware_codec::Encode::encode(&polynomial),
        commonware_codec::Encode::encode(&fixture.polynomial)
    );
    assert_eq!(last_dkg_output.as_ref(), Some(&fixture.output));
}

#[test]
fn cli_signer_requires_current_and_consistent_material_when_output_is_bound() {
    let fixture = MaterialFixture::new();
    let other = MaterialFixture::new();
    let dir = tempfile::tempdir().unwrap();
    let args = fixture.cli(&dir.path().join("manual"));
    other.save(&dir.path().join("other"));
    for context in [fresh_context(), fixture.boundary_context(true)] {
        fixture.assert_ready(fixture.obtain(&args, context).unwrap());
    }
    let mut no_output = args.clone();
    no_output.dkg_output = None;
    let ThresholdMaterial::Ready {
        last_dkg_output,
        bootstrap_from_live_dkg,
        ..
    } = fixture.obtain(&no_output, fresh_context()).unwrap()
    else {
        panic!("expected manually provisioned signer")
    };
    assert!(last_dkg_output.is_none());
    assert!(!bootstrap_from_live_dkg);
    let error = fixture
        .obtain(&no_output, fixture.boundary_context(true))
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("CLI DKG material lacks the output required"),
        "{error}"
    );
    for context in [
        StartupDkgContext {
            recovered_vrf_group_public_key: Some(B256::ZERO),
            ..fixture.boundary_context(true)
        },
        StartupDkgContext {
            recovered_dkg_output_hash: Some(B256::ZERO),
            ..fixture.boundary_context(true)
        },
    ] {
        let error = fixture.obtain(&args, context).err().unwrap().to_string();
        assert!(error.contains("CLI DKG material is stale"), "{error}");
    }
    let mut inconsistent = args.clone();
    inconsistent.dkg_output = Some(dir.path().join("other").join(DKG_OUTPUT_FILE));
    let error = fixture
        .obtain(&inconsistent, fresh_context())
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("CLI DKG material triplet is inconsistent"),
        "{error}"
    );
    for (field, message) in [
        (0, "failed to load BLS signing share"),
        (1, "failed to load BLS public polynomial"),
        (2, "failed to load BLS DKG output"),
    ] {
        let mut broken = args.clone();
        let absent = Some(dir.path().join("absent"));
        match field {
            0 => broken.signing_share = absent,
            1 => broken.public_polynomial = absent,
            _ => broken.dkg_output = absent,
        }
        let error = fixture
            .obtain(&broken, fresh_context())
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn manual_shareless_material_precedes_genesis_and_incomplete_pairs_do_not() {
    let fixture = MaterialFixture::new();
    let dir = tempfile::tempdir().unwrap();
    let mut args = fixture.cli(dir.path());
    args.signing_share = None;
    assert!(matches!(
        fixture.obtain(&args, fresh_context()).unwrap(),
        ThresholdMaterial::VerifierOnly { .. }
    ));
    for missing_output in [true, false] {
        let mut incomplete = args.clone();
        if missing_output {
            incomplete.dkg_output = None;
        } else {
            incomplete.public_polynomial = None;
        }
        let error = fixture
            .obtain(&incomplete, fixture.boundary_context(true))
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("no current threshold material is available"),
            "{error}"
        );
    }
}

#[test]
fn interactive_dkg_requires_empty_proven_genesis_and_local_membership() {
    let mut fixture = MaterialFixture::new();
    let args = default_args();
    for context in [
        StartupDkgContext {
            genesis_formation_proven: false,
            ..fresh_context()
        },
        StartupDkgContext {
            last_execution_height: 1,
            ..fresh_context()
        },
        StartupDkgContext {
            last_consensus_finalized_height: 1,
            ..fresh_context()
        },
        fixture.boundary_context(true),
    ] {
        let error = fixture.obtain(&args, context).err().unwrap().to_string();
        assert!(
            error.contains("no current threshold material is available"),
            "{error}"
        );
    }
    let error = fixture
        .obtain(&args, fresh_context())
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("DKG participant recovery requires --consensus.keys-dir"),
        "{error}"
    );
    fixture.validators.public_keys.remove(0);
    let error = fixture
        .obtain(&args, fresh_context())
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("no current threshold material is available"),
        "{error}"
    );
}
