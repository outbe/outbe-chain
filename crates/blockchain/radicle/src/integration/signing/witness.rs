//! Separate interchangeable evidence; these bytes never enter the native manifest hash.

use super::{Error, RepositoryPartial, RepositorySigningMaterial};
use radicle::{
    canonical::formatter::CanonicalFormatter,
    crypto::{BlsEnvelope, RepositorySubject},
};
use serde::{Deserialize, Serialize};

pub const MAX_WITNESS_BYTES: usize = 1024 * 1024;

pub struct RepositoryWitnessBundle {
    output: Vec<u8>,
    subjects: Vec<Vec<RepositoryPartial>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BundleWire {
    version: u8,
    public_output: String,
    subjects: Vec<SubjectWire>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubjectWire {
    ordinal: u8,
    partials: Vec<String>,
}

impl RepositoryWitnessBundle {
    /// Subject order is native signing order; each participant list is canonicalized.
    pub fn new(output: Vec<u8>, mut subjects: Vec<Vec<RepositoryPartial>>) -> Result<Self, Error> {
        if subjects.len() > 3
            || subjects
                .iter()
                .any(|frames| frames.len() > outbe_consensus::bls::MAX_VALIDATORS as usize)
        {
            return Err(Error::Witness);
        }
        for frames in &mut subjects {
            frames.sort_by_key(RepositoryPartial::participant_index);
        }
        let result = Self { output, subjects };
        result.to_bytes()?;
        Ok(result)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_WITNESS_BYTES {
            return Err(Error::Witness);
        }
        let wire: BundleWire = serde_json::from_slice(bytes).map_err(|_| Error::Witness)?;
        if wire.version != 1 || wire.subjects.len() > 3 {
            return Err(Error::Witness);
        }
        let output = hex::decode(wire.public_output).map_err(|_| Error::Witness)?;
        let subjects = wire
            .subjects
            .into_iter()
            .enumerate()
            .map(|(ordinal, subject)| {
                if subject.ordinal as usize != ordinal
                    || subject.partials.len() > outbe_consensus::bls::MAX_VALIDATORS as usize
                {
                    return Err(Error::Witness);
                }
                subject
                    .partials
                    .into_iter()
                    .map(|encoded| {
                        if encoded.len() != 268 * 2 {
                            return Err(Error::Witness);
                        }
                        let bytes = hex::decode(encoded).map_err(|_| Error::Witness)?;
                        RepositoryPartial::from_bytes(&bytes)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = Self { output, subjects };
        if result.to_bytes()? != bytes {
            return Err(Error::Witness);
        }
        Ok(result)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        if self.output.is_empty()
            || self.output.len() > MAX_WITNESS_BYTES / 2
            || self.subjects.len() > 3
            || self.subjects.iter().any(|frames| {
                frames.is_empty()
                    || frames.len() > outbe_consensus::bls::MAX_VALIDATORS as usize
                    || frames
                        .windows(2)
                        .any(|pair| pair[0].participant_index() >= pair[1].participant_index())
            })
        {
            return Err(Error::Witness);
        }
        let wire = BundleWire {
            version: 1,
            public_output: hex::encode(&self.output),
            subjects: self
                .subjects
                .iter()
                .enumerate()
                .map(|(ordinal, frames)| SubjectWire {
                    ordinal: ordinal as u8,
                    partials: frames.iter().map(|f| hex::encode(f.to_bytes())).collect(),
                })
                .collect(),
        };
        let mut bytes = Vec::new();
        wire.serialize(&mut serde_json::Serializer::with_formatter(
            &mut bytes,
            CanonicalFormatter::new(),
        ))
        .map_err(|_| Error::Witness)?;
        if bytes.len() > MAX_WITNESS_BYTES {
            return Err(Error::Witness);
        }
        Ok(bytes)
    }

    pub fn public_output(&self) -> &[u8] {
        &self.output
    }

    /// `actual` must be reconstructed from checked native objects in ordinal order.
    /// Permission/current checkpoint binding remains the package publication layer's job.
    pub fn verify(
        &self,
        material: &RepositorySigningMaterial,
        actual: &[(RepositorySubject, BlsEnvelope)],
    ) -> Result<(), Error> {
        if self.output != material.canonical_output() || self.subjects.len() != actual.len() {
            return Err(Error::Witness);
        }
        for (frames, (subject, envelope)) in self.subjects.iter().zip(actual) {
            material.verify_witness(subject, envelope, frames)?;
        }
        Ok(())
    }
}
