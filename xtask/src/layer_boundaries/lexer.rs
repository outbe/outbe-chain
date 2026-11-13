pub(super) struct Lexer<'a> {
    pub(super) src: &'a str,
    pub(super) bytes: &'a [u8],
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

    pub(super) fn skip_ascii_ws(&mut self) {
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
