pub(super) struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pub(super) pos: usize,
    pub(super) line: usize,
}

impl<'a> Lexer<'a> {
    pub(super) const fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
        }
    }

    pub(super) const fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub(super) fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    pub(super) fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    pub(super) fn advance(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        if byte == b'\n' {
            self.line += 1;
        }
        Some(byte)
    }

    pub(super) fn skip_ws_and_comments(&mut self) {
        loop {
            self.skip_ascii_ws();
            if !self.try_skip_line_comment() && !self.try_skip_block_comment() {
                break;
            }
        }
    }

    fn skip_ascii_ws(&mut self) {
        while matches!(self.peek(), Some(byte) if byte.is_ascii_whitespace()) {
            self.advance();
        }
    }

    fn try_skip_line_comment(&mut self) -> bool {
        if self.peek() != Some(b'/') || self.peek_at(1) != Some(b'/') {
            return false;
        }
        while let Some(byte) = self.advance() {
            if byte == b'\n' {
                break;
            }
        }
        true
    }

    fn try_skip_block_comment(&mut self) -> bool {
        if self.peek() != Some(b'/') || self.peek_at(1) != Some(b'*') {
            return false;
        }
        self.advance();
        self.advance();
        let mut depth = 1usize;
        while depth > 0 {
            let Some(byte) = self.advance() else {
                break;
            };
            if byte == b'/' && self.peek() == Some(b'*') {
                self.advance();
                depth += 1;
            } else if byte == b'*' && self.peek() == Some(b'/') {
                self.advance();
                depth -= 1;
            }
        }
        true
    }

    pub(super) fn starts_literal_token(&self) -> bool {
        self.starts_raw_string_at(self.pos) || self.starts_prefixed_literal()
    }

    fn starts_raw_string_at(&self, index: usize) -> bool {
        if self.bytes.get(index) != Some(&b'r') {
            return false;
        }
        let mut cursor = index + 1;
        while self.bytes.get(cursor) == Some(&b'#') {
            cursor += 1;
        }
        self.bytes.get(cursor) == Some(&b'"')
    }

    fn starts_prefixed_literal(&self) -> bool {
        match (self.peek(), self.peek_at(1)) {
            (Some(b'b'), Some(b'"' | b'\'')) | (Some(b'c'), Some(b'"')) => true,
            (Some(b'b' | b'c'), Some(b'r')) => self.starts_raw_string_at(self.pos + 1),
            _ => false,
        }
    }

    pub(super) fn skip_single_atom(&mut self) {
        match self.peek() {
            None => {}
            Some(b'"') => {
                let _ = self.parse_quoted_string();
            }
            Some(b'\'') => self.skip_char_or_lifetime(),
            Some(b'r') if self.starts_raw_string_at(self.pos) => {
                let _ = self.parse_raw_string();
            }
            Some(b'b' | b'c') if self.starts_prefixed_literal() => {
                self.advance();
                self.skip_single_atom();
            }
            Some(_) => {
                self.advance();
            }
        }
    }

    fn skip_char_or_lifetime(&mut self) {
        self.advance();
        if self.peek() == Some(b'\\') {
            self.advance();
            self.advance();
            while let Some(byte) = self.advance() {
                if byte == b'\'' || byte == b'\n' {
                    break;
                }
            }
            return;
        }
        let Some(rest) = self.src.get(self.pos..) else {
            return;
        };
        let mut chars = rest.chars();
        if let (Some(first), Some('\'')) = (chars.next(), chars.next()) {
            for _ in 0..(first.len_utf8() + 1) {
                self.advance();
            }
        }
    }

    pub(super) fn parse_string_literal(&mut self) -> Option<String> {
        self.skip_ws_and_comments();
        if self.starts_raw_string_at(self.pos) {
            self.parse_raw_string()
        } else if self.peek() == Some(b'"') {
            self.parse_quoted_string()
        } else {
            None
        }
    }

    fn parse_raw_string(&mut self) -> Option<String> {
        if self.advance() != Some(b'r') {
            return None;
        }
        let mut hashes = 0usize;
        while self.peek() == Some(b'#') {
            self.advance();
            hashes += 1;
        }
        if self.advance() != Some(b'"') {
            return None;
        }
        let start = self.pos;
        while !self.is_eof() {
            if self.advance()? == b'"' && self.has_hashes(hashes) {
                let end = self.pos - 1;
                for _ in 0..hashes {
                    self.advance();
                }
                return Some(self.src[start..end].to_owned());
            }
        }
        None
    }

    fn has_hashes(&self, count: usize) -> bool {
        (0..count).all(|offset| self.bytes.get(self.pos + offset) == Some(&b'#'))
    }

    fn parse_quoted_string(&mut self) -> Option<String> {
        if self.advance() != Some(b'"') {
            return None;
        }
        let mut value = String::new();
        while !self.is_eof() {
            match self.next_utf8_char()? {
                '"' => return Some(value),
                '\\' => self.consume_escape(&mut value)?,
                other => value.push(other),
            }
        }
        None
    }

    fn next_utf8_char(&mut self) -> Option<char> {
        let ch = self.src.get(self.pos..)?.chars().next()?;
        for _ in 0..ch.len_utf8() {
            self.advance();
        }
        Some(ch)
    }

    fn consume_escape(&mut self, value: &mut String) -> Option<()> {
        match self.next_utf8_char()? {
            'n' => value.push('\n'),
            'r' => value.push('\r'),
            't' => value.push('\t'),
            '\n' => self.skip_ascii_ws(),
            other => value.push(other),
        }
        Some(())
    }

    pub(super) fn read_ident(&mut self) -> Option<&'a str> {
        let start = self.pos;
        if !is_ident_start(self.peek()?) {
            return None;
        }
        while matches!(self.peek(), Some(byte) if is_ident_continue(byte)) {
            self.advance();
        }
        Some(&self.src[start..self.pos])
    }
}

pub(super) const fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
