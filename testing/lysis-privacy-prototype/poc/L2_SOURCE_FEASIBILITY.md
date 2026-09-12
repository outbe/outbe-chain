# Реальный current P_L2 FullProof: локальный путь и границы совместимости

Дата: 2026-09-11. Scope: подготовка fresh canonical TributeDraft fixtures, настоящий `outbe.full_proof@1.1.0`, его проверка и linkage с отдельным P_link. Production файлы и другие PoC файлы не изменялись. Graph MCP отсутствует; выводы основаны на exact source, не graph coverage.

**Вывод:** bb/nargo в PATH не нужны. Pinned `outbe-zk-backend` решает готовый ACIR через Rust ACVM и вызывает native Barretenberg FFI. Compiled ABI/bytecode/VK, native `.a`, Rust backend artifacts и SRS доступны локально. Это конкретный путь к настоящему current FullProof; новый proof в рамках этой ограниченной research-задачи **не генерировался**, RSS и runtime не измерялись.

## 1. Pin и найденные артефакты

В [production Cargo.toml:279–282](../../../Cargo.toml#L279) задан `outbe-circuits` tag `v0.14.0`; [Cargo.lock:9909–9927](../../../Cargo.lock#L9909) фиксирует commit `984d57ed0d2f014a1a74d0b3b4b0769801957791`. Ниже `C` обозначает локальный [checkout этого commit](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e). Package version в его workspace — `0.11.0`; нельзя заменять git pin догадкой по semver пакета.

| Компонент | Точный вариант / наблюдение |
|---|---|
| FullProof | `outbe.full_proof@1.1.0`, active в `C/crates/outbe-zk-canonical/circuits/manifest.toml:91–95` |
| Noir | `v1.0.0-beta.22`, commit `c57152f91260ecdb9faad4efc20abb14b6d2ece7` |
| Barretenberg Rust | `5.0.0-nightly.20260522`; production lock checksum `216c63f05c86241de57a9084206954e587ae6370cedca3c1d4b96e86aff72a3f` |
| Prover field/API | BN254 Fr, arkworks **0.6**; parent P_link на ark0.5 должен обмениваться canonical bytes либо использовать явно разные dependency aliases |
| Native `.a` | `/Users/sakor/outbe-io/outbe-chain/target/release/build/barretenberg-rs-b78e1bff630b7ec2/out/libbb-external.a`; также найдены debug copies |
| Compiled Rust backend | `target/release/deps/liboutbe_zk_backend-467ad45ef61f51fa.rlib` и `.rmeta`; наличие cache не гарантирует reuse после изменения features/profile/toolchain |
| SRS | `/Users/sakor/.bb-crs/bn254_g1.dat`, **67 108 928 B**; prefix hashes для 8193, 65537, 131073, 262145, 524289 и 1048577 points совпали с pinned таблицей backend |

Frozen FullProof artifacts находятся в `C/crates/outbe-zk-canonical/resources/circuits/full_proof/1.1.0/`:

| Файл | Bytes | SHA-256 |
|---|---:|---|
| `abi.json` | 1876 | `e4ee51293cbf11495d35f05ed080b4ba241f18c114928edc2efa1348e7ca9df6` |
| `bytecode.b64` | 20920 | `05fb300bd0877f7628849201d65d7147516c33d9dfe43e1a280d4544e4e205de` |
| `circuit.vk` | 1888 | `6b5d5609be0894c5f8379803ab6421bf66b0f1c25c5667644772f2d2bf7225bc` |

Base64-decoded gzip ACIR — 15 690 B; распакованный — 182 069 B. Checked `noir/outbe-full-circuit/target/full_proof.json`: ABI и bytecode **точно совпадают** с frozen1.1.0, `noir_version` совпадает с beta22 pin. Это verification bytes/artifact sizes, не proving-key/RAM measurement.

`C/crates/outbe-zk-canonical/build.rs:1–22` явно делает read-only codegen из frozen resources; обычная компиляция Rust не запускает nargo/bb. Native linking задаёт registry source [`barretenberg-rs/build.rs:17–50`](/Users/sakor/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/barretenberg-rs-5.0.0-nightly.20260522/build.rs:17): `BB_LIB_DIR` позволяет использовать уже имеющийся `.a`, иначе build script пытается скачать release.

## 2. Что именно доказывает current FullProof

Exact source: [full Noir circuit](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e/crates/outbe-zk-canonical/noir/outbe-full-circuit/src/main.nr:16), [ownership constraints](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e/crates/outbe-zk-canonical/noir/outbe-circuit-core/src/ownership.nr:95), [Merkle fold](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e/crates/outbe-zk-canonical/noir/outbe-circuit-core/src/merkle_tree.nr:19).

```text
private = pk(x,y), signature[64], nonce,
          merkle_path_siblings[32], merkle_path_indices[32]
public  = [owner, nft_hash, binding_hash, expected_merkle_root]

pk != infinity; pk.y² = pk.x³ - 17                 # Grumpkin
owner = Poseidon2_hash3(pk.x, pk.y, nonce)
message = BE32(Poseidon2_hash3(nft_hash, nonce, binding_hash))
Schnorr_Grumpkin_verify(pk, signature, message) == true
MerkleRoot32(nft_hash, siblings, flags, domain) == expected_merkle_root
domain = Fr(BE("OUTBE_FULL_CIRCUIT"))
inner = Poseidon2_hash3(domain, left, right)
flags[i] == true means current node is LEFT       # inverse of index bit
```

Private witness **не содержит secret key sk**. Он содержит действительную Schnorr signature; стандартный builder получает её от signer, владеющего sk. Нельзя заменять точное statement неточной фразой «circuit напрямую проверяет знание secret key». Signature — 64 B `s||e`, где challenge использует Grumpkin Pedersen + Blake2s. Pinned Rust [`primitive/signature.rs:80–159`](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e/crates/outbe-protocol/src/primitive/signature.rs:80) реализует совместимое подписание; отдельный bb call ради подписи не нужен.

**FullProof не вычисляет canonical TributeDraft hash из amount/day/SU-list.** Его `nft_hash` и `binding_hash` — public inputs с ownership/signature relation. Текущий host сверяет их с hashes, вычисленными enclave из plaintext draft. No-TEE P_link обязан заменить именно эту проверку preimage/context, сохранив общий statement; сама настоящая Merkle/Schnorr proof её не добавляет.

## 3. Fresh canonical TributeDraft fixture и shared inputs P_link

Production canonical structure: [zk_claim.rs:26–74](../../../bin/outbe-tee-enclave/src/zk_claim.rs#L26). Для fixture следует воспроизвести эту структуру с `#[derive(outbe_protocol_derive::Entity)]`, а не использовать произвольный `TestNft` из backend smoke test.

```rust
#[derive(outbe_protocol_derive::Entity)]
struct TributeDraftClaim {
    #[outbe(id_seed)] id: alloy_primitives::B256,
    #[outbe(body, owner, pos=0)] derived_owner: alloy_primitives::B256,
    #[outbe(body, pos=1)] worldwide_day: u64,
    #[outbe(body, pos=2)] currency: u16,
    #[outbe(body, pos=3)] base: u64,
    #[outbe(body, pos=4)] atto: u64,
    #[outbe(body, pos=5)] su_ids: Vec<alloy_primitives::B256>,
}
```

`atto` — legacy имя remainder **0..999999**, а не 18-decimal fraction; `issuance6=base*10^6+atto`. [compute.rs:157–177](../../../bin/outbe-tee-enclave/src/compute.rs#L157).

Новый per-TD signer можно получить `Signer::<OutbeV1>::local(&mut rng)`; `NftSigner::owner_seed().derive_owner()` даёт значение `derived_owner`. Использовать свежий отдельный TD key — **совместимо** с нынешним circuit; заменить его на другой тип ключа или другую owner formula — нет. `id`, `derived_owner`, каждый `su_id` в B256 должны канонически кодировать Fr `< modulus`, не произвольный uint256 с mod reduction. Это проверяет `C/crates/outbe-protocol/src/codec.rs:280–289`.

Su IDs предварительно нормализуются `outbe_protocol::codec::sort_set::<ark_bn254::Fr,B256>(&ids)`. В entity derive `Vec<T>` автоматически становится **SortedSet**: тело включает length prefix и строго возрастающие уникальные элементы (`codec.rs:139–222`, `outbe-protocol-derive/src/lib.rs:181–191`). Нельзя использовать bare Vec encoding без длины.

```text
nft_hash = fold_hash2(td_id,
  [derived_owner, day_u64, currency_u16, base_u64, atto_u64,
   su_count, sorted_unique_su_ids...])

binding_hash = Poseidon2_hash5(
  [1, sender_address160, td_id_low128, td_id_high128, chain_id_u64])
```

Entity/hash formulas: `C/crates/outbe-protocol/src/protocol/entity.rs:28–50`, `primitive/hash.rs:27–34`, `suite.rs:98–134`. ID low/high order — именно low128 затем high128. Hash — pinned Poseidon2 sponge, не circom Poseidon1.

| Связь | Обязательная проверка |
|---|---|
| P_L2 public `owner` ↔ P_link draft `derived_owner` | Одно canonical Fr значение; это не sender EOA |
| P_L2 `nft_hash` ↔ P_link canonical preimage | Точное equality; внутри P_link доказаны field encodings, day/currency/amount/SU list |
| P_L2 `binding_hash` ↔ P_link/runtime context | Один hash от sender/td_id/chain ID; runtime не принимает произвольный prover-selected binding |
| P_L2 root ↔ source certificate | Exact root bytes в signed source commitment; local fixture root не становится trusted лишь из-за valid proof |
| P_link output `C(a6)` | Nominal integer relation и oracle inputs связаны с **этим же draft**; дополнительных PL2 public fields для nominal нет |

Для передачи ark0.6 → ark0.5 использовать canonical 32-byte big-endian values и reject `>= modulus` на принимающей стороне. Оба BN254 Fr имеют ту же математику, но это разные Rust types; scalar Baby subgroup также отдельный domain.

## 4. Короткий путь генерации и проверки

Библиотечный builder: [`full.rs:36–112`](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e/crates/outbe-zk-canonical/src/full.rs:36). Он проверяет owner, depth32/domain, сам создаёт signature и вычисляет root из path. Алгоритм для **одного листа в новом fixture tree**:

```rust
use outbe_protocol::{Codec, OutbeV1, Suite};
use outbe_protocol::protocol::entity::Entity;
use outbe_protocol::protocol::imt::Imt;
use outbe_protocol::protocol::zk::{Circuit, ProofGenerator, ProofVerifier};
use outbe_zk_canonical::full::{full_circuit_domain, FullProvable};
use outbe_zk_canonical::noir::full_proof::FullProof;
use outbe_zk_backend::barretenberg::{Barretenberg, verify_circuit};

// draft is canonical TributeDraftClaim, signer matches its derived_owner;
// sender/td_id/chain_id come from the same fixture consumed by P_link.
let binding = OutbeV1::binding(&sender, &td_id, chain_id)?;
let mut tree = Imt::<OutbeV1>::new(full_circuit_domain(), 32)?;
let path = tree.empty_inclusion_path(0);
tree.append(<TributeDraftClaim as Entity<OutbeV1>>::entity_hash(&draft)?)?;
let (w, p) = draft.derive_full_witness(&mut rng, &signer, binding, &path)?;
assert_eq!(p.expected_merkle_root, tree.root());

outbe_zk_backend::barretenberg::set_srs_path(srs_path);
let backend = Barretenberg {
    disable_zk: false,
    low_memory: true,
    max_storage_usage: None, // choose a disk budget separately if needed
};
let proof = ProofGenerator::<OutbeV1, FullProof>::generate(&backend, &w, &p)?;
assert!(ProofVerifier::<OutbeV1, FullProof>::verify(&backend, &p, &proof)?);

let fields = <FullProof as Circuit<OutbeV1>>::public_inputs(&p);
let mut combined = (fields.len() as u32).to_be_bytes().to_vec();
for f in fields { combined.extend(OutbeV1::field_to_be_bytes(&f)); }
for word in &proof.proof { assert_eq!(word.len(),32); combined.extend(word); }
assert_eq!(combined.len(), outbe_zk_canonical::full_proof::COMBINED_LEN);
let decoded = outbe_zk_canonical::full_proof::decode_public_inputs(&combined)?;
// Assert decoded public words equal accepted fixture/P_link claims here.
assert!(verify_circuit::<FullProof>(&combined)?); // same-process CRS already initialized
```

Это source-derived integration sketch, **не скомпилированный здесь helper**. Его следует разместить в отдельном ark0.6 helper package либо явно отделённом module/dependency namespace. Snippet assumes enclosing Result-returning function and prepared rng/draft/signer/IDs/SRS path.

Для нескольких leaves в одном root нельзя каждому использовать `empty_inclusion_path`: siblings уже не пустые. Нужно построить полное fixture tree, сохранить sibling paths относительно **одного финального root**, проверить каждый `path.root(nft_hash)==root`. `Imt` хранит frontier, а не произвольные historical paths; самостоятельно заданный InclusionPath должен иметь верные siblings/index/domain. One-leaf path в snippet согласован с реально выполненным append, а не с empty-tree root.

Backend [`witness.rs:57–104`](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e/crates/outbe-zk-backend/src/witness.rs:57) исполняет ACVM, требует `Solved`, сериализует witness stack и распаковывает его для FFI. [`barretenberg/mod.rs:182–238`](/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e/crates/outbe-zk-backend/src/barretenberg/mod.rs:182) загружает bytecode/VK, sizes SRS и вызывает `circuit_prove`; verify получает тот же pinned VK. `disable_zk` оставляется **false**.

Wire format production: `[u32_BE=4] || four BE32 public words || 274 BE32 proof words`, всего **8900 B**. Strict decoder находится в `C/crates/outbe-zk-canonical/src/full_proof.rs:5–25`. `verify_circuit` проверяет combined с compile-time FullProof VK; `RawVerifier` сам не выполняет strict FullProof decoding и не загружает CRS. Для separate verifier process нужен явный CRS init; в same-process generate→verify CRS уже установлен.

Для optional **backend smoke test**, не создающего canonical draft fixture, имеется точный существующий test. Следующая команда здесь **не запускалась**; она может потребовать компиляцию и prover RAM, поэтому её запускать только под принятым resource supervisor:

```sh
CIRCUITS_DIR=/Users/sakor/.cargo/git/checkouts/outbe-circuits-56de82d8dac53513/984d57e
BB_LIB_DIR=/Users/sakor/outbe-io/outbe-chain/target/release/build/barretenberg-rs-b78e1bff630b7ec2/out \
BB_CRS_PATH=/Users/sakor/.bb-crs/bn254_g1.dat \
cargo test --offline --locked --manifest-path "$CIRCUITS_DIR/Cargo.toml" \
  --target-dir /Users/sakor/outbe-io/outbe-chain/target --release \
  -p outbe-zk-backend --test barretenberg \
  full_proof_prove_verify_round_trip -- --exact --nocapture
```

Для helper dependency `outbe-zk-backend` можно отключить `default-features`; `with-network-srs` тогда отсутствует и missing local SRS становится ошибкой. `BB_LIB_DIR` нужен build script, `BB_CRS_PATH` либо `set_srs_path` — runtime. Не следует включать большой aggregate benchmark как проверку единственного FullProof.

Официальная [Noir manual workflow](https://noir-lang.org/docs/getting_started_manually) разделяет compilation/execution witness и backend proving. CLI — доступная общая альтернатива, но здесь native pinned source уже содержит оба шага без необходимости устанавливать новые версии CLI. Непроверенная команда с latest bb/noir не гарантирует совместимость frozen VK.

## 5. Memory, source trust и допустимые подписи

`Barretenberg.low_memory=true` включает file-backed polynomial mode. `max_storage_usage`/`BB_STORAGE_BUDGET` — **disk/storage budget, не жёсткий RSS limit**. Нельзя назвать такой run успешным по 512000000 B без отдельного peak measurement/supervisor. Source loader `srs.rs:207–220` читает **весь `.dat`** через `fs::read`, затем копирует needed prefix; даже для меньшего circuit это временно включает полный 67 MB file. `init_crs()` заранее грузит максимальный canonical `(1<<20)+1`; single-circuit generate может сам выбрать меньший размер через `circuit_stats`. Размер FullProof domain здесь не вычислялся. Cold witness generation, SRS, native verifier и связывание двух proof flows учитывать отдельно от размера final proof.

| Что проверяем | Что это означает |
|---|---|
| Genuine FullProof на fresh fixture root | Реально доказаны current Schnorr/owner/inclusion relation, если proof прошёл pinned verifier |
| BLS MinSig certificate fixture committee | Можно воспроизвести отдельную source-authority подпись на root; она **не заменяет FullProof** |
| FullProof на production-accepted L2 root | Требуется actual authorized root + membership witness от источника; одного local key/tree недостаточно |
| PoC knowledge-of-secret вместо embedded Schnorr | Новый circuit/VK/statement; при Baby key также новая owner formula. Это semantic substitute, нельзя маркировать `outbe.full_proof@1.1.0` |

Production source authority отдельно проверяется в [L2Registry api:16–62](../../../crates/system/l2registry/src/api.rs#L16): caller должен быть зарегистрированным L2 operator с zk_enabled, root32 подписан registered BLS MinSig key в namespace `_PSO_CHAIN_COMMITMENT_ROOT`. [TributeFactory:101–128](../../../crates/core/tributefactory/src/runtime.rs#L101) требует FullProof только для Verified branch и проверяет его root против signed root; NotRegistered/Disabled проходят другой branch. End-to-end no-TEE PoC с source proofs должен выбрать Verified-like branch и не использовать эти bypasses как успешную проверку P_L2.

В fixtures можно объявить отдельный локальный source key и registry record, sign реальный root и затем проверить **оба** свидетельства. Это корректная проверка механики под явно заданным тестовым source trust. Она не доказывает подлинность жизненных Spending Units, правильность L2 admission, отсутствие двойного включения на реальном L2 или честность его committee. Fresh canonical TD values/secret/signature/path должны быть доступны producer; произвольная чужая canonical fixture без соответствующей owner authority и path не доказуема этим рецептом.

Замена подписи внутри нового circuit на отношение `pk=sk*G` теоретически может быть отдельным proof-of-possession дизайном, но надо заново связать owner/nft_hash/binding/root, зарегистрировать VK и проверить soundness/privacy. Нельзя оставлять binding как unconstrained public word. Такая замена не является необходимой для quickest current FullProof: совместимый pure-Rust signer уже есть.

## 6. Граница выполненной проверки

**Исполнено:** exact source reads указанного checkout и production consumers; artifact SHA-256/size; equality frozen ABI/bytecode с compiled JSON; SRS prefix hashes; наличие `.a` и cached Rust artifacts; official Noir workflow lookup. Никаких новых source signatures или proofs не выдавалось за сгенерированные.

**Следующий обязательный результат helper:** fresh canonical fixture → solved ACVM witness → real ZK FullProof → typed verify → strict combined decode → production `verify_circuit::<FullProof>`; проверить mismatch owner/hash/binding/root и повреждённую signature/path. Затем joint admission проверяет те же canonical public inputs P_link и авторизованный source root. Performance и cold RSS остаются измерениями следующего запуска, а не следствием наличия кеша.
