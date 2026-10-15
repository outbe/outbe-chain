//! Exact native BLS commit buffers. Cryptographic envelope verification is separate.

use std::collections::BTreeSet;

const SIGNATURE_HEADER: &str = "radicle-bls-sig ";
const ENVELOPE_HEX_LENGTH: usize = 538;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("non UTF-8 native BLS commit")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid native BLS commit grammar")]
    Grammar,
    #[error("invalid or duplicate native BLS commit object ID")]
    ObjectId,
    #[error("invalid native BLS commit author, timestamp or timezone")]
    Author,
    #[error("invalid native BLS signature header")]
    Signature,
}

/// Validated exact bytes, without an object prefix or signature header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsignedCommit {
    buffer: String,
    tree: [u8; 20],
}

impl UnsignedCommit {
    pub fn parse(buffer: &[u8]) -> Result<Self, Error> {
        let buffer = std::str::from_utf8(buffer)?;
        let (headers, _) = parts(buffer)?;
        let mut lines = headers.split('\n').peekable();
        let tree = object_id(
            lines
                .next()
                .and_then(|s| s.strip_prefix("tree "))
                .ok_or(Error::Grammar)?,
        )?;
        let mut parents = BTreeSet::new();
        while let Some(parent) = lines.peek().and_then(|s| s.strip_prefix("parent ")) {
            object_id(parent)?;
            if !parents.insert(parent) {
                return Err(Error::ObjectId);
            }
            lines.next();
        }
        author(
            lines
                .next()
                .and_then(|s| s.strip_prefix("author "))
                .ok_or(Error::Grammar)?,
        )?;
        author(
            lines
                .next()
                .and_then(|s| s.strip_prefix("committer "))
                .ok_or(Error::Grammar)?,
        )?;
        if lines.next().is_some() {
            return Err(Error::Grammar);
        }
        Ok(Self {
            buffer: buffer.to_owned(),
            tree,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.buffer.as_bytes()
    }

    pub fn tree(&self) -> [u8; 20] {
        self.tree
    }

    /// Attach a syntax-checked envelope after the committer, preserving every other byte.
    /// The caller must verify the envelope against this buffer and its repository context.
    pub fn attach(self, envelope: &str) -> Result<SignedCommit, Error> {
        signature(envelope)?;
        Ok(SignedCommit {
            unsigned: self,
            envelope: envelope.to_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedCommit {
    unsigned: UnsignedCommit,
    envelope: String,
}

impl SignedCommit {
    /// Remove only the single last physical header. No message/trailer normalization occurs.
    pub fn parse(buffer: &[u8]) -> Result<Self, Error> {
        let buffer = std::str::from_utf8(buffer)?;
        let (headers, message) = parts(buffer)?;
        let (unsigned_headers, signature_header) =
            headers.rsplit_once('\n').ok_or(Error::Signature)?;
        let envelope = signature_header
            .strip_prefix(SIGNATURE_HEADER)
            .ok_or(Error::Signature)?;
        signature(envelope)?;
        let unsigned =
            UnsignedCommit::parse(format!("{unsigned_headers}\n\n{message}").as_bytes())?;
        Ok(Self {
            unsigned,
            envelope: envelope.to_owned(),
        })
    }

    pub fn unsigned(&self) -> &UnsignedCommit {
        &self.unsigned
    }

    pub fn envelope(&self) -> &str {
        &self.envelope
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let (headers, message) = self
            .unsigned
            .buffer
            .split_once("\n\n")
            .expect("validated unsigned commits contain the separator");
        format!(
            "{headers}\n{SIGNATURE_HEADER}{}\n\n{message}",
            self.envelope
        )
        .into_bytes()
    }
}

fn parts(buffer: &str) -> Result<(&str, &str), Error> {
    if buffer.contains(['\r', '\0']) {
        return Err(Error::Grammar);
    }
    let (headers, message) = buffer.split_once("\n\n").ok_or(Error::Grammar)?;
    if !message.ends_with('\n') {
        return Err(Error::Grammar);
    }
    Ok((headers, message))
}

fn object_id(input: &str) -> Result<[u8; 20], Error> {
    if input.len() != 40
        || !input
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::ObjectId);
    }
    let mut result = [0; 20];
    for (out, pair) in result.iter_mut().zip(input.as_bytes().chunks_exact(2)) {
        let hex = |b: u8| {
            if b.is_ascii_digit() {
                b - b'0'
            } else {
                b - b'a' + 10
            }
        };
        *out = (hex(pair[0]) << 4) | hex(pair[1]);
    }
    Ok(result)
}

fn author(input: &str) -> Result<(), Error> {
    let (identity_time, zone) = input.rsplit_once(' ').ok_or(Error::Author)?;
    let (identity, seconds) = identity_time.rsplit_once(' ').ok_or(Error::Author)?;
    let time = seconds.parse::<i64>().map_err(|_| Error::Author)?;
    if time.to_string() != seconds {
        return Err(Error::Author);
    }
    let zone = zone.as_bytes();
    if zone.len() != 5
        || !matches!(zone[0], b'+' | b'-')
        || !zone[1..].iter().all(u8::is_ascii_digit)
    {
        return Err(Error::Author);
    }
    let hours = (zone[1] - b'0') * 10 + zone[2] - b'0';
    let minutes = (zone[3] - b'0') * 10 + zone[4] - b'0';
    if hours > 23 || minutes > 59 || zone == b"-0000" {
        return Err(Error::Author);
    }
    let (name, email) = identity.rsplit_once(" <").ok_or(Error::Author)?;
    let email = email.strip_suffix('>').ok_or(Error::Author)?;
    if name.contains(['<', '>']) || email.contains(['<', '>']) {
        return Err(Error::Author);
    }
    Ok(())
}

fn signature(input: &str) -> Result<(), Error> {
    if input.len() != ENVELOPE_HEX_LENGTH
        || !input
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Signature);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNSIGNED: &[u8] = include_bytes!("bls/unsigned-commit.txt");
    const ENVELOPE: &str = include_str!("bls/envelope.hex");

    #[test]
    fn p0_native_buffer_roundtrip_preserves_metadata_and_trailers() {
        let unsigned = UnsignedCommit::parse(UNSIGNED).unwrap();
        assert_eq!(unsigned.as_bytes(), UNSIGNED);
        assert_eq!(
            &unsigned.tree(),
            &object_id(
                std::str::from_utf8(UNSIGNED)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .strip_prefix("tree ")
                    .unwrap()
            )
            .unwrap()
        );
        let signed = unsigned.attach(ENVELOPE).unwrap();
        let bytes = signed.to_bytes();
        let parsed = SignedCommit::parse(&bytes).unwrap();
        assert_eq!(parsed, signed);
        assert_eq!(parsed.unsigned().as_bytes(), UNSIGNED);
        assert_eq!(parsed.envelope(), ENVELOPE);
        assert_eq!(parsed.to_bytes(), bytes);
        #[cfg(feature = "git2")]
        assert_eq!(
            git2::Oid::hash_object(git2::ObjectType::Commit, &bytes)
                .unwrap()
                .to_string(),
            include_str!("bls/expected-oid.txt")
        );
        let text = std::str::from_utf8(UNSIGNED).unwrap();
        let body_header = format!("{text}\nradicle-bls-sig not-a-header\n");
        assert!(UnsignedCommit::parse(body_header.as_bytes()).is_ok());
    }

    #[test]
    fn malformed_metadata_and_headers_are_rejected() {
        let text = std::str::from_utf8(UNSIGNED).unwrap();
        let (headers, message) = text.split_once("\n\n").unwrap();
        for invalid in [
            text.replace("tree ", "Tree "),
            text.replace('\n', "\r\n"),
            format!("{text}\0"),
            text.trim_end().to_owned(),
            format!("{headers}\n\n"),
            format!("{headers}\ngpgsig xxx\n\n{message}"),
            format!("{headers}\nencoding UTF-8\n\n{message}"),
            format!("{headers}\n extra folded header\n\n{message}"),
            text.replacen("tree ", "tree A", 1),
        ] {
            assert!(
                UnsignedCommit::parse(invalid.as_bytes()).is_err(),
                "{invalid:?}"
            );
        }
        let parent = format!("parent {}", "ab".repeat(20));
        let with_parent = text.replacen("\nauthor ", &format!("\n{parent}\nauthor "), 1);
        assert!(UnsignedCommit::parse(with_parent.as_bytes()).is_ok());
        let duplicate = with_parent.replacen(&parent, &format!("{parent}\n{parent}"), 1);
        assert!(UnsignedCommit::parse(duplicate.as_bytes()).is_err());
        let uppercase = with_parent.replacen(&parent, &format!("parent {}", "AB".repeat(20)), 1);
        assert!(UnsignedCommit::parse(uppercase.as_bytes()).is_err());
        let mut invalid_utf8 = UNSIGNED.to_vec();
        invalid_utf8[0] = 0xff;
        assert!(UnsignedCommit::parse(&invalid_utf8).is_err());
    }

    #[test]
    fn timestamps_and_identity_are_canonical() {
        let text = std::str::from_utf8(UNSIGNED).unwrap();
        let author_line = text.lines().find(|s| s.starts_with("author ")).unwrap();
        for suffix in [
            "0 +0000",
            "-1 -2359",
            "9223372036854775807 +0000",
            "-9223372036854775808 +0000",
        ] {
            let line = format!("author Person <person@example.invalid> {suffix}");
            assert!(UnsignedCommit::parse(text.replace(author_line, &line).as_bytes()).is_ok());
        }
        for value in [
            "Person <person@example.invalid> 00 +0000",
            "Person <person@example.invalid> -0 +0000",
            "Person <person@example.invalid> +1 +0000",
            "Person <person@example.invalid> 9223372036854775808 +0000",
            "Person <person@example.invalid> 0 -0000",
            "Person <person@example.invalid> 0 +2400",
            "Person <person@example.invalid> 0 +0060",
            "Person <person@example.invalid> 0 +000",
            "Person <person@example.invalid> 0 +00a0",
            "Person <person@example.invalid> 0 00000",
            "Person <bad<email> 0 +0000",
            "Person <email>> 0 +0000",
            "<person@example.invalid> 0 +0000",
        ] {
            let invalid = text.replace(author_line, &format!("author {value}"));
            assert!(
                UnsignedCommit::parse(invalid.as_bytes()).is_err(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn signature_header_is_single_unfolded_lowercase_and_cannot_coexist_with_gpgsig() {
        let unsigned = UnsignedCommit::parse(UNSIGNED).unwrap();
        for invalid in [
            ENVELOPE.to_uppercase(),
            ENVELOPE[..536].to_owned(),
            format!("{ENVELOPE}00"),
            format!(" {ENVELOPE}"),
            "g".repeat(538),
        ] {
            assert!(unsigned.clone().attach(&invalid).is_err());
        }
        let signed = String::from_utf8(unsigned.attach(ENVELOPE).unwrap().to_bytes()).unwrap();
        let line = format!("{SIGNATURE_HEADER}{ENVELOPE}");
        for invalid in [
            signed.replace(&line, &format!("{line}\n{line}")),
            signed.replace(&line, &format!("{line}\ngpgsig xxx")),
            signed.replace(&line, &format!("gpgsig xxx\n{line}")),
            signed.replace(&line, &format!("{line}\n more")),
            signed.replace(
                &line,
                &format!(
                    "{SIGNATURE_HEADER}{}\n {}",
                    &ENVELOPE[..200],
                    &ENVELOPE[200..]
                ),
            ),
            signed.replace(&line, "radicle-bls-sig invalid"),
        ] {
            assert!(SignedCommit::parse(invalid.as_bytes()).is_err());
        }
    }
}
