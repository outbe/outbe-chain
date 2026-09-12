//! P_source: canonical L2 draft/economics → salted SHA256 nominal digest.
//! This MUST be composed with P_commit over the exact same digest.
use ark_bn254::Fr;
use ark_crypto_primitives::crh::sha256::constraints::Sha256Gadget;
use ark_ff::{AdditiveGroup, Field, PrimeField};
use ark_r1cs_std::{
    alloc::{AllocVar, AllocationMode},
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    prelude::ToBitsGadget,
    uint8::UInt8,
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use outbe_p_link_measurements::{
    integer::{bounded_fp, fr_integer, less, less_constant, UInt},
    poseidon2::{hash, native_hash},
};
use sha2::{Digest, Sha256};

pub const MAX_SOURCES: usize = 4;
pub const DOMAIN: u64 = 0x504c534f555201;
pub const BRIDGE_DOMAIN: &[u8] = b"OUTBE-P-LINK-BRIDGE-v1";
pub fn bridge_digest(a: &BigUint, salt: &[u8; 32]) -> [u8; 32] {
    assert!(a.bits() <= 256);
    let mut encoded = [0u8; 32];
    let bytes = a.to_bytes_be();
    encoded[32 - bytes.len()..].copy_from_slice(&bytes);
    let mut h = Sha256::new();
    h.update(BRIDGE_DOMAIN);
    h.update(encoded);
    h.update(salt);
    h.finalize().into()
}
#[derive(Clone, Debug)]
pub struct LinkPublic {
    pub derived_owner: Fr,
    pub nft_hash: Fr,
    pub binding_hash: Fr,
    pub merkle_root: Fr,
    pub sender: BigUint,
    pub chain_id: u64,
    pub day: u64,
    pub currency: u16,
    pub reference_currency: u16,
    pub exclude_from_intex: bool,
    pub source_count: u8,
    pub source_ids: [Fr; MAX_SOURCES],
    pub issuance_vwap: BigUint,
    pub reference_vwap: BigUint,
    pub reference_scurve: BigUint,
    pub nominal_digest: [u8; 32],
}
#[derive(Clone, Debug)]
pub struct LinkWitness {
    pub draft_id: Fr,
    pub base: u64,
    pub atto: u64,
    pub nominal: BigUint,
    pub bridge_salt: [u8; 32],
}
#[derive(Clone)]
pub struct LinkCircuit {
    pub public: LinkPublic,
    pub private: LinkWitness,
}

fn limbs(value: &BigUint, count: usize) -> Vec<Fr> {
    let mask = (BigUint::one() << 64usize) - BigUint::one();
    (0..count)
        .map(|i| Fr::from_le_bytes_mod_order(&((value >> (64 * i)) & &mask).to_bytes_le()))
        .collect()
}
impl LinkPublic {
    pub fn validate(&self) -> Result<(), String> {
        if self.sender.bits() > 160 {
            return Err("sender exceeds address".into());
        }
        if self.source_count as usize > MAX_SOURCES {
            return Err("too many source markers for this circuit".into());
        }
        if self.issuance_vwap.bits() > 256
            || self.reference_vwap.bits() > 256
            || self.reference_scurve.bits() > 256
        {
            return Err("Oracle value exceeds uint256".into());
        }
        if self.issuance_vwap.is_zero() || self.reference_vwap.is_zero() {
            return Err("zero VWAP".into());
        }
        let n = self.source_count as usize;
        if self.source_ids[n..].iter().any(|v| *v != Fr::ZERO) {
            return Err("nonzero inactive source marker".into());
        }
        for pair in self.source_ids[..n].windows(2) {
            if fr_integer(pair[0]) >= fr_integer(pair[1]) {
                return Err("source markers not strictly ordered".into());
            }
        }
        Ok(())
    }
    /// Canonical verifier input vector. Raw amounts/openings are never inputs.
    pub fn inputs(&self) -> Result<Vec<Fr>, String> {
        self.validate()?;
        let mut v = vec![
            self.derived_owner,
            self.nft_hash,
            self.binding_hash,
            self.merkle_root,
            Fr::from_le_bytes_mod_order(&self.sender.to_bytes_le()),
            Fr::from(self.chain_id),
            Fr::from(self.day),
            Fr::from(self.currency as u64),
            Fr::from(self.reference_currency as u64),
            Fr::from(self.exclude_from_intex as u64),
            Fr::from(self.source_count as u64),
        ];
        for p in [
            &self.issuance_vwap,
            &self.reference_vwap,
            &self.reference_scurve,
        ] {
            v.extend(limbs(p, 4));
        }
        v.extend(self.source_ids);
        for chunk in self.nominal_digest.chunks_exact(16) {
            v.push(Fr::from_be_bytes_mod_order(chunk));
        }
        let mut context = vec![Fr::from(DOMAIN)];
        context.extend_from_slice(&v);
        v.push(native_hash(&context));
        Ok(v)
    }
}
pub fn draft_hash(public: &LinkPublic, private: &LinkWitness) -> Fr {
    let mut body = vec![
        public.derived_owner,
        Fr::from(public.day),
        Fr::from(public.currency as u64),
        Fr::from(private.base),
        Fr::from(private.atto),
        Fr::from(public.source_count as u64),
    ];
    body.extend_from_slice(&public.source_ids[..public.source_count as usize]);
    body.iter()
        .fold(private.draft_id, |h, x| native_hash(&[h, *x]))
}
pub fn binding_hash(public: &LinkPublic, draft_id: Fr) -> Fr {
    let id = fr_integer(draft_id);
    let mask = (BigUint::one() << 128usize) - BigUint::one();
    native_hash(&[
        Fr::ONE,
        Fr::from_le_bytes_mod_order(&public.sender.to_bytes_le()),
        Fr::from_le_bytes_mod_order(&(&id & &mask).to_bytes_le()),
        Fr::from_le_bytes_mod_order(&(id >> 128usize).to_bytes_le()),
        Fr::from(public.chain_id),
    ])
}
pub fn nominal(public: &LinkPublic, base: u64, atto: u64) -> Result<BigUint, String> {
    if atto >= 1_000_000 {
        return Err("noncanonical amount remainder".into());
    }
    let u = BigUint::from(base) * BigUint::from(1_000_000u64) + BigUint::from(atto);
    if u.is_zero() || public.issuance_vwap.is_zero() || public.reference_vwap.is_zero() {
        return Err("zero amount/price".into());
    }
    let num = &u * BigUint::from(1_000_000u64) * &public.reference_vwap;
    let den = &public.issuance_vwap
        * public
            .reference_vwap
            .clone()
            .max(public.reference_scurve.clone());
    if num.bits() > 512 || den.bits() > 512 {
        return Err("U512 overflow".into());
    }
    let result = num / den;
    if result.is_zero() || result.bits() > 256 {
        return Err("nominal outside positive uint256".into());
    }
    Ok(result)
}
pub fn fixture() -> LinkCircuit {
    let mut public = LinkPublic {
        derived_owner: Fr::from(17u64),
        nft_hash: Fr::ZERO,
        binding_hash: Fr::ZERO,
        merkle_root: Fr::from(29u64),
        sender: BigUint::parse_bytes(b"112233445566778899aabbccddeeff0011223344", 16).unwrap(),
        chain_id: 19_280_501,
        day: 20260911,
        currency: 840,
        reference_currency: 978,
        exclude_from_intex: false,
        source_count: 2,
        source_ids: [Fr::from(31u64), Fr::from(37u64), Fr::ZERO, Fr::ZERO],
        issuance_vwap: 2_123_457u64.into(),
        reference_vwap: 1_054_321u64.into(),
        reference_scurve: 1_500_001u64.into(),
        nominal_digest: [0; 32],
    };
    let mut private = LinkWitness {
        draft_id: Fr::from(13u64),
        base: 12_345,
        atto: 678_901,
        nominal: BigUint::zero(),
        bridge_salt: Sha256::digest(b"OUTBE-P-LINK-PUBLIC-FIXTURE-SALT-v1").into(),
    };
    private.nominal = nominal(&public, private.base, private.atto).unwrap();
    public.nft_hash = draft_hash(&public, &private);
    public.binding_hash = binding_hash(&public, private.draft_id);
    public.nominal_digest = bridge_digest(&private.nominal, &private.bridge_salt);
    LinkCircuit { public, private }
}

fn public_fp(
    cs: ConstraintSystemRef<Fr>,
    v: Fr,
    all: &mut Vec<FpVar<Fr>>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let var = FpVar::new_input(cs, || Ok(v))?;
    all.push(var.clone());
    Ok(var)
}
fn public_bounded(
    cs: ConstraintSystemRef<Fr>,
    v: BigUint,
    w: usize,
    all: &mut Vec<FpVar<Fr>>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let (var, _) = bounded_fp(cs, &v, w, AllocationMode::Input)?;
    all.push(var.clone());
    Ok(var)
}
fn public_uint(
    cs: ConstraintSystemRef<Fr>,
    v: BigUint,
    w: usize,
    all: &mut Vec<FpVar<Fr>>,
) -> Result<UInt, SynthesisError> {
    let n = UInt::alloc(cs, v, w, AllocationMode::Input)?;
    all.extend_from_slice(&n.limbs);
    Ok(n)
}

impl ConstraintSynthesizer<Fr> for LinkCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let p = &self.public;
        let w = &self.private;
        let mut all = Vec::new();
        let owner = public_fp(cs.clone(), p.derived_owner, &mut all)?;
        let expected_hash = public_fp(cs.clone(), p.nft_hash, &mut all)?;
        let expected_binding = public_fp(cs.clone(), p.binding_hash, &mut all)?;
        let _ = public_fp(cs.clone(), p.merkle_root, &mut all)?;
        let sender = public_bounded(cs.clone(), p.sender.clone(), 160, &mut all)?;
        let chain = public_bounded(cs.clone(), p.chain_id.into(), 64, &mut all)?;
        let day = public_bounded(cs.clone(), p.day.into(), 64, &mut all)?;
        let currency = public_bounded(cs.clone(), (p.currency as u64).into(), 16, &mut all)?;
        let _ = public_bounded(
            cs.clone(),
            (p.reference_currency as u64).into(),
            16,
            &mut all,
        )?;
        let _ = public_bounded(
            cs.clone(),
            (p.exclude_from_intex as u64).into(),
            1,
            &mut all,
        )?;
        let count = public_bounded(cs.clone(), (p.source_count as u64).into(), 3, &mut all)?;
        let count_bits = count.to_bits_le()?;
        less_constant(&count_bits, &BigUint::from(MAX_SOURCES + 1))?
            .enforce_equal(&Boolean::TRUE)?;
        let vi = public_uint(cs.clone(), p.issuance_vwap.clone(), 256, &mut all)?;
        let vr = public_uint(cs.clone(), p.reference_vwap.clone(), 256, &mut all)?;
        let sc = public_uint(cs.clone(), p.reference_scurve.clone(), 256, &mut all)?;
        let mut source_vars = Vec::new();
        for id in p.source_ids {
            source_vars.push(public_fp(cs.clone(), id, &mut all)?);
        }
        let mut digest_vars = Vec::new();
        for chunk in p.nominal_digest.chunks_exact(16) {
            digest_vars.push(public_bounded(
                cs.clone(),
                BigUint::from_bytes_be(chunk),
                128,
                &mut all,
            )?);
        }
        let expected_context = self
            .public
            .inputs()
            .map_err(|_| SynthesisError::Unsatisfiable)?
            .last()
            .copied()
            .unwrap();
        let context = FpVar::new_input(cs.clone(), || Ok(expected_context))?;
        let mut hash_context = vec![FpVar::constant(Fr::from(DOMAIN))];
        hash_context.extend(all);
        hash(&hash_context)?.enforce_equal(&context)?;

        let id = FpVar::new_witness(cs.clone(), || Ok(w.draft_id))?;
        let id_bits = id.to_bits_le()?;
        let id_low = Boolean::le_bits_to_fp(&id_bits[..128])?;
        let id_high = Boolean::le_bits_to_fp(&id_bits[128..])?;
        hash(&[FpVar::one(), sender, id_low, id_high, chain])?.enforce_equal(&expected_binding)?;
        let (base, base_bits) =
            bounded_fp(cs.clone(), &w.base.into(), 64, AllocationMode::Witness)?;
        let (atto, atto_bits) =
            bounded_fp(cs.clone(), &w.atto.into(), 20, AllocationMode::Witness)?;
        less_constant(&atto_bits, &1_000_000u64.into())?.enforce_equal(&Boolean::TRUE)?;
        let mut running = id;
        for item in [
            owner,
            day,
            currency,
            base.clone(),
            atto.clone(),
            count.clone(),
        ] {
            running = hash(&[running, item])?;
        }
        for (i, source) in source_vars.iter().enumerate() {
            let inactive = less_constant(&count_bits, &BigUint::from(i + 1))?;
            source.conditional_enforce_equal(&FpVar::zero(), &inactive)?;
            let next = hash(&[running.clone(), source.clone()])?;
            running = inactive.select(&running, &next)?;
            if i > 0 {
                less(&source_vars[i - 1].to_bits_le()?, &source.to_bits_le()?)?
                    .conditional_enforce_equal(&Boolean::TRUE, &(!inactive))?;
            }
        }
        running.enforce_equal(&expected_hash)?;

        let base = UInt::from_bounded_fp(base, base_bits, w.base.into());
        let atto = UInt::from_bounded_fp(atto, atto_bits, w.atto.into());
        let u =
            base.mul(&UInt::constant(1_000_000), cs.clone(), 128)?
                .add(&atto, cs.clone(), 128)?;
        u.enforce_nonzero()?;
        vi.enforce_nonzero()?;
        vr.enforce_nonzero()?;
        let choose_sc = less(&vr.bits, &sc.bits)?;
        let max_value = p.reference_vwap.clone().max(p.reference_scurve.clone());
        let effective = UInt::alloc(cs.clone(), max_value, 256, AllocationMode::Witness)?;
        for i in 0..4 {
            choose_sc
                .select(&sc.limbs[i], &vr.limbs[i])?
                .enforce_equal(&effective.limbs[i])?;
        }
        let numerator =
            u.mul(&UInt::constant(1_000_000), cs.clone(), 256)?
                .mul(&vr, cs.clone(), 512)?;
        let denominator = vi.mul(&effective, cs.clone(), 512)?;
        let a = UInt::alloc(cs.clone(), w.nominal.clone(), 256, AllocationMode::Witness)?;
        a.enforce_nonzero()?;
        let remainder_value = if denominator.value.is_zero() {
            BigUint::zero()
        } else {
            &numerator.value % &denominator.value
        };
        let remainder = UInt::alloc(cs.clone(), remainder_value, 512, AllocationMode::Witness)?;
        less(&remainder.bits, &denominator.bits)?.enforce_equal(&Boolean::TRUE)?;
        a.mul(&denominator, cs.clone(), 768)?
            .add(&remainder, cs.clone(), 768)?
            .enforce_equal(&numerator)?;

        // Build canonical 32-byte big-endian nominal from the SAME bounded bits
        // used in quotient/remainder arithmetic, never from a reduced field scalar.
        let mut preimage = BRIDGE_DOMAIN
            .iter()
            .copied()
            .map(UInt8::constant)
            .collect::<Vec<_>>();
        preimage.extend(a.bits.chunks_exact(8).rev().map(UInt8::from_bits_le));
        for byte in w.bridge_salt {
            preimage.push(UInt8::new_witness(cs.clone(), || Ok(byte))?);
        }
        let digest = Sha256Gadget::digest(&preimage)?;
        for (chunk, expected) in digest.0.chunks_exact(16).zip(digest_vars) {
            let mut le_bits = Vec::new();
            for byte in chunk.iter().rev() {
                le_bits.extend(byte.to_bits_le()?);
            }
            Boolean::le_bits_to_fp(&le_bits)?.enforce_equal(&expected)?;
        }
        Ok(())
    }
}
