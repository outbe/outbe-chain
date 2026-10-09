//! Drift checks between Solidity interface files and Rust precompile
//! annotations. Tests use it to compare one `function` declaration with the
//! expected argument types, `view` modifier and return types.

/// One Solidity function declaration reduced to comparable type lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolFnCanonical {
    pub arg_types: String,
    pub is_view: bool,
    pub ret_types: String,
}

/// Parses one `function NAME(...) ... returns (...)` declaration out of a
/// Solidity interface body into a comparable canonical form.
pub fn sol_function_canonical(sol: &str, name: &str) -> Option<SolFnCanonical> {
    let needle = format!("function {name}(");
    let start = sol.find(&needle)? + needle.len() - 1;
    let args_end = closing_paren(sol, start);
    let args_raw = &sol[start + 1..args_end];
    let arg_types = canonical_type_list(args_raw);

    let tail_end = sol[args_end..].find(';')? + args_end;
    let tail = &sol[args_end + 1..tail_end];
    let is_view = tail.split_whitespace().any(|t| t == "view");
    let ret_types = match tail.find("returns") {
        Some(idx) => returns_type_list(&tail[idx + "returns".len()..])?,
        None => String::new(),
    };
    Some(SolFnCanonical {
        arg_types,
        is_view,
        ret_types,
    })
}

/// Returns the index of the parenthesis that closes the one at `open`.
/// Returns `open` when no parenthesis closes it.
fn closing_paren(sol: &str, open: usize) -> usize {
    let mut depth = 0i32;
    for (i, b) in sol.as_bytes()[open..].iter().enumerate() {
        depth += match b {
            b'(' => 1,
            b')' => -1,
            _ => 0,
        };
        if *b == b')' && depth == 0 {
            return open + i;
        }
    }
    open
}

/// Reduces the `(...)` list after `returns` to a list of types.
fn returns_type_list(after_returns: &str) -> Option<String> {
    let lparen = after_returns.find('(')?;
    let rparen = after_returns.rfind(')')?;
    Some(canonical_type_list(&after_returns[lparen + 1..rparen]))
}

/// Reduces a Solidity parameter list to a comma-separated list of types.
fn canonical_type_list(list: &str) -> String {
    list.split(',')
        .map(|part| part.split_whitespace().next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::sol_function_canonical;

    /// Pins the parser output for names, `calldata`/`memory` markers, `view` and
    /// a declaration without `returns`.
    #[test]
    fn sol_function_canonical_reads_sample_interface() {
        const SAMPLE: &str = "interface ISample {\n\
                function a(address owner) external view returns (uint256);\n\
                function b(uint8 pool, uint256 amount) external returns (uint256 claimed);\n\
                function c(bytes calldata data, string memory note) external;\n\
            }";
        let a = sol_function_canonical(SAMPLE, "a").unwrap();
        assert_eq!(
            (a.arg_types.as_str(), a.is_view, a.ret_types.as_str()),
            ("address", true, "uint256")
        );
        let b = sol_function_canonical(SAMPLE, "b").unwrap();
        assert_eq!(
            (b.arg_types.as_str(), b.is_view, b.ret_types.as_str()),
            ("uint8,uint256", false, "uint256")
        );
        let c = sol_function_canonical(SAMPLE, "c").unwrap();
        assert_eq!(
            (c.arg_types.as_str(), c.is_view, c.ret_types.as_str()),
            ("bytes,string", false, "")
        );
        assert!(sol_function_canonical(SAMPLE, "missing").is_none());
    }
}
