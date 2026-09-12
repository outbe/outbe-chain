//! Mutate adversarial assignments, including Boolean decompositions, rather
//! than relying only on honestly generated (zero-padded) witness values.
use ark_bn254::Fr;
use ark_r1cs_std::{alloc::AllocationMode, boolean::Boolean, fields::fp::FpVar};
use ark_relations::r1cs::{ConstraintSystem, ConstraintSystemRef, Variable};
use outbe_p_link_measurements::integer::UInt;

fn set_witness(cs: &ConstraintSystemRef<Fr>, variable: Variable, value: u64) {
    let Variable::Witness(index) = variable else {
        panic!("test expects an allocated witness");
    };
    cs.borrow_mut().unwrap().witness_assignment[index] = Fr::from(value);
}

#[test]
fn multiplication_rejects_forged_high_padding_limb_and_matching_bits() {
    // The exact vulnerable path was a 128-bit issuance multiplied by a 64-bit
    // constant, returned as 256 bits. The fourth result limb must be zero.
    for width in [256, 384] {
        let cs = ConstraintSystem::<Fr>::new_ref();
        let issuance =
            UInt::alloc(cs.clone(), 123_456u64.into(), 128, AllocationMode::Witness).unwrap();
        let product = issuance
            .mul(&UInt::constant(1_000_000), cs.clone(), width)
            .unwrap();

        // Forge product += 2^192 while preserving this limb's Boolean
        // decomposition. A missing multiplication column accepts this attack.
        let FpVar::Var(high_limb) = &product.limbs[3] else {
            panic!("test expects an allocated result limb");
        };
        let Boolean::Var(high_bit) = &product.bits[192] else {
            panic!("test expects an allocated result bit");
        };
        set_witness(&cs, high_limb.variable, 1);
        set_witness(&cs, high_bit.variable(), 1);
        assert!(
            !cs.is_satisfied().unwrap(),
            "widened multiplication accepted a forged high result limb"
        );
    }
}

#[test]
fn widened_multiplication_accepts_honest_padding() {
    let cs = ConstraintSystem::<Fr>::new_ref();
    let issuance =
        UInt::alloc(cs.clone(), 123_456u64.into(), 128, AllocationMode::Witness).unwrap();
    issuance
        .mul(&UInt::constant(1_000_000), cs.clone(), 256)
        .unwrap();
    assert!(cs.is_satisfied().unwrap());
}
