use crate::span::{Error, Pos, Span};

pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub enum TokenKind {
    Fn,
    Mod,
    Struct,
    Let,
    Mut,
    Const,
    As,
    Unsafe,
    Impl,
    SelfLower,
    SelfUpper,
    LAngle,
    RAngle,
    Ident(String),
    // `'name` — a lifetime token. The string is the bare name (no leading apostrophe).
    Lifetime(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Arrow,
    Semi,
    Colon,
    Dot,
    PathSep,
    Comma,
    Amp,
    Star,
    Eq,
    IntLit(u64),
}

pub fn token_kind_name(t: &TokenKind) -> &'static str {
    match t {
        TokenKind::Fn => "`fn`",
        TokenKind::Mod => "`mod`",
        TokenKind::Struct => "`struct`",
        TokenKind::Let => "`let`",
        TokenKind::Mut => "`mut`",
        TokenKind::Const => "`const`",
        TokenKind::As => "`as`",
        TokenKind::Unsafe => "`unsafe`",
        TokenKind::Impl => "`impl`",
        TokenKind::SelfLower => "`self`",
        TokenKind::SelfUpper => "`Self`",
        TokenKind::LAngle => "`<`",
        TokenKind::RAngle => "`>`",
        TokenKind::Ident(_) => "identifier",
        TokenKind::Lifetime(_) => "lifetime",
        TokenKind::LParen => "`(`",
        TokenKind::RParen => "`)`",
        TokenKind::LBrace => "`{`",
        TokenKind::RBrace => "`}`",
        TokenKind::Arrow => "`->`",
        TokenKind::Semi => "`;`",
        TokenKind::Colon => "`:`",
        TokenKind::Dot => "`.`",
        TokenKind::PathSep => "`::`",
        TokenKind::Comma => "`,`",
        TokenKind::Amp => "`&`",
        TokenKind::Star => "`*`",
        TokenKind::Eq => "`=`",
        TokenKind::IntLit(_) => "integer literal",
    }
}

pub fn tokenize(file: &str, source: &str) -> Result<Vec<Token>, Error> {
    let bytes = source.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut byte_pos: usize = 0;
    let mut line: u32 = 1;
    let mut col: u32 = 1;

    while byte_pos < bytes.len() {
        let b = bytes[byte_pos];

        if b == b'\n' {
            byte_pos += 1;
            line += 1;
            col = 1;
        } else if b == b' ' || b == b'\t' || b == b'\r' {
            byte_pos += 1;
            col += 1;
        } else if is_ident_start(b) {
            let start = Pos::new(line, col);
            let start_byte = byte_pos;
            while byte_pos < bytes.len() && is_ident_continue(bytes[byte_pos]) {
                byte_pos += 1;
                col += 1;
            }
            let text = source[start_byte..byte_pos].to_string();
            let end = Pos::new(line, col);
            let span = Span::new(start, end);
            if text == "fn" {
                tokens.push(Token {
                    kind: TokenKind::Fn,
                    span,
                });
            } else if text == "mod" {
                tokens.push(Token {
                    kind: TokenKind::Mod,
                    span,
                });
            } else if text == "struct" {
                tokens.push(Token {
                    kind: TokenKind::Struct,
                    span,
                });
            } else if text == "let" {
                tokens.push(Token {
                    kind: TokenKind::Let,
                    span,
                });
            } else if text == "mut" {
                tokens.push(Token {
                    kind: TokenKind::Mut,
                    span,
                });
            } else if text == "const" {
                tokens.push(Token {
                    kind: TokenKind::Const,
                    span,
                });
            } else if text == "as" {
                tokens.push(Token {
                    kind: TokenKind::As,
                    span,
                });
            } else if text == "unsafe" {
                tokens.push(Token {
                    kind: TokenKind::Unsafe,
                    span,
                });
            } else if text == "impl" {
                tokens.push(Token {
                    kind: TokenKind::Impl,
                    span,
                });
            } else if text == "self" {
                tokens.push(Token {
                    kind: TokenKind::SelfLower,
                    span,
                });
            } else if text == "Self" {
                tokens.push(Token {
                    kind: TokenKind::SelfUpper,
                    span,
                });
            } else {
                tokens.push(Token {
                    kind: TokenKind::Ident(text),
                    span,
                });
            }
        } else if is_digit(b) {
            let start = Pos::new(line, col);
            let start_byte = byte_pos;
            while byte_pos < bytes.len() && is_digit(bytes[byte_pos]) {
                byte_pos += 1;
                col += 1;
            }
            let digits = source[start_byte..byte_pos].as_bytes();
            let mut value: u64 = 0;
            let mut i = 0;
            while i < digits.len() {
                let digit = (digits[i] - b'0') as u64;
                value = value * 10 + digit;
                i += 1;
            }
            let end = Pos::new(line, col);
            tokens.push(Token {
                kind: TokenKind::IntLit(value),
                span: Span::new(start, end),
            });
        } else if b == b'(' {
            push_single(&mut tokens, TokenKind::LParen, line, &mut col);
            byte_pos += 1;
        } else if b == b')' {
            push_single(&mut tokens, TokenKind::RParen, line, &mut col);
            byte_pos += 1;
        } else if b == b'{' {
            push_single(&mut tokens, TokenKind::LBrace, line, &mut col);
            byte_pos += 1;
        } else if b == b'}' {
            push_single(&mut tokens, TokenKind::RBrace, line, &mut col);
            byte_pos += 1;
        } else if b == b';' {
            push_single(&mut tokens, TokenKind::Semi, line, &mut col);
            byte_pos += 1;
        } else if b == b',' {
            push_single(&mut tokens, TokenKind::Comma, line, &mut col);
            byte_pos += 1;
        } else if b == b'.' {
            push_single(&mut tokens, TokenKind::Dot, line, &mut col);
            byte_pos += 1;
        } else if b == b'&' {
            push_single(&mut tokens, TokenKind::Amp, line, &mut col);
            byte_pos += 1;
        } else if b == b'*' {
            push_single(&mut tokens, TokenKind::Star, line, &mut col);
            byte_pos += 1;
        } else if b == b'=' {
            push_single(&mut tokens, TokenKind::Eq, line, &mut col);
            byte_pos += 1;
        } else if b == b'<' {
            push_single(&mut tokens, TokenKind::LAngle, line, &mut col);
            byte_pos += 1;
        } else if b == b'>' {
            push_single(&mut tokens, TokenKind::RAngle, line, &mut col);
            byte_pos += 1;
        } else if b == b'\'' {
            // `'name` — a lifetime. Single quote followed by an ident.
            // (We don't have char literals, so the syntax is unambiguous.)
            let start = Pos::new(line, col);
            let apos_pos = byte_pos;
            byte_pos += 1;
            col += 1;
            if byte_pos >= bytes.len() || !is_ident_start(bytes[byte_pos]) {
                let end = Pos::new(line, col);
                return Err(Error {
                    file: file.to_string(),
                    message: "expected lifetime name after `'`".to_string(),
                    span: Span::new(start, end),
                });
            }
            let name_start = byte_pos;
            while byte_pos < bytes.len() && is_ident_continue(bytes[byte_pos]) {
                byte_pos += 1;
                col += 1;
            }
            let name = source[name_start..byte_pos].to_string();
            let end = Pos::new(line, col);
            let _ = apos_pos;
            tokens.push(Token {
                kind: TokenKind::Lifetime(name),
                span: Span::new(start, end),
            });
        } else if b == b'-' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'>' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token {
                kind: TokenKind::Arrow,
                span: Span::new(start, end),
            });
            byte_pos += 2;
        } else if b == b':' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b':' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token {
                kind: TokenKind::PathSep,
                span: Span::new(start, end),
            });
            byte_pos += 2;
        } else if b == b':' {
            push_single(&mut tokens, TokenKind::Colon, line, &mut col);
            byte_pos += 1;
        } else {
            let start = Pos::new(line, col);
            let end = Pos::new(line, col + 1);
            return Err(Error {
                file: file.to_string(),
                message: format!("unexpected character `{}`", b as char),
                span: Span::new(start, end),
            });
        }
    }

    Ok(tokens)
}

fn push_single(tokens: &mut Vec<Token>, kind: TokenKind, line: u32, col: &mut u32) {
    let start = Pos::new(line, *col);
    let end = Pos::new(line, *col + 1);
    *col += 1;
    tokens.push(Token {
        kind,
        span: Span::new(start, end),
    });
}

fn is_ident_start(b: u8) -> bool {
    (b >= b'a' && b <= b'z') || (b >= b'A' && b <= b'Z') || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    is_ident_start(b) || is_digit(b)
}

fn is_digit(b: u8) -> bool {
    b >= b'0' && b <= b'9'
}
