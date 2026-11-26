//! Strict, byte-preserving `# outbe-up:<key>:begin..end` block parser and splicer.
//!
//! Shared by DHCP hosts (`dnsmasq.rs`) and host firewall (`/etc/host-firewall.nft`, S6.1).
//! Guarantees:
//! - Every `# outbe-up:` marker line is strictly validated (`:begin` / `:end` only).
//! - Every `:begin` has a matching `:end`; no nesting, no overlap, no duplicates, no unclosed blocks.
//! - Bytes outside our target block (CRLF, blank lines, trailing newline or its absence, foreign
//!   blocks) are copied 100% verbatim by byte offset — never re-joined with `.lines()`.

use std::collections::BTreeSet;

use eyre::{bail, ensure, Result};

const MARKER_PREFIX: &str = "# outbe-up:";

/// Byte offsets of one validated `# outbe-up:<key>:begin..end` block within a file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedBlockSpan {
    pub key: String,
    /// Byte offset of the `# outbe-up:<key>:begin` line start.
    pub block_start: usize,
    /// Byte offset right after the `:begin` line's newline.
    pub body_start: usize,
    /// Byte offset of the `# outbe-up:<key>:end` line start.
    pub body_end: usize,
    /// Byte offset right after the `:end` line's newline (or EOF).
    pub block_end: usize,
    /// 1-based line number of the `:begin` marker.
    pub begin_line: usize,
    /// 1-based line number of the `:end` marker.
    pub end_line: usize,
}

/// Result of a byte-preserving block upsert or remove splice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockSpliceResult {
    pub content: String,
    /// True iff a complete, valid block for `key` existed in the input (for upsert: iff its exact
    /// bytes already equaled the desired rendered block; for remove: iff the block was present).
    pub exact_match: bool,
    /// True iff `content` differs from the input bytes.
    pub changed: bool,
    /// Exact original slice of the existing block, if present in the input.
    pub existing_block: Option<String>,
}

/// Exact line slice with byte offsets, preserving `\n`, `\r\n`, and missing final newline.
#[derive(Clone, Copy, Debug)]
pub struct LineSpan<'a> {
    pub line_no: usize,
    pub start: usize,
    pub end_incl_nl: usize,
    /// Line text without trailing `\r` or `\n`.
    pub text: &'a str,
}

/// Iterate lines of `content` with exact byte offsets.
pub fn iter_line_spans(content: &str) -> Vec<LineSpan<'_>> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut line_no = 1usize;
    while pos < bytes.len() {
        let start = pos;
        while pos < bytes.len() && bytes[pos] != b'\n' {
            pos += 1;
        }
        let end_excl_lf = pos;
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        }
        let end_incl_nl = pos;
        let text_end = if end_excl_lf > start && bytes[end_excl_lf - 1] == b'\r' {
            end_excl_lf - 1
        } else {
            end_excl_lf
        };
        out.push(LineSpan {
            line_no,
            start,
            end_incl_nl,
            text: &content[start..text_end],
        });
        line_no += 1;
    }
    out
}

/// Strictly parse and validate ALL `# outbe-up:` marker blocks in `content`.
///
/// Refuses:
/// - malformed `# outbe-up:` comment lines not ending in `:begin` or `:end` with a valid key
/// - nested or overlapping blocks (any `:begin` while another block is open)
/// - unmatched or mismatched `:end` markers
/// - unclosed `:begin` markers at EOF
/// - duplicate block keys anywhere in the file
pub fn parse_managed_blocks(content: &str) -> Result<Vec<ManagedBlockSpan>> {
    let mut blocks = Vec::new();
    let mut seen_keys = BTreeSet::new();
    let mut open: Option<(String, usize, usize, usize)> = None;

    for span in iter_line_spans(content) {
        let trimmed = span.text.trim();
        let Some(after_hash) = trimmed.strip_prefix('#') else {
            continue;
        };
        if !after_hash.trim_start().starts_with("outbe-up:") {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix(MARKER_PREFIX) else {
            bail!(
                "malformed outbe-up marker at line {}: `{trimmed}` (expected exact `# outbe-up:<key>:begin` or `# outbe-up:<key>:end`)",
                span.line_no
            );
        };

        if let Some(key) = rest.strip_suffix(":begin") {
            validate_marker_key(key, span.line_no)?;
            if let Some((ref open_key, _, _, open_line)) = open {
                bail!(
                    "nested outbe-up marker `# outbe-up:{key}:begin` at line {} inside unclosed block `# outbe-up:{open_key}:begin` (line {open_line})",
                    span.line_no
                );
            }
            ensure!(
                seen_keys.insert(key.to_owned()),
                "duplicate outbe-up managed block `{key}` at line {}",
                span.line_no
            );
            open = Some((key.to_owned(), span.start, span.end_incl_nl, span.line_no));
        } else if let Some(key) = rest.strip_suffix(":end") {
            validate_marker_key(key, span.line_no)?;
            let Some((open_key, block_start, body_start, begin_line)) = open.take() else {
                bail!(
                    "unmatched end marker `# outbe-up:{key}:end` at line {} without opening begin marker",
                    span.line_no
                );
            };
            ensure!(
                open_key == key,
                "mismatched end marker `# outbe-up:{key}:end` at line {} (expected `# outbe-up:{open_key}:end` for block opened at line {begin_line})",
                span.line_no
            );
            blocks.push(ManagedBlockSpan {
                key: open_key,
                block_start,
                body_start,
                body_end: span.start,
                block_end: span.end_incl_nl,
                begin_line,
                end_line: span.line_no,
            });
        } else {
            bail!(
                "malformed outbe-up marker at line {}: `{trimmed}` (expected `# outbe-up:<key>:begin` or `# outbe-up:<key>:end`)",
                span.line_no
            );
        }
    }

    if let Some((open_key, _, _, open_line)) = open {
        bail!(
            "unclosed outbe-up managed block `# outbe-up:{open_key}:begin` opened at line {open_line} (missing matching `:end` marker)"
        );
    }

    Ok(blocks)
}

fn validate_marker_key(key: &str, line_no: usize) -> Result<()> {
    ensure!(
        !key.is_empty()
            && !key.starts_with(':')
            && !key.ends_with(':')
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')),
        "invalid outbe-up marker key `{key}` at line {line_no}"
    );
    Ok(())
}

/// Format a canonical managed block with `\n` line endings (always terminated by `\n`).
pub fn render_managed_block(key: &str, body_lines: &[String], indent: &str) -> Result<String> {
    validate_marker_key(key, 0)?;
    let mut out = format!("{indent}# outbe-up:{key}:begin\n");
    for line in body_lines {
        ensure!(
            !line.contains('\n') && !line.contains('\r'),
            "managed block body line must not contain embedded newlines"
        );
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("{indent}# outbe-up:{key}:end\n"));
    Ok(out)
}

/// Upsert a managed block at `insert_at` (when absent) or replace the existing block in place
/// (when present), preserving every byte outside the block verbatim.
///
/// If an existing block for `key` is found, `validate_existing_body` is called on its raw body
/// slice before replacement so any foreign/unexpected lines inside our block cause a refusal.
pub fn splice_upsert_block_at<F>(
    content: &str,
    key: &str,
    rendered_block: &str,
    insert_at: usize,
    validate_existing_body: F,
) -> Result<BlockSpliceResult>
where
    F: FnOnce(&str) -> Result<()>,
{
    ensure!(
        rendered_block.ends_with('\n'),
        "rendered managed block must end with a newline"
    );
    ensure!(
        insert_at <= content.len() && content.is_char_boundary(insert_at),
        "invalid insertion byte offset {insert_at}"
    );
    let blocks = parse_managed_blocks(content)?;
    if let Some(span) = blocks.iter().find(|b| b.key == key) {
        let existing_body = &content[span.body_start..span.body_end];
        validate_existing_body(existing_body)?;
        let existing_slice = &content[span.block_start..span.block_end];
        let effective_block = if span.block_end == content.len() && !existing_slice.ends_with('\n')
        {
            rendered_block.strip_suffix('\n').unwrap_or(rendered_block)
        } else {
            rendered_block
        };
        if existing_slice == effective_block {
            return Ok(BlockSpliceResult {
                content: content.to_owned(),
                exact_match: true,
                changed: false,
                existing_block: Some(existing_slice.to_owned()),
            });
        }
        let mut out =
            String::with_capacity(content.len() - existing_slice.len() + effective_block.len());
        out.push_str(&content[..span.block_start]);
        out.push_str(effective_block);
        out.push_str(&content[span.block_end..]);
        return Ok(BlockSpliceResult {
            content: out,
            exact_match: false,
            changed: true,
            existing_block: Some(existing_slice.to_owned()),
        });
    }

    // Inserting a new block at `insert_at`: ensure `insert_at` is at the start of a new line
    // without touching any preceding bytes other than adding a single `\n` if the prefix lacks one.
    // When appending at EOF to a file that has no final LF, omit the trailing LF on the inserted
    // block so a subsequent `splice_remove_block` restores the exact original bytes.
    let prefix = &content[..insert_at];
    let suffix = &content[insert_at..];
    let need_leading_nl = !prefix.is_empty() && !prefix.ends_with('\n');
    let effective_block = if need_leading_nl && suffix.is_empty() {
        rendered_block.strip_suffix('\n').unwrap_or(rendered_block)
    } else {
        rendered_block
    };
    let mut out =
        String::with_capacity(content.len() + effective_block.len() + usize::from(need_leading_nl));
    out.push_str(prefix);
    if need_leading_nl {
        out.push('\n');
    }
    out.push_str(effective_block);
    out.push_str(suffix);
    Ok(BlockSpliceResult {
        content: out,
        exact_match: false,
        changed: true,
        existing_block: None,
    })
}

/// Remove ONLY the exact `# outbe-up:<key>:begin..end` block by byte range, preserving every byte
/// outside our block verbatim (CRLF, blank lines, trailing newline or its absence).
///
/// If the block is absent, returns the exact input bytes with `exact_match = false` and
/// `changed = false`. If present, `validate_existing_body` is called before removal so foreign
/// lines injected inside our block are refused instead of deleted.
pub fn splice_remove_block<F>(
    content: &str,
    key: &str,
    validate_existing_body: F,
) -> Result<BlockSpliceResult>
where
    F: FnOnce(&str) -> Result<()>,
{
    let blocks = parse_managed_blocks(content)?;
    let Some(span) = blocks.iter().find(|b| b.key == key) else {
        return Ok(BlockSpliceResult {
            content: content.to_owned(),
            exact_match: false,
            changed: false,
            existing_block: None,
        });
    };
    let existing_body = &content[span.body_start..span.body_end];
    validate_existing_body(existing_body)?;
    let existing_slice = &content[span.block_start..span.block_end];

    let unterminated_final_block =
        span.block_end == content.len() && !existing_slice.ends_with('\n');
    let cut_start =
        if unterminated_final_block && ends_with_bare_lf(&content.as_bytes()[..span.block_start]) {
            span.block_start - 1
        } else {
            span.block_start
        };

    let mut out = String::with_capacity(content.len() - (span.block_end - cut_start));
    out.push_str(&content[..cut_start]);
    out.push_str(&content[span.block_end..]);
    Ok(BlockSpliceResult {
        content: out,
        exact_match: true,
        changed: true,
        existing_block: Some(existing_slice.to_owned()),
    })
}

/// True when `prefix` ends with an LF that is not part of a CRLF pair.
///
/// Removal of a final block without its own newline also takes this LF, so the file does not
/// keep a dangling newline. A CRLF stays intact.
fn ends_with_bare_lf(prefix: &[u8]) -> bool {
    prefix.ends_with(b"\n") && !prefix.ends_with(b"\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f2_rejects_unclosed_nested_duplicate_mismatched_and_malformed_markers() {
        // Unclosed begin followed by foreign block.
        let unclosed = "\
# outbe-up:vm:probe:begin
02:00:00:00:00:88,10.77.0.88,10m
# outbe-up:final-amd:begin
be:f7:7b:2c:d1:85,10.77.0.68,10m
# outbe-up:final-amd:end
";
        assert!(parse_managed_blocks(unclosed).is_err());

        // Unclosed begin at EOF.
        let unclosed_eof = "\
# outbe-up:vm:probe:begin
02:00:00:00:00:88,10.77.0.88,10m
";
        let err = parse_managed_blocks(unclosed_eof).unwrap_err();
        assert!(err.to_string().contains("unclosed"), "{err}");

        // Unmatched end.
        let unmatched_end = "# outbe-up:vm:probe:end\n";
        let err = parse_managed_blocks(unmatched_end).unwrap_err();
        assert!(err.to_string().contains("unmatched end"), "{err}");

        // Mismatched end.
        let mismatched = "\
# outbe-up:vm:a:begin
# outbe-up:vm:b:end
";
        let err = parse_managed_blocks(mismatched).unwrap_err();
        assert!(err.to_string().contains("mismatched"), "{err}");

        // Duplicate block key.
        let dup = "\
# outbe-up:vm:a:begin
# outbe-up:vm:a:end
# outbe-up:vm:a:begin
# outbe-up:vm:a:end
";
        let err = parse_managed_blocks(dup).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");

        // Malformed marker.
        let malformed = "# outbe-up:vm:a:middle\n";
        let err = parse_managed_blocks(malformed).unwrap_err();
        assert!(err.to_string().contains("malformed"), "{err}");
    }

    #[test]
    fn f3_byte_preserving_splice_keeps_crlf_blank_lines_and_missing_final_lf_verbatim() {
        let crlf_with_blanks = "02:00:00:00:00:11,10.77.0.11,infinite\r\n\r\n";

        // Release when absent returns exact original bytes with changed=false.
        let noop_rm = splice_remove_block(crlf_with_blanks, "vm:probe", |_| Ok(())).unwrap();
        assert!(!noop_rm.exact_match);
        assert!(!noop_rm.changed);
        assert_eq!(noop_rm.content.as_bytes(), crlf_with_blanks.as_bytes());

        // Missing final newline when absent also preserved byte-for-byte.
        let no_lf = "02:00:00:00:00:11,10.77.0.11,infinite";
        let noop_no_lf = splice_remove_block(no_lf, "vm:probe", |_| Ok(())).unwrap();
        assert!(!noop_no_lf.changed);
        assert_eq!(noop_no_lf.content.as_bytes(), no_lf.as_bytes());

        // Upsert then remove on CRLF + trailing blank lines restores original byte-for-byte.
        let block = render_managed_block(
            "vm:probe",
            &["02:00:00:00:00:88,10.77.0.88,10m".to_owned()],
            "",
        )
        .unwrap();
        let inserted = splice_upsert_block_at(
            crlf_with_blanks,
            "vm:probe",
            &block,
            crlf_with_blanks.len(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(inserted.changed);
        assert!(!inserted.exact_match);
        assert!(inserted.content.starts_with(crlf_with_blanks));

        // Second upsert with same block -> exact_match=true, changed=false, byte-identical.
        let idempotent = splice_upsert_block_at(
            &inserted.content,
            "vm:probe",
            &block,
            inserted.content.len(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(idempotent.exact_match);
        assert!(!idempotent.changed);
        assert_eq!(idempotent.content.as_bytes(), inserted.content.as_bytes());

        // Remove restores original CRLF + trailing blank lines byte-for-byte.
        let removed = splice_remove_block(&inserted.content, "vm:probe", |_| Ok(())).unwrap();
        assert!(removed.exact_match);
        assert!(removed.changed);
        assert_eq!(removed.content.as_bytes(), crlf_with_blanks.as_bytes());

        // Add-then-remove on a file WITHOUT a final LF restores the exact original bytes (no extra LF).
        let added_no_lf =
            splice_upsert_block_at(no_lf, "vm:probe", &block, no_lf.len(), |_| Ok(())).unwrap();
        assert!(added_no_lf.changed);
        assert!(!added_no_lf.exact_match);
        assert!(added_no_lf.content.starts_with(no_lf));
        assert!(!added_no_lf.content.ends_with('\n'));

        // Re-upsert on that file is idempotent (exact_match=true, changed=false).
        let idemp_no_lf = splice_upsert_block_at(
            &added_no_lf.content,
            "vm:probe",
            &block,
            added_no_lf.content.len(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(idemp_no_lf.exact_match);
        assert!(!idemp_no_lf.changed);
        assert_eq!(
            idemp_no_lf.content.as_bytes(),
            added_no_lf.content.as_bytes()
        );

        // Removing the single block restores the exact LF-less original bytes.
        let removed_no_lf =
            splice_remove_block(&added_no_lf.content, "vm:probe", |_| Ok(())).unwrap();
        assert!(removed_no_lf.exact_match);
        assert!(removed_no_lf.changed);
        assert_eq!(removed_no_lf.content.as_bytes(), no_lf.as_bytes());

        // Multi-block add + remove in EITHER order on a file without final LF restores exact bytes.
        let block_b = render_managed_block(
            "vm:second",
            &["02:00:00:00:00:89,10.77.0.89,10m".to_owned()],
            "",
        )
        .unwrap();
        let both = splice_upsert_block_at(
            &added_no_lf.content,
            "vm:second",
            &block_b,
            added_no_lf.content.len(),
            |_| Ok(()),
        )
        .unwrap();
        // Order 1: remove first block, then second block.
        let rm_a_first = splice_remove_block(&both.content, "vm:probe", |_| Ok(())).unwrap();
        let rm_then_b = splice_remove_block(&rm_a_first.content, "vm:second", |_| Ok(())).unwrap();
        assert_eq!(rm_then_b.content.as_bytes(), no_lf.as_bytes());
        // Order 2: remove second block, then first block.
        let rm_b_first = splice_remove_block(&both.content, "vm:second", |_| Ok(())).unwrap();
        let rm_then_a = splice_remove_block(&rm_b_first.content, "vm:probe", |_| Ok(())).unwrap();
        assert_eq!(rm_then_a.content.as_bytes(), no_lf.as_bytes());
    }
}
