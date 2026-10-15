"""Reproduce the P0 source inventory, not an implementation or correctness test."""
import hashlib,json,os,subprocess,tomllib
from pathlib import Path
root=Path(__file__).resolve().parents[3]
with (root/'Cargo.lock').open('rb') as lockfile:
    root_lock=tomllib.load(lockfile)
source=next(p['source'] for p in root_lock['package'] if p['name']=='commonware-cryptography')
revision=source.rsplit('#',1)[1]
assert revision=='d476a2361ce6840d2b9d0aa6fb30a924429046d4',source
cargo_cache=Path(os.environ.get('CARGO_HOME',Path.home()/'.cargo'))
candidates=cargo_cache.joinpath('git/checkouts').glob('*/'+revision[:7]+'/cryptography/Cargo.toml')
sdk=next(p.parent.parent for p in candidates if subprocess.check_output(['git','rev-parse','HEAD'],cwd=p.parent,text=True).strip()==revision)

anchors=[
('S01','vendor/heartwood/crates/radicle-crypto/src/lib.rs','pub trait Signer'),
('S02','vendor/heartwood/crates/radicle-crypto/src/lib.rs','pub type PublicKeyBytes'),
('S03','vendor/heartwood/crates/radicle-crypto/src/ssh.rs','pub fn to_pem'),
('S04','vendor/heartwood/crates/radicle/src/identity/did.rs','pub struct Did'),
('S05','vendor/heartwood/crates/radicle/src/cob/identity.rs','pub fn id(&self) -> RepoId'),
('S06','vendor/heartwood/crates/radicle/src/identity/doc.rs','pub fn sign('),
('S07','vendor/heartwood/crates/radicle-cob/src/backend/git/change.rs','ExtendedSignature::try_sign'),
('S08','vendor/heartwood/crates/radicle-cob/src/change/store.rs','pub fn valid_signatures'),
('S09','vendor/heartwood/crates/radicle/src/storage/refs/sigrefs/write.rs','let canonical = self.refs.canonical();'),
('S10','vendor/heartwood/crates/radicle/src/storage/refs/sigrefs/read.rs','.verify(&self.refs.canonical(), &self.signature)'),
('S11','vendor/heartwood/crates/radicle/src/storage.rs','pub type RemoteId'),
('S12','vendor/heartwood/crates/radicle-protocol/src/wire.rs','impl Decode for PublicKey'),
('S13','vendor/heartwood/crates/radicle-node/src/wire.rs','type WireSession'),
('S14','vendor/heartwood/crates/radicle/src/cob/patch.rs','Action::Review { .. } => Authorization::Allow'),
('S15','vendor/heartwood/crates/radicle/src/cob/patch.rs','Action::Merge { revision, commit } =>'),
('S16','vendor/heartwood/crates/radicle-cob/src/backend/git/change.rs','let author = repo.signature()?;'),
('S17','crates/system/vote/src/runtime.rs','ensure_active_validator(self.storage.clone(), voter)?;'),
('S18','crates/system/vote/src/state.rs','pub fn active_validator_addresses'),
('S19','crates/system/validatorset/src/runtime/queries.rs','pub fn get_active_consensus_set'),
('S20','crates/system/validatorset/src/runtime/queries.rs','pub fn current_epoch_u64'),
('S21','crates/blockchain/radicle/src/manager/snapshot.rs','.state_by_block_hash(block.hash)'),
('S22','crates/blockchain/consensus/src/hybrid.rs','//! Hybrid signing scheme'),
('S23','crates/blockchain/consensus/src/hybrid/vrf_material.rs','pub(crate) fn sign_seed'),
('S24','crates/blockchain/consensus/src/dkg_manager/ceremony.rs','let mut logs = Logs::<MinSig'),
('S25','crates/system/radicleregistry/src/runtime.rs','pub fn register_repository'),
('S26','scripts/genesis_seed_alloc.py','def seed_radicle_registry'),
('S27','crates/blockchain/evm/src/handlers.rs','static ACTIVE_VOTE_TARGETS'),
('S28','vendor/heartwood/crates/radicle-git-metadata/src/commit/headers.rs',"pub enum Signature<'a>"),
('C01',str(sdk/'cryptography/src/bls12381/primitives/ops/threshold.rs'),'pub fn sign_message'),
('C02',str(sdk/'cryptography/src/bls12381/primitives/ops/threshold.rs'),'pub fn recover<'),
('C03',str(sdk/'cryptography/src/bls12381/primitives/group.rs'),'pub const G1_MESSAGE'),
('C04',str(sdk/'cryptography/src/bls12381/dkg/feldman_desmedt.rs'),'pub fn new<M: Faults>'),
('C05',str(sdk/'utils/src/faults.rs'),'fn quorum('),
('C06',str(sdk/'utils/src/lib.rs'),'pub fn union_unique'),
('S29','crates/blockchain/primitives/src/reshare_artifact.rs','pub struct OutbeBlockArtifacts'),
('S30','crates/blockchain/primitives/src/consensus.rs','pub const OUTBE_MAX_EXTRA_DATA_SIZE'),
('S31','crates/blockchain/evm/src/config/system_txs.rs','fn system_tx_expectations_for_block'),
('S32','crates/blockchain/consensus/src/proof/constants.rs','pub fn hybrid_seed_namespace'),
('S33','crates/blockchain/consensus/src/dkg_manager.rs','let info = Info::<MinSig'),
]
rows=[]
for key,path,needle in anchors:
 p=root/path
 raw=p.read_bytes(); lines=raw.decode().splitlines(); hits=[i+1 for i,s in enumerate(lines) if needle in s]
 assert hits,(key,path,needle)
 rows.append(dict(id=key,path=path,anchor=needle,lines=hits,sha256=hashlib.sha256(raw).hexdigest()))
cmd=['rg','-l',r'\b(PublicKey|NodeId|RemoteId|ActorId|Signature|Signer)\b',
 'vendor/heartwood/crates/radicle/src','vendor/heartwood/crates/radicle-cob/src',
 'vendor/heartwood/crates/radicle-protocol/src','vendor/heartwood/crates/radicle-fetch/src',
 'vendor/heartwood/crates/radicle-node/src','vendor/heartwood/crates/radicle-remote-helper/src']
paths=subprocess.run(cmd,cwd=root,capture_output=True,text=True,check=True).stdout.splitlines()
result=dict(base_sha=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
 commonware_revision='d476a2361ce6840d2b9d0aa6fb30a924429046d4',anchors=rows,
 coupling_inventory_note='Search candidates, not a claim every file must change. Transport stays Ed25519; classify before edits.',
 coupling_candidates=[dict(path=p,sha256=hashlib.sha256((root/p).read_bytes()).hexdigest()) for p in sorted(paths)])
output=root/'plans/radicle-patch-vote/evidence/outbe-chain-pjz.1/source-audit.json'
output.write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
print(f'{len(rows)} checked anchors; {len(paths)} coupling candidate files; base {result["base_sha"]}')
