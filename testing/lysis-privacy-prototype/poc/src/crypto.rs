use ark_bn254::Fr as CircuitField;
use ark_ec::{AffineRepr, CurveGroup};
use ark_ed_on_bn254::{EdwardsAffine, Fr as Scalar};
use ark_ff::{BigInteger, PrimeField};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use num_bigint::BigUint;
use sha2::{Digest, Sha256};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::OnceLock;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn scalar_modulus() -> BigUint {
    BigUint::from_bytes_le(&Scalar::MODULUS.to_bytes_le())
}
pub fn scalar_integer(value: Scalar) -> BigUint {
    BigUint::from_bytes_le(&value.into_bigint().to_bytes_le())
}
pub fn scalar(value: &BigUint) -> Result<Scalar> {
    if value >= &scalar_modulus() {
        return Err("integer cannot be reduced modulo the commitment scalar field".into());
    }
    Ok(Scalar::from_le_bytes_mod_order(&value.to_bytes_le()))
}
pub fn generators() -> (EdwardsAffine, EdwardsAffine) {
    // Deterministic try-and-increment point decoding + cofactor clearing.
    // Do NOT generate H as a publicly known scalar times G.
    static BASES: OnceLock<(EdwardsAffine, EdwardsAffine)> = OnceLock::new();
    *BASES.get_or_init(|| {
        let g = EdwardsAffine::generator();
        for counter in 0u64.. {
            let mut hash = Sha256::new();
            hash.update(b"OUTBE-PRIVATE-LIFECYCLE-POC-BABY-H-v1");
            hash.update(counter.to_be_bytes());
            if let Some(p) = EdwardsAffine::from_random_bytes(&hash.finalize()) {
                let h = p.clear_cofactor();
                if !h.is_zero() && h != g {
                    return (g, h);
                }
            }
        }
        unreachable!("unbounded deterministic generator derivation")
    })
}
pub fn commit(value: &BigUint, blind: Scalar) -> Result<EdwardsAffine> {
    let (g, h) = generators();
    Ok((g * scalar(value)? + h * blind).into_affine())
}
pub fn encode<T: CanonicalSerialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(value.compressed_size());
    value.serialize_compressed(&mut bytes)?;
    Ok(bytes)
}
pub fn decode<T: CanonicalDeserialize>(bytes: &[u8]) -> Result<T> {
    let mut input = bytes;
    let value = T::deserialize_compressed(&mut input)?;
    if !input.is_empty() {
        return Err("trailing canonical bytes".into());
    }
    Ok(value)
}
pub fn write_canonical<T: CanonicalSerialize>(path: &Path, value: &T) -> Result<()> {
    let mut writer = BufWriter::new(std::fs::File::create(path)?);
    value.serialize_compressed(&mut writer)?;
    writer.flush()?;
    Ok(())
}
pub fn read_canonical<T: CanonicalDeserialize>(path: &Path) -> Result<T> {
    // Stream instead of retaining a second compressed proving-key allocation.
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let value = T::deserialize_compressed(&mut reader)?;
    let mut extra = [0];
    if reader.read(&mut extra)? != 0 {
        return Err("trailing canonical file bytes".into());
    }
    Ok(value)
}
pub fn field_hex(value: CircuitField) -> String {
    hex::encode(value.into_bigint().to_bytes_be())
}
pub fn field_from_hex(value: &str) -> Result<CircuitField> {
    let bytes = hex::decode(value)?;
    if bytes.len() != 32 || hex::encode(&bytes) != value {
        return Err("expected canonical lowercase BE32 field".into());
    }
    let integer = BigUint::from_bytes_be(&bytes);
    let modulus = BigUint::from_bytes_le(&CircuitField::MODULUS.to_bytes_le());
    if integer >= modulus {
        return Err("noncanonical circuit field".into());
    }
    Ok(CircuitField::from_be_bytes_mod_order(
        &integer.to_bytes_be(),
    ))
}
pub fn integer(value: &str) -> Result<BigUint> {
    let n = BigUint::parse_bytes(value.as_bytes(), 10).ok_or("invalid unsigned decimal")?;
    if n.to_string() != value {
        return Err("noncanonical unsigned decimal".into());
    }
    Ok(n)
}
pub fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)?;
    Ok(())
}
pub fn write_private_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
