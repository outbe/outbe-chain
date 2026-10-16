//! Finite P0 byte vectors using Git's actual object hashing, not production parsers.
use commonware_codec::DecodeExt;
use commonware_cryptography::{bls12381::primitives::group::G2, sha256::Sha256, Hasher};
use commonware_utils::union_unique;
use std::{
    io::Write,
    process::{Command, Stdio},
};

const DOMAIN: &[u8] = b"radicle.repository.bls.v1";
const ACTOR: &str = "Outbe Network <network@outbe.invalid> 1700000000 +0000";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn oid(kind: &str, bytes: &[u8]) -> [u8; 20] {
    let mut child = Command::new("git")
        .args(["hash-object", "-t", kind, "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    unhex(std::str::from_utf8(&output.stdout).unwrap().trim())
        .unwrap()
        .try_into()
        .unwrap()
}

fn message(
    kind: u8,
    repo: [u8; 20],
    context: [u8; 32],
    meta: [u8; 32],
    native: &[u8],
) -> Option<Vec<u8>> {
    if !(1..=3).contains(&kind) || native.len() > 1 << 20 || (kind < 3 && native.len() != 20) {
        return None;
    }
    Some(
        [
            b"RDB1".as_slice(),
            &[kind],
            &repo,
            &context,
            &meta,
            &(native.len() as u32).to_be_bytes(),
            &Sha256::hash(&[native]).0,
        ]
        .concat(),
    )
}

fn envelope_matches(
    e: &[u8],
    expected: &[u8],
    public: &[u8],
    verify: &impl Fn(&[u8], &[u8]) -> bool,
) -> bool {
    e.len() == 269
        && expected.len() == 125
        && &e[..125] == expected
        && &e[125..221] == public
        && G2::decode(&e[125..221]).is_ok()
        && verify(expected, &e[221..])
}

// Deliberately limited fixture parser: the complete normative grammar is design.md §2.1.
fn unsigned(commit: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(commit).ok()?;
    if s.contains(['\r', '\0']) || !s.ends_with('\n') {
        return None;
    }
    let (headers, body) = s.split_once("\n\n")?;
    let mut seen = false;
    let mut out = String::new();
    for line in headers.lines() {
        if let Some(value) = line.strip_prefix("radicle-bls-sig ") {
            if seen || value.len() != 538 || unhex(value)?.len() != 269 {
                return None;
            }
            seen = true;
        } else {
            if line.starts_with(' ') || line.starts_with("gpgsig ") {
                return None;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    if !seen {
        return None;
    }
    out.push('\n');
    out.push_str(body);
    Some(out.into_bytes())
}

fn key_roundtrip(key: &[u8]) -> bool {
    match key.first() {
        Some(0) => key.len() == 33,
        Some(1) => key.len() == 97 && G2::decode(&key[1..]).is_ok(),
        _ => false,
    }
}

pub fn check(
    public: &[u8],
    sign: impl Fn(&[u8]) -> Vec<u8>,
    verify: impl Fn(&[u8], &[u8]) -> bool,
) {
    let doc = include_bytes!("../vectors/identity.json");
    let repo = oid("blob", doc);
    // Native COB tree with manifest + operation blob, in Git tree ordering.
    let op = br#"{"revision":"3333333333333333333333333333333333333333","type":"review","verdict":"accept"}"#;
    let manifest = br#"{"typeName":"xyz.radicle.patch","version":1}"#;
    let mut tree = b"100644 0\0".to_vec();
    tree.extend_from_slice(&oid("blob", op));
    tree.extend_from_slice(b"100644 manifest\0");
    tree.extend_from_slice(&oid("blob", manifest));
    let tree_oid = oid("tree", &tree);
    let metadata = format!(
        "tree {}\nauthor {ACTOR}\ncommitter {ACTOR}\n\nAccept patch\n",
        hex(&tree_oid)
    );
    let empty_tree = oid("tree", b"");
    let code = format!(
        "tree {}\nauthor {ACTOR}\ncommitter {ACTOR}\n\nInitial content\n",
        hex(&empty_tree)
    );
    let code_head = oid("commit", code.as_bytes());
    let mut cob_commit: Option<[u8; 20]> = None;
    let previous_sigrefs = [0x44; 20];
    println!("VECTOR doc_utf8={}", std::str::from_utf8(doc).unwrap());
    println!("VECTOR cob_tree_raw={}", hex(&tree));
    println!("VECTOR cob_unsigned_commit={}", hex(metadata.as_bytes()));
    let key = [&[1], public].concat();
    assert!(key_roundtrip(&key));
    assert!(!key_roundtrip(&key[..96]));
    assert!(!key_roundtrip(&[&[2], public].concat()));
    assert!(!key_roundtrip(&[&[1][..], &[0; 96][..]].concat()));
    assert!(key_roundtrip(&[&[0][..], &[3; 32][..]].concat())); // Ed framing only.
    println!("VECTOR repository_key_v2={}", hex(&key));
    println!("VECTOR upgrade_control_v1=0302");
    println!("VECTOR upgrade_ack_control_v1=0402");
    println!(
        "VECTOR repository_namespace={}",
        include_str!("../vectors/repository-key.txt")
    );
    for kind in 1..=3 {
        let (payload, meta) = match kind {
            1 => (repo.to_vec(), [0; 32]),
            2 => (tree_oid.to_vec(), Sha256::hash(&[metadata.as_bytes()]).0),
            3 => {
                let refs = format!("{} refs/cobs/xyz.radicle.patch/3333333333333333333333333333333333333333\n{} refs/heads/main\n", hex(&cob_commit.unwrap()), hex(&code_head));
                println!("VECTOR refs_canonical={}", hex(refs.as_bytes()));
                (refs.into_bytes(), Sha256::hash(&[&previous_sigrefs]).0)
            }
            _ => unreachable!(),
        };
        // Full typed context examples; native subjects are independent vectors,
        // not an assembled valid managed bootstrap/patch history.
        let bootstrap = kind == 1;
        let ordinal = if kind == 3 { 1 } else { 0 };
        let proposal = if bootstrap { [0; 32] } else { [1; 32] };
        let patch = if bootstrap { [0; 20] } else { [0x33; 20] };
        let target = if bootstrap { [0; 20] } else { code_head };
        let job = [
            b"OPJ1".as_slice(),
            &2026u64.to_be_bytes(),
            &[7; 32],
            &repo,
            &proposal,
            &patch,
            &patch,
            &target,
            &[if bootstrap { 0 } else { 2 }],
            &[ordinal],
            &2u64.to_be_bytes(),
            &1u64.to_be_bytes(),
            &[9; 32],
            &[0; 32],
        ]
        .concat();
        let context = Sha256::hash(&[&job]).0;
        let m = message(kind, repo, context, meta, &payload).unwrap();
        assert_eq!(m.len(), 125);
        let signature = sign(&m);
        assert_eq!(signature.len(), 48);
        let envelope = [m.as_slice(), public, signature.as_slice()].concat();
        assert!(envelope_matches(&envelope, &m, public, &verify));
        let mut tampered = envelope.clone();
        tampered[4] = if kind == 1 { 2 } else { 1 };
        assert!(!envelope_matches(&tampered, &m, public, &verify));
        tampered = envelope.clone();
        tampered[89] ^= 1;
        assert!(!envelope_matches(&tampered, &m, public, &verify));
        tampered = envelope.clone();
        tampered[268] ^= 1;
        assert!(!envelope_matches(&tampered, &m, public, &verify));
        assert!(!envelope_matches(&envelope[..268], &m, public, &verify));
        assert!(!envelope_matches(
            &[envelope.as_slice(), &[0]].concat(),
            &m,
            public,
            &verify
        ));
        let h = hex(&envelope);
        assert_eq!(unhex(&h).unwrap(), envelope);
        assert!(unhex(&h.to_uppercase()).is_none());
        println!("VECTOR kind={kind} native={} metadata_hash={} job={} context={} message={} commonware_input={} signature={} envelope={}",
            hex(&payload), hex(&meta), hex(&job), hex(&context), hex(&m), hex(&union_unique(DOMAIN, &m)), hex(&signature), h);
        if kind == 2 {
            let (head, body) = metadata.split_once("\n\n").unwrap();
            let commit = format!("{head}\nradicle-bls-sig {h}\n\n{body}");
            assert_eq!(unsigned(commit.as_bytes()).unwrap(), metadata.as_bytes());
            cob_commit = Some(oid("commit", commit.as_bytes()));
            assert_ne!(cob_commit.unwrap(), tree_oid);
            let duplicate = commit.replace("\n\n", &format!("\nradicle-bls-sig {h}\n\n"));
            assert!(unsigned(duplicate.as_bytes()).is_none());
            assert!(unsigned(commit.replace('\n', "\r\n").as_bytes()).is_none());
            let altered = metadata.replace("Accept patch", "Merge patch");
            let different = message(
                2,
                repo,
                context,
                Sha256::hash(&[altered.as_bytes()]).0,
                &payload,
            )
            .unwrap();
            assert!(!verify(&different, &signature));
            println!(
                "VECTOR cob_signed_commit_oid={}",
                hex(&oid("commit", commit.as_bytes()))
            );
        }
    }
    assert!(message(0, repo, [0; 32], [0; 32], &repo).is_none());
    assert!(message(1, repo, [0; 32], [0; 32], &[0; 19]).is_none());
    assert!(message(3, repo, [0; 32], [0; 32], &vec![0; (1 << 20) + 1]).is_none());
    println!("PASS native vectors: actual Git blob/tree/commit OIDs, three RDB1 subjects, real threshold signatures, metadata/header reconstruction, key/envelope framing and tamper negatives");
    println!("VECTOR ONLY: fixture parsers are not production P2 parsers or managed bootstrap implementation");
}
