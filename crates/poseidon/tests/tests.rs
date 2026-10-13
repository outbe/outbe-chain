use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonError, PoseidonHasher};

/// This test checks the hash of byte slices that contain ones and twos.
#[test]
fn test_poseidon_bn254_x5_fq_input_ones_twos() {
    // Use the same method as the Poseidon unit test.
    let input1 = Fr::from_le_bytes_mod_order(&[1u8; 32]);
    let input2 = Fr::from_le_bytes_mod_order(&[2u8; 32]);
    let mut hasher = Poseidon::<Fr>::new_circom(2).unwrap();
    let hash = hasher.hash(&[input1, input2]).unwrap();

    // The expected value matches the Poseidon unit test in little-endian order.
    let expected_le = [
        144, 25, 130, 41, 200, 53, 231, 38, 27, 206, 162, 156, 254, 132, 123, 32, 25, 99, 242, 85,
        3, 94, 235, 125, 28, 140, 138, 143, 147, 225, 84, 13,
    ];

    assert_eq!(hash.into_bigint().to_bytes_le(), expected_le);
}

/// This test checks a custom domain tag with byte slices that contain ones and twos.
#[test]
fn test_poseidon_bn254_x5_fq_with_domain_tag() {
    let input1 = Fr::from_le_bytes_mod_order(&[1u8; 32]);
    let input2 = Fr::from_le_bytes_mod_order(&[2u8; 32]);
    let mut hasher = Poseidon::<Fr>::with_domain_tag_circom(2, Fr::from(0u64)).unwrap();
    let hash = hasher.hash(&[input1, input2]).unwrap();

    // A zero domain tag must give the default hash value.
    let expected_tag_zero_le = [
        144, 25, 130, 41, 200, 53, 231, 38, 27, 206, 162, 156, 254, 132, 123, 32, 25, 99, 242, 85,
        3, 94, 235, 125, 28, 140, 138, 143, 147, 225, 84, 13,
    ];

    assert_eq!(hash.into_bigint().to_bytes_le(), expected_tag_zero_le);

    let mut hasher = Poseidon::<Fr>::with_domain_tag_circom(2, Fr::from(1u64)).unwrap();
    let hash = hasher.hash(&[input1, input2]).unwrap();
    // The hash should differ from the hash with a zero domain tag.
    assert_ne!(hash.into_bigint().to_bytes_le(), expected_tag_zero_le);
}

/// This test checks that the hasher returns an error if the input count differs from the supported count.
#[test]
fn test_poseidon_bn254_x5_fq_too_many_inputs() {
    // Use known valid field elements.
    let input1 = Fr::from_le_bytes_mod_order(&[1u8; 32]);
    let input2 = Fr::from_le_bytes_mod_order(&[2u8; 32]);

    for i in 1..13 {
        let mut hasher = Poseidon::<Fr>::new_circom(i).unwrap();

        for j in 1..13 {
            if i != j {
                // Create `j` inputs that alternate between `input1` and `input2`.
                let inputs: Vec<_> = (0..j)
                    .map(|k| if k % 2 == 0 { input1 } else { input2 })
                    .collect();
                let res = hasher.hash(&inputs);
                assert!(res.is_err());
            }
        }
    }
}

/// This test checks that the constructor returns an error for more than 12 inputs.
#[test]
fn test_circom_solana_t_gt_12_fails() {
    for i in 13..16 {
        let hasher = Poseidon::<Fr>::new_circom(i);
        assert!(hasher.is_err());
        if let Err(PoseidonError::InvalidWidthCircom { width, max_limit }) = hasher {
            assert_eq!(width, i + 1);
            assert_eq!(max_limit, 13);
        } else {
            panic!("Expected InvalidWidthCircom error");
        }
    }
}

/// This test checks that the constructor returns an error for 0 inputs.
#[test]
fn test_circom_t_0_fails() {
    let hasher = Poseidon::<Fr>::new_circom(0);
    assert!(hasher.is_err());
    if let Err(PoseidonError::InvalidWidthCircom { width, max_limit }) = hasher {
        assert_eq!(width, 1);
        assert_eq!(max_limit, 13);
    } else {
        panic!("Expected InvalidWidthCircom error");
    }
}

/// This test checks that two hashes of the same input match.
#[test]
fn test_poseidon_bn254_x5_fq_same_input_same_results() {
    let input = Fr::from_le_bytes_mod_order(&[1u8; 32]);

    for nr_inputs in 1..12 {
        let mut hasher = Poseidon::<Fr>::new_circom(nr_inputs).unwrap();

        let mut inputs = Vec::with_capacity(nr_inputs);
        for _ in 0..nr_inputs {
            inputs.push(input);
        }

        let hash1 = hasher.hash(&inputs).unwrap();
        let hash2 = hasher.hash(&inputs).unwrap();

        assert_eq!(hash1, hash2);
    }
}

/// This test checks that different inputs give different hashes.
#[test]
fn test_poseidon_bn254_x5_fq_different_inputs_different_results() {
    let input1 = Fr::from_le_bytes_mod_order(&[1u8; 32]);
    let input2 = Fr::from_le_bytes_mod_order(&[2u8; 32]);

    for nr_inputs in 1..12 {
        let mut hasher = Poseidon::<Fr>::new_circom(nr_inputs).unwrap();

        let mut inputs1 = Vec::with_capacity(nr_inputs);
        let mut inputs2 = Vec::with_capacity(nr_inputs);
        for _ in 0..nr_inputs {
            inputs1.push(input1);
            inputs2.push(input2);
        }

        let hash1 = hasher.hash(&inputs1).unwrap();
        let hash2 = hasher.hash(&inputs2).unwrap();

        assert_ne!(hash1, hash2);
    }
}

/// This test checks that different input counts give different hashes.
#[test]
fn test_poseidon_bn254_x5_fq_different_widths_different_results() {
    let input = Fr::from_le_bytes_mod_order(&[1u8; 32]);

    let mut hashes = Vec::new();
    for nr_inputs in 1..12 {
        let mut hasher = Poseidon::<Fr>::new_circom(nr_inputs).unwrap();
        let mut inputs = Vec::with_capacity(nr_inputs);
        for _ in 0..nr_inputs {
            inputs.push(input);
        }
        let hash = hasher.hash(&inputs).unwrap();
        hashes.push(hash);
    }

    // All hashes should differ.
    for i in 0..hashes.len() {
        for j in (i + 1)..hashes.len() {
            assert_ne!(hashes[i], hashes[j]);
        }
    }
}

/// This test checks that the same inputs give the same hash.
#[test]
fn test_poseidon_bn254_x5_fq_deterministic() {
    // Use known field elements.
    let input1 = Fr::from_le_bytes_mod_order(&[1u8; 32]);
    let input2 = Fr::from_le_bytes_mod_order(&[2u8; 32]);

    for nr_inputs in 1..12 {
        // Create inputs that alternate between `input1` and `input2`.
        let inputs: Vec<_> = (0..nr_inputs)
            .map(|k| if k % 2 == 0 { input1 } else { input2 })
            .collect();

        // Calculate the hash twice with the same inputs.
        let mut hasher1 = Poseidon::<Fr>::new_circom(nr_inputs).unwrap();
        let mut hasher2 = Poseidon::<Fr>::new_circom(nr_inputs).unwrap();

        let hash1 = hasher1.hash(&inputs).unwrap();
        let hash2 = hasher2.hash(&inputs).unwrap();

        assert_eq!(hash1, hash2);
    }
}

/// This test checks that the hasher accepts different field elements.
#[test]
fn test_poseidon_bn254_x5_fq_various_inputs() {
    // Use different known valid field elements.
    let mut inputs_list = Vec::new();

    // Add zero.
    inputs_list.push(Fr::from(0u64));

    // Add byte arrays with values 1 through 9 in the final byte.
    for i in 1..10 {
        let mut bytes = [0u8; 32];
        bytes[31] = i;
        inputs_list.push(Fr::from_le_bytes_mod_order(&bytes));
    }

    // Add repeated byte patterns.
    inputs_list.push(Fr::from_le_bytes_mod_order(&[1u8; 32]));
    inputs_list.push(Fr::from_le_bytes_mod_order(&[2u8; 32]));

    for nr_inputs in 1..12 {
        let mut hasher = Poseidon::<Fr>::new_circom(nr_inputs).unwrap();

        // Create inputs from the list, with repetition from the start when necessary.
        let inputs: Vec<_> = (0..nr_inputs)
            .map(|i| inputs_list[i % inputs_list.len()])
            .collect();

        // The hash calculation should succeed.
        let result = hasher.hash(&inputs);
        assert!(result.is_ok());

        // The result should be a valid field element.
        let hash = result.unwrap();
        // The hash can be zero, but it should be consistent.
        let _ = hash.into_bigint().to_bytes_le(); // Check that the hash converts to bytes.
    }
}
