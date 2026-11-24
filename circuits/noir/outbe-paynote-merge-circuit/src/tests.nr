use super::{main, MAX_MERGE_INPUTS, PAYNOTE_POOL_ADDRESS};
use bignum::{BigNum, U256};
use outbe_circuit_core::{hash::hash_3, merkle_tree::DEPTH, types::EthAddress};
use paynote_lib::{
    empty_leaf, merkle_root, note_commitment, note_nullifier, note_sn, PAYNOTE_DOMAIN,
};

struct Fixture {
    chain_id: u64,
    pool: EthAddress,
    root: Field,
    count: u32,
    asset: EthAddress,
    nullifiers: [Field; MAX_MERGE_INPUTS],
    output: Field,
    amounts: [U256; MAX_MERGE_INPUTS],
    keys: [Field; MAX_MERGE_INPUTS],
    indices: [u32; MAX_MERGE_INPUTS],
    paths: [[Field; DEPTH]; MAX_MERGE_INPUTS],
    output_key: Field,
}

fn fixture(count: u32, amounts: [U256; MAX_MERGE_INPUTS]) -> Fixture {
    let asset = EthAddress::from_field(123);
    let mut leaves = [empty_leaf(31337); MAX_MERGE_INPUTS];
    let mut f = Fixture {
        chain_id: 31337,
        pool: EthAddress::from_field(PAYNOTE_POOL_ADDRESS),
        root: 0,
        count,
        asset,
        nullifiers: [0; MAX_MERGE_INPUTS],
        output: 0,
        amounts,
        keys: [0; MAX_MERGE_INPUTS],
        indices: [0; MAX_MERGE_INPUTS],
        paths: [[0; DEPTH]; MAX_MERGE_INPUTS],
        output_key: 99,
    };
    let mut total = U256::zero();
    for i in 0..MAX_MERGE_INPUTS {
        if i < count {
            f.keys[i] = (i + 17) as Field;
            leaves[i] = note_commitment(31337, note_sn(f.keys[i]), asset, amounts[i]);
            f.nullifiers[i] = note_nullifier(leaves[i], f.keys[i]);
            total = total + amounts[i];
        }
    }
    let pairs = [
        hash_3([PAYNOTE_DOMAIN, leaves[0], leaves[1]]),
        hash_3([PAYNOTE_DOMAIN, leaves[2], leaves[3]]),
    ];
    let mut zeros = [empty_leaf(31337); DEPTH];
    for i in 1..DEPTH {
        zeros[i] = hash_3([PAYNOTE_DOMAIN, zeros[i - 1], zeros[i - 1]]);
    }
    for i in 0..MAX_MERGE_INPUTS {
        if i < count {
            f.indices[i] = i;
            f.paths[i] = zeros;
            f.paths[i][0] = leaves[i ^ 1];
            f.paths[i][1] = pairs[(i / 2) ^ 1];
        }
    }
    f.root = merkle_root(leaves[0], 0, f.paths[0]);
    f.output = note_commitment(31337, note_sn(f.output_key), asset, total);
    f
}

fn normal(count: u32) -> Fixture {
    let mut amounts = [U256::zero(); MAX_MERGE_INPUTS];
    let values = [12, 8, 5, 7];
    for i in 0..MAX_MERGE_INPUTS {
        if i < count {
            amounts[i] = U256::from(values[i]);
        }
    }
    fixture(count, amounts)
}

fn run(f: Fixture) {
    main(
        f.chain_id,
        f.pool,
        f.root,
        f.count,
        f.asset,
        f.nullifiers,
        f.output,
        f.amounts,
        f.keys,
        f.indices,
        f.paths,
        f.output_key,
    );
}

#[test]
fn two_three_and_four_inputs() {
    run(normal(2));
    run(normal(3));
    run(normal(4));
}
#[test]
fn exact_u256_max() {
    let max = U256::from_limbs([(1 << 120) - 1, (1 << 120) - 1, (1 << 16) - 1]);
    run(
        fixture(
            2,
            [max - U256::from(1), U256::from(1), U256::zero(), U256::zero()],
        ),
    );
}
#[test(should_fail)]
fn overflow() {
    let max = U256::from_limbs([(1 << 120) - 1, (1 << 120) - 1, (1 << 16) - 1]);
    run(fixture(2, [max, U256::from(1), U256::zero(), U256::zero()]));
}
#[test(should_fail)]
fn intermediate_overflow() {
    let max = U256::from_limbs([(1 << 120) - 1, (1 << 120) - 1, (1 << 16) - 1]);
    run(
        fixture(3, [max, U256::from(2), U256::from(5), U256::zero()]),
    );
}
#[test(should_fail)]
fn zero_amount() {
    run(
        fixture(2, [U256::zero(), U256::from(1), U256::zero(), U256::zero()]),
    );
}
#[test(should_fail)]
fn modulus_zero() {
    run(
        fixture(
            2,
            [U256::from_limbs([0, 0, 1 << 16]), U256::from(1), U256::zero(), U256::zero()],
        ),
    );
}
#[test(should_fail)]
fn oversized_limb() {
    run(fixture(
        2,
        [U256::from_limbs([1 << 127, 0, 0]), U256::from(1), U256::zero(), U256::zero()],
    ));
}
#[test(should_fail)]
fn one_input() {
    run(normal(1));
}
#[test(should_fail)]
fn zero_inputs() {
    run(normal(0));
}
#[test(should_fail)]
fn five_inputs() {
    let mut f = normal(4);
    f.count = 5;
    run(f);
}
#[test(should_fail)]
fn wrong_key() {
    let mut f = normal(3);
    f.keys[1] = 100;
    run(f);
}
#[test(should_fail)]
fn wrong_chain() {
    let mut f = normal(3);
    f.chain_id = 1;
    run(f);
}
#[test(should_fail)]
fn wrong_pool() {
    let mut f = normal(3);
    f.pool = EthAddress::from_field(0x1018);
    run(f);
}
#[test(should_fail)]
fn wrong_asset() {
    let mut f = normal(3);
    f.asset = EthAddress::from_field(124);
    run(f);
}
#[test(should_fail)]
fn zero_asset() {
    let mut f = normal(3);
    f.asset = EthAddress::from_field(0);
    run(f);
}
#[test(should_fail)]
fn wrong_path() {
    let mut f = normal(3);
    f.paths[1][0] = 0;
    run(f);
}
#[test(should_fail)]
fn wrong_index() {
    let mut f = normal(3);
    f.indices[1] = 0;
    run(f);
}
#[test(should_fail)]
fn wrong_root() {
    let mut f = normal(3);
    f.root = 1;
    run(f);
}
#[test(should_fail)]
fn wrong_nullifier() {
    let mut f = normal(3);
    f.nullifiers[1] = 1;
    run(f);
}
#[test(should_fail)]
fn duplicate_note() {
    let mut f = normal(3);
    f.amounts[1] = f.amounts[0];
    f.keys[1] = f.keys[0];
    f.paths[1] = f.paths[0];
    f.indices[1] = f.indices[0];
    f.nullifiers[1] = f.nullifiers[0];
    run(f);
}
#[test(should_fail)]
fn substituted_output() {
    let mut f = normal(3);
    f.output_key = 100;
    run(f);
}
#[test(should_fail)]
fn wrong_sum() {
    let mut f = normal(3);
    f.output = note_commitment(31337, note_sn(f.output_key), f.asset, U256::from(26));
    run(f);
}
#[test(should_fail)]
fn reused_output_key() {
    let mut f = normal(3);
    f.output_key = f.keys[0];
    run(f);
}
#[test(should_fail)]
fn zero_output_key() {
    let mut f = normal(3);
    f.output_key = 0;
    run(f);
}
#[test(should_fail)]
fn public_padding() {
    let mut f = normal(3);
    f.nullifiers[3] = 1;
    run(f);
}
#[test(should_fail)]
fn private_padding() {
    let mut f = normal(3);
    f.paths[3][0] = 1;
    run(f);
}
