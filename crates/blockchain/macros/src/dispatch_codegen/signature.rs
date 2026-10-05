//! Solidity signature grammar used by contract dispatch generation.

use super::*;

pub(super) struct ParsedSig {
    pub(super) name: String,
    pub(super) arg_types: Vec<String>,
    pub(super) tail: String,
}

pub(super) fn parse_signature(lit: &LitStr) -> syn::Result<ParsedSig> {
    let raw = lit.value();
    let raw = raw.trim();

    let open = raw.find('(').ok_or_else(|| {
        syn::Error::new_spanned(lit, "signature missing '(' - expected `name(types) ...`")
    })?;
    let name = raw[..open].trim().to_string();
    if name.is_empty() || !is_valid_sol_ident(&name) {
        return Err(syn::Error::new_spanned(
            lit,
            format!("signature has invalid or empty function name: `{}`", name),
        ));
    }

    let mut depth = 1i32;
    let mut close = None;
    for (i, ch) in raw[open + 1..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + 1 + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close =
        close.ok_or_else(|| syn::Error::new_spanned(lit, "signature missing matching ')'"))?;

    let inner = &raw[open + 1..close];
    let arg_types = if inner.trim().is_empty() {
        Vec::new()
    } else {
        split_top_level(inner)
    };

    let tail = raw[close + 1..].trim().to_string();

    Ok(ParsedSig {
        name,
        arg_types,
        tail,
    })
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '(' | '[' => {
                depth += 1;
                buf.push(ch);
            }
            ')' | ']' => {
                depth -= 1;
                buf.push(ch);
            }
            ',' if depth == 0 => {
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    let trimmed = buf.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    out
}

fn is_valid_sol_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
