use ark_bn254::Fr as CircuitField;
use ark_ff::{BigInteger, PrimeField};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use curve25519_dalek::{
    ristretto::{CompressedRistretto, RistrettoPoint},
    scalar::Scalar,
};
use num_bigint::BigUint;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn scalar_modulus() -> BigUint {
    (BigUint::from(1u8) << 252usize)
        + BigUint::parse_bytes(b"27742317777372353535851937790883648493", 10).unwrap()
}
pub fn scalar_integer(v: Scalar) -> BigUint {
    BigUint::from_bytes_le(&v.to_bytes())
}
pub fn scalar(v: &BigUint) -> Result<Scalar> {
    if v >= &scalar_modulus() {
        return Err("noncanonical Ristretto scalar".into());
    }
    let raw = v.to_bytes_le();
    let mut a = [0; 32];
    a[..raw.len()].copy_from_slice(&raw);
    Option::<Scalar>::from(Scalar::from_canonical_bytes(a))
        .ok_or_else(|| "noncanonical scalar".into())
}
pub fn generators() -> (RistrettoPoint, RistrettoPoint) {
    let p = bulletproofs::PedersenGens::default();
    (p.B, p.B_blinding)
}
pub fn commit(v: &BigUint, r: Scalar) -> Result<RistrettoPoint> {
    let (g, h) = generators();
    Ok(scalar(v)? * g + r * h)
}
pub fn point_decode(s: &str) -> Result<RistrettoPoint> {
    let a: [u8; 32] = hex::decode(s)?.try_into().map_err(|_| "point length")?;
    if hex::encode(a) != s {
        return Err("noncanonical point hex".into());
    }
    CompressedRistretto(a)
        .decompress()
        .ok_or_else(|| "invalid Ristretto point".into())
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
