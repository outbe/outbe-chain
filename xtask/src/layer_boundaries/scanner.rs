use std::path::Path;

use super::{
    lexer::{is_ident_start, Lexer},
    InclusionKind, LayerBoundaryError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TargetBase {
    SourceParent,
    ManifestDir,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DiscoveredInclusion {
    pub(super) line: usize,
    pub(super) directive: InclusionKind,
    pub(super) base: TargetBase,
    pub(super) raw_target: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DiscoveredStringLiteral {
    pub(super) line: usize,
    pub(super) value: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ScanOutput {
    pub(super) inclusions: Vec<DiscoveredInclusion>,
    pub(super) string_literals: Vec<DiscoveredStringLiteral>,
}

pub(super) fn scan_source_items(
    source_file: &Path,
    src: &str,
) -> Result<ScanOutput, LayerBoundaryError> {
    Scanner::new(source_file, src).scan_all()
}

struct Scanner<'a> {
    source_file: &'a Path,
    lex: Lexer<'a>,
}

impl<'a> Scanner<'a> {
    const fn new(source_file: &'a Path, src: &'a str) -> Self {
        Self {
            source_file,
            lex: Lexer::new(src),
        }
    }

    fn scan_all(&mut self) -> Result<ScanOutput, LayerBoundaryError> {
        let mut out = ScanOutput::default();
        while !self.lex.is_eof() {
            self.lex.skip_ws_and_comments();
            let Some(byte) = self.lex.peek() else {
                break;
            };
            if byte == b'#' {
                self.scan_hash(&mut out.inclusions)?;
            } else if is_ident_start(byte) && !self.lex.starts_literal_token() {
                self.scan_identifier(&mut out.inclusions)?;
            } else if let Some((line, value)) = self.lex.try_read_any_string_literal() {
                out.string_literals
                    .push(DiscoveredStringLiteral { line, value });
            } else {
                self.lex.skip_single_atom();
            }
        }
        Ok(out)
    }

    fn scan_hash(&mut self, out: &mut Vec<DiscoveredInclusion>) -> Result<(), LayerBoundaryError> {
        self.lex.advance();
        self.lex.skip_ws_and_comments();
        if self.lex.peek() == Some(b'!') {
            self.lex.advance();
            self.lex.skip_ws_and_comments();
        }
        if self.lex.peek() == Some(b'[') {
            self.lex.advance();
            self.scan_meta_list(b']', out)?;
        }
        Ok(())
    }

    fn scan_meta_list(
        &mut self,
        closing: u8,
        out: &mut Vec<DiscoveredInclusion>,
    ) -> Result<(), LayerBoundaryError> {
        while !self.lex.is_eof() {
            self.lex.skip_ws_and_comments();
            if self.lex.peek() == Some(closing) {
                self.lex.advance();
                return Ok(());
            }
            if !self.try_scan_meta_ident(out)? {
                self.skip_balanced_meta_token(closing);
            }
        }
        Ok(())
    }

    fn try_scan_meta_ident(
        &mut self,
        out: &mut Vec<DiscoveredInclusion>,
    ) -> Result<bool, LayerBoundaryError> {
        if !matches!(self.lex.peek(), Some(ch) if is_ident_start(ch) && !self.lex.starts_literal_token())
        {
            return Ok(false);
        }
        let item_line = self.lex.line;
        let ident = self.lex.read_ident().unwrap_or_default();
        self.lex.skip_ws_and_comments();
        if ident == "path" && self.lex.peek() == Some(b'=') && self.lex.peek_at(1) != Some(b'=') {
            self.lex.advance();
            let (base, raw_target) =
                self.parse_target_expr(item_line, InclusionKind::ModulePath)?;
            out.push(DiscoveredInclusion {
                line: item_line,
                directive: InclusionKind::ModulePath,
                base,
                raw_target,
            });
            return Ok(true);
        }
        if ident == "cfg_attr" && self.lex.peek() == Some(b'(') {
            self.lex.advance();
            self.skip_cfg_predicate();
            self.scan_meta_list(b')', out)?;
        }
        Ok(true)
    }

    fn skip_cfg_predicate(&mut self) {
        let mut paren_depth = 0usize;
        while !self.lex.is_eof() {
            self.lex.skip_ws_and_comments();
            match self.lex.peek() {
                None | Some(b')') if paren_depth == 0 => return,
                Some(b',') if paren_depth == 0 => {
                    self.lex.advance();
                    return;
                }
                Some(b'(') => {
                    paren_depth += 1;
                    self.lex.advance();
                }
                Some(b')') => {
                    paren_depth -= 1;
                    self.lex.advance();
                }
                _ => self.lex.skip_single_atom(),
            }
        }
    }

    fn skip_balanced_meta_token(&mut self, closing: u8) {
        match self.lex.peek() {
            None => {}
            Some(ch) if ch == closing => {}
            Some(b'(') => self.skip_delimited(b'(', b')'),
            Some(b'[') => self.skip_delimited(b'[', b']'),
            Some(b'{') => self.skip_delimited(b'{', b'}'),
            _ => self.lex.skip_single_atom(),
        }
    }

    fn skip_delimited(&mut self, open: u8, close: u8) {
        if self.lex.peek() != Some(open) {
            return;
        }
        self.lex.advance();
        let mut depth = 1usize;
        while depth > 0 && !self.lex.is_eof() {
            self.lex.skip_ws_and_comments();
            match self.lex.peek() {
                None => break,
                Some(ch) if ch == open => {
                    depth += 1;
                    self.lex.advance();
                }
                Some(ch) if ch == close => {
                    depth -= 1;
                    self.lex.advance();
                }
                _ => self.lex.skip_single_atom(),
            }
        }
    }

    fn scan_identifier(
        &mut self,
        out: &mut Vec<DiscoveredInclusion>,
    ) -> Result<(), LayerBoundaryError> {
        let ident_line = self.lex.line;
        let Some(directive) = self.read_inclusion_macro_name() else {
            return Ok(());
        };
        let Some(close) = self.try_open_macro_group() else {
            return Ok(());
        };
        let (base, raw_target) = self.parse_target_expr(ident_line, directive)?;
        self.consume_optional_comma();
        if self.lex.peek() != Some(close) {
            return Err(self.unparsable(ident_line, directive));
        }
        self.lex.advance();
        out.push(DiscoveredInclusion {
            line: ident_line,
            directive,
            base,
            raw_target,
        });
        Ok(())
    }

    fn read_inclusion_macro_name(&mut self) -> Option<InclusionKind> {
        match self.lex.read_ident()? {
            "include_str" => Some(InclusionKind::IncludeStr),
            "include_bytes" => Some(InclusionKind::IncludeBytes),
            "include" => Some(InclusionKind::IncludeMacro),
            _ => None,
        }
    }

    fn try_open_macro_group(&mut self) -> Option<u8> {
        let saved = (self.lex.pos, self.lex.line);
        self.lex.skip_ws_and_comments();
        if self.lex.peek() != Some(b'!') {
            (self.lex.pos, self.lex.line) = saved;
            return None;
        }
        self.lex.advance();
        self.lex.skip_ws_and_comments();
        let Some(close) = matching_close(self.lex.peek()) else {
            (self.lex.pos, self.lex.line) = saved;
            return None;
        };
        self.lex.advance();
        Some(close)
    }

    fn consume_optional_comma(&mut self) {
        self.lex.skip_ws_and_comments();
        if self.lex.peek() == Some(b',') {
            self.lex.advance();
            self.lex.skip_ws_and_comments();
        }
    }

    fn parse_target_expr(
        &mut self,
        line: usize,
        directive: InclusionKind,
    ) -> Result<(TargetBase, String), LayerBoundaryError> {
        self.lex.skip_ws_and_comments();
        if let Some(literal) = self.lex.parse_string_literal() {
            return Ok((TargetBase::SourceParent, literal));
        }
        if self.try_consume_macro_head("concat") {
            return self.parse_concat_expr(line, directive);
        }
        Err(self.unparsable(line, directive))
    }

    fn parse_concat_expr(
        &mut self,
        line: usize,
        directive: InclusionKind,
    ) -> Result<(TargetBase, String), LayerBoundaryError> {
        let close =
            matching_close(self.lex.peek()).ok_or_else(|| self.unparsable(line, directive))?;
        self.lex.advance();
        let mut base = TargetBase::SourceParent;
        let mut joined = String::new();
        let mut first = true;
        loop {
            self.lex.skip_ws_and_comments();
            if self.lex.peek() == Some(close) {
                self.lex.advance();
                break;
            }
            if first && self.try_consume_manifest_dir_env(line, directive)? {
                base = TargetBase::ManifestDir;
            } else if let Some(segment) = self.lex.parse_string_literal() {
                joined.push_str(&segment);
            } else {
                return Err(self.unparsable(line, directive));
            }
            first = false;
            self.consume_optional_comma();
        }
        let target = if base == TargetBase::ManifestDir {
            joined.trim_start_matches('/').to_owned()
        } else {
            joined
        };
        Ok((base, target))
    }

    fn try_consume_manifest_dir_env(
        &mut self,
        line: usize,
        directive: InclusionKind,
    ) -> Result<bool, LayerBoundaryError> {
        if !self.try_consume_macro_head("env") {
            return Ok(false);
        }
        let close =
            matching_close(self.lex.peek()).ok_or_else(|| self.unparsable(line, directive))?;
        self.lex.advance();
        let var_name = self
            .lex
            .parse_string_literal()
            .ok_or_else(|| self.unparsable(line, directive))?;
        self.lex.skip_ws_and_comments();
        if self.lex.peek() != Some(close) || var_name != "CARGO_MANIFEST_DIR" {
            return Err(self.unparsable(line, directive));
        }
        self.lex.advance();
        Ok(true)
    }

    fn try_consume_macro_head(&mut self, expected: &str) -> bool {
        let saved = (self.lex.pos, self.lex.line);
        if self.lex.read_ident() != Some(expected) {
            (self.lex.pos, self.lex.line) = saved;
            return false;
        }
        self.lex.skip_ws_and_comments();
        if self.lex.peek() != Some(b'!') {
            (self.lex.pos, self.lex.line) = saved;
            return false;
        }
        self.lex.advance();
        self.lex.skip_ws_and_comments();
        true
    }

    fn unparsable(&self, line: usize, directive: InclusionKind) -> LayerBoundaryError {
        LayerBoundaryError::UnparsableInclusion {
            source_file: self.source_file.to_path_buf(),
            line,
            directive,
        }
    }
}

const fn matching_close(open: Option<u8>) -> Option<u8> {
    match open {
        Some(b'(') => Some(b')'),
        Some(b'[') => Some(b']'),
        Some(b'{') => Some(b'}'),
        _ => None,
    }
}
