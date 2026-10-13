fn main(inputs: [Field; 8], len: u32) -> pub Field {
    assert(len <= 8);
    let mut state = [0, 0, 0, (len as Field) * 18446744073709551616];
    for block in 0..2 {
        if block < len / 3 {
            for i in 0..3 {
                state[i] += inputs[block * 3 + i];
            }
            state = std::hash::poseidon2_permutation(state);
        }
    }
    for i in 0..3 {
        if i < len % 3 {
            state[i] += inputs[(len / 3) * 3 + i];
        }
    }
    std::hash::poseidon2_permutation(state)[0]
}
