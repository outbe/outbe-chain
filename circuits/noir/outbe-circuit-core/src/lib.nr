// Shared library for ZK proof circuits.
// Modules:
// - `ownership`: sec.4.2 single-NFT ownership proof helpers --
//   Poseidon3 owner commitment + Grumpkin Schnorr signature over
//   Poseidon2(nft_hash, nonce).
// - `merkle_tree`: domain-separated Merkle node hash and root resolution from
//   a sibling path.
// - `tags`: base purpose tags; a circuit derives its own via
//   `tags::tag(ITS_DOMAIN, TAG_X)`.

pub mod ownership;
pub mod merkle_tree;
pub mod hash;
pub mod tags;
pub mod types;
