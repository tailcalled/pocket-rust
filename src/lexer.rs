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
    Trait,
    For,
    SelfLower,
    SelfUpper,
    If,
    Else,
    True,
    False,
    Use,
    Pub,
    Enum,
    Match,
    Ref,
    While,
    Break,
    Continue,
    // `type` — only meaningful inside trait bodies (`type Name;`) and
    // impl bodies (`type Name = T;`) so far. Lexed unconditionally; the
    // parser is the one that requires it to be in those positions.
    Type,
    // `return` — early-exit expression. `return EXPR` or `return`.
    Return,
    // `?` — try-operator postfix. `expr?` short-circuits to the
    // enclosing function's `Err(...)` if `expr` is `Err`; otherwise
    // unwraps the `Ok(v)`. Kept as a first-class AST node (no early
    // desugar) so error messages point at the `?` site.
    Question,
    // The currency-sign character `¤` (U+00A4). Prefixes a builtin
    // intrinsic call: `¤name(args)`. The lexer emits this as a
    // standalone token; the following identifier (and parenthesized
    // arg list) belong to the builtin syntax in the parser.
    Builtin,
    LAngle,
    RAngle,
    Ident(String),
    // `'name` — a lifetime token. The string is the bare name (no leading apostrophe).
    Lifetime(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Arrow,
    Semi,
    Colon,
    Dot,
    PathSep,
    Comma,
    Amp,
    Star,
    Plus,
    Minus,
    Slash,
    Percent,
    Bang,
    EqEq,
    NotEq,
    LtEq,
    GtEq,
    Eq,
    // `_` — wildcard / placeholder. Used by patterns (matches anything,
    // binds nothing) and `let _ = …;`. Lexed as a standalone token, not
    // an identifier — pure-`_` ident isn't a binding name.
    Underscore,
    // `|` — pipe. Or-patterns (`A | B | C`).
    Pipe,
    // `@` — pattern at-binding (`name @ subpattern`).
    At,
    // `..` — rest pattern in struct/tuple patterns (`Foo { x, .. }`).
    DotDot,
    // `..=` — inclusive range, used in range patterns (`0..=9`).
    DotDotEq,
    IntLit(u64),
    // `"..."` — UTF-8 string literal. The `String` carries the
    // **decoded** content (escape sequences already resolved); since
    // pocket-rust source is UTF-8 and `\xNN` is restricted to ASCII
    // (per Rust), the result is always valid UTF-8. A future
    // `b"..."` byte-literal would carry `Vec<u8>` to express that
    // it can hold arbitrary bytes.
    StrLit(String),
    FatArrow,
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
        TokenKind::Trait => "`trait`",
        TokenKind::Type => "`type`",
        TokenKind::Return => "`return`",
        TokenKind::Question => "`?`",
        TokenKind::For => "`for`",
        TokenKind::SelfLower => "`self`",
        TokenKind::SelfUpper => "`Self`",
        TokenKind::If => "`if`",
        TokenKind::Else => "`else`",
        TokenKind::True => "`true`",
        TokenKind::False => "`false`",
        TokenKind::Use => "`use`",
        TokenKind::Pub => "`pub`",
        TokenKind::Enum => "`enum`",
        TokenKind::Match => "`match`",
        TokenKind::Ref => "`ref`",
        TokenKind::While => "`while`",
        TokenKind::Break => "`break`",
        TokenKind::Continue => "`continue`",
        TokenKind::Builtin => "`¤`",
        TokenKind::LAngle => "`<`",
        TokenKind::RAngle => "`>`",
        TokenKind::Ident(_) => "identifier",
        TokenKind::Lifetime(_) => "lifetime",
        TokenKind::LParen => "`(`",
        TokenKind::RParen => "`)`",
        TokenKind::LBracket => "`[`",
        TokenKind::RBracket => "`]`",
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
        TokenKind::Plus => "`+`",
        TokenKind::Minus => "`-`",
        TokenKind::Slash => "`/`",
        TokenKind::Percent => "`%`",
        TokenKind::Bang => "`!`",
        TokenKind::EqEq => "`==`",
        TokenKind::NotEq => "`!=`",
        TokenKind::LtEq => "`<=`",
        TokenKind::GtEq => "`>=`",
        TokenKind::Eq => "`=`",
        TokenKind::Underscore => "`_`",
        TokenKind::Pipe => "`|`",
        TokenKind::At => "`@`",
        TokenKind::DotDot => "`..`",
        TokenKind::DotDotEq => "`..=`",
        TokenKind::FatArrow => "`=>`",
        TokenKind::IntLit(_) => "integer literal",
        TokenKind::StrLit(_) => "string literal",
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
            } else if text == "trait" {
                tokens.push(Token {
                    kind: TokenKind::Trait,
                    span,
                });
            } else if text == "type" {
                tokens.push(Token {
                    kind: TokenKind::Type,
                    span,
                });
            } else if text == "return" {
                tokens.push(Token {
                    kind: TokenKind::Return,
                    span,
                });
            } else if text == "for" {
                tokens.push(Token {
                    kind: TokenKind::For,
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
            } else if text == "if" {
                tokens.push(Token {
                    kind: TokenKind::If,
                    span,
                });
            } else if text == "else" {
                tokens.push(Token {
                    kind: TokenKind::Else,
                    span,
                });
            } else if text == "true" {
                tokens.push(Token {
                    kind: TokenKind::True,
                    span,
                });
            } else if text == "false" {
                tokens.push(Token {
                    kind: TokenKind::False,
                    span,
                });
            } else if text == "use" {
                tokens.push(Token {
                    kind: TokenKind::Use,
                    span,
                });
            } else if text == "pub" {
                tokens.push(Token {
                    kind: TokenKind::Pub,
                    span,
                });
            } else if text == "enum" {
                tokens.push(Token {
                    kind: TokenKind::Enum,
                    span,
                });
            } else if text == "match" {
                tokens.push(Token {
                    kind: TokenKind::Match,
                    span,
                });
            } else if text == "ref" {
                tokens.push(Token {
                    kind: TokenKind::Ref,
                    span,
                });
            } else if text == "while" {
                tokens.push(Token {
                    kind: TokenKind::While,
                    span,
                });
            } else if text == "break" {
                tokens.push(Token {
                    kind: TokenKind::Break,
                    span,
                });
            } else if text == "continue" {
                tokens.push(Token {
                    kind: TokenKind::Continue,
                    span,
                });
            } else if text == "_" {
                tokens.push(Token {
                    kind: TokenKind::Underscore,
                    span,
                });
            } else {
                tokens.push(Token {
                    kind: TokenKind::Ident(text),
                    span,
                });
            }
        } else if b == b'"' {
            // String literal `"..."`. Recognized escapes match the
            // common subset of Rust's: `\n`, `\r`, `\t`, `\\`, `\"`,
            // `\0`. Everything else is a lex error. Unterminated
            // strings (EOF before closing `"`) are also rejected.
            //
            // We collect the decoded payload as raw bytes and convert
            // to `String` at the end. This preserves any multi-byte
            // UTF-8 sequence from the source file verbatim — casting
            // each byte to `char` along the way would re-interpret
            // them as codepoints, which is wrong for bytes ≥ 0x80.
            // Since the source is UTF-8 and our escapes only emit
            // ASCII-range bytes, the assembled buffer is always valid
            // UTF-8 and `String::from_utf8` succeeds.
            let start = Pos::new(line, col);
            byte_pos += 1;
            col += 1;
            let mut decoded: Vec<u8> = Vec::new();
            let mut closed = false;
            while byte_pos < bytes.len() {
                let c = bytes[byte_pos];
                if c == b'"' {
                    byte_pos += 1;
                    col += 1;
                    closed = true;
                    break;
                }
                if c == b'\\' {
                    if byte_pos + 1 >= bytes.len() {
                        return Err(Error {
                            file: file.to_string(),
                            message: "unterminated string literal".to_string(),
                            span: Span::new(start.copy(), Pos::new(line, col)),
                        });
                    }
                    let esc = bytes[byte_pos + 1];
                    let resolved: u8 = match esc {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'\\' => b'\\',
                        b'"' => b'"',
                        b'0' => 0,
                        _ => {
                            let span = Span::new(
                                Pos::new(line, col),
                                Pos::new(line, col + 2),
                            );
                            return Err(Error {
                                file: file.to_string(),
                                message: format!(
                                    "unknown escape sequence `\\{}`",
                                    esc as char
                                ),
                                span,
                            });
                        }
                    };
                    decoded.push(resolved);
                    byte_pos += 2;
                    col += 2;
                    continue;
                }
                if c == b'\n' {
                    decoded.push(b'\n');
                    byte_pos += 1;
                    line += 1;
                    col = 1;
                    continue;
                }
                // Any other byte: copy through verbatim, preserving
                // multi-byte UTF-8 sequences from the source.
                decoded.push(c);
                byte_pos += 1;
                col += 1;
            }
            if !closed {
                return Err(Error {
                    file: file.to_string(),
                    message: "unterminated string literal".to_string(),
                    span: Span::new(start.copy(), Pos::new(line, col)),
                });
            }
            let end = Pos::new(line, col);
            let payload = String::from_utf8(decoded)
                .expect("source is UTF-8 and escapes only emit ASCII bytes");
            tokens.push(Token {
                kind: TokenKind::StrLit(payload),
                span: Span::new(start, end),
            });
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
        } else if b == b'[' {
            push_single(&mut tokens, TokenKind::LBracket, line, &mut col);
            byte_pos += 1;
        } else if b == b']' {
            push_single(&mut tokens, TokenKind::RBracket, line, &mut col);
            byte_pos += 1;
        } else if b == b';' {
            push_single(&mut tokens, TokenKind::Semi, line, &mut col);
            byte_pos += 1;
        } else if b == b',' {
            push_single(&mut tokens, TokenKind::Comma, line, &mut col);
            byte_pos += 1;
        } else if b == b'.'
            && (byte_pos + 2) < bytes.len()
            && bytes[byte_pos + 1] == b'.'
            && bytes[byte_pos + 2] == b'='
        {
            let start = Pos::new(line, col);
            col += 3;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::DotDotEq, span: Span::new(start, end) });
            byte_pos += 3;
        } else if b == b'.' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'.' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::DotDot, span: Span::new(start, end) });
            byte_pos += 2;
        } else if b == b'.' {
            push_single(&mut tokens, TokenKind::Dot, line, &mut col);
            byte_pos += 1;
        } else if b == b'&' {
            push_single(&mut tokens, TokenKind::Amp, line, &mut col);
            byte_pos += 1;
        } else if b == b'*' {
            push_single(&mut tokens, TokenKind::Star, line, &mut col);
            byte_pos += 1;
        } else if b == b'+' {
            push_single(&mut tokens, TokenKind::Plus, line, &mut col);
            byte_pos += 1;
        } else if b == b'/'
            && (byte_pos + 1) < bytes.len()
            && bytes[byte_pos + 1] == b'/'
        {
            // Line comment: skip from `//` through end-of-line. The
            // newline itself is left for the outer whitespace handler
            // so line/col tracking stays uniform.
            byte_pos += 2;
            col += 2;
            while byte_pos < bytes.len() && bytes[byte_pos] != b'\n' {
                byte_pos += 1;
                col += 1;
            }
        } else if b == b'/'
            && (byte_pos + 1) < bytes.len()
            && bytes[byte_pos + 1] == b'*'
        {
            // Block comment: skip until matching `*/`. Nested `/* */`
            // pairs are honored — `cargo` lexes Rust the same way.
            byte_pos += 2;
            col += 2;
            let mut depth: u32 = 1;
            while byte_pos < bytes.len() && depth > 0 {
                if (byte_pos + 1) < bytes.len()
                    && bytes[byte_pos] == b'/'
                    && bytes[byte_pos + 1] == b'*'
                {
                    depth += 1;
                    byte_pos += 2;
                    col += 2;
                } else if (byte_pos + 1) < bytes.len()
                    && bytes[byte_pos] == b'*'
                    && bytes[byte_pos + 1] == b'/'
                {
                    depth -= 1;
                    byte_pos += 2;
                    col += 2;
                } else if bytes[byte_pos] == b'\n' {
                    line += 1;
                    col = 1;
                    byte_pos += 1;
                } else {
                    byte_pos += 1;
                    col += 1;
                }
            }
            if depth > 0 {
                return Err(Error {
                    file: file.to_string(),
                    message: "unterminated block comment".to_string(),
                    span: Span::new(
                        Pos::new(line, col),
                        Pos::new(line, col),
                    ),
                });
            }
        } else if b == b'/' {
            push_single(&mut tokens, TokenKind::Slash, line, &mut col);
            byte_pos += 1;
        } else if b == b'%' {
            push_single(&mut tokens, TokenKind::Percent, line, &mut col);
            byte_pos += 1;
        } else if b == b'=' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::EqEq, span: Span::new(start, end) });
            byte_pos += 2;
        } else if b == b'=' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'>' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::FatArrow, span: Span::new(start, end) });
            byte_pos += 2;
        } else if b == b'=' {
            push_single(&mut tokens, TokenKind::Eq, line, &mut col);
            byte_pos += 1;
        } else if b == b'!' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::NotEq, span: Span::new(start, end) });
            byte_pos += 2;
        } else if b == b'!' {
            push_single(&mut tokens, TokenKind::Bang, line, &mut col);
            byte_pos += 1;
        } else if b == b'?' {
            push_single(&mut tokens, TokenKind::Question, line, &mut col);
            byte_pos += 1;
        } else if b == b'<' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::LtEq, span: Span::new(start, end) });
            byte_pos += 2;
        } else if b == b'<' {
            push_single(&mut tokens, TokenKind::LAngle, line, &mut col);
            byte_pos += 1;
        } else if b == b'>' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::GtEq, span: Span::new(start, end) });
            byte_pos += 2;
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
        } else if b == b'-' {
            push_single(&mut tokens, TokenKind::Minus, line, &mut col);
            byte_pos += 1;
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
        } else if b == b'|' {
            push_single(&mut tokens, TokenKind::Pipe, line, &mut col);
            byte_pos += 1;
        } else if b == b'@' {
            push_single(&mut tokens, TokenKind::At, line, &mut col);
            byte_pos += 1;
        } else if b == 0xc2 && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == 0xa4 {
            // `¤` U+00A4 — UTF-8 encoded as the two bytes 0xC2 0xA4.
            // Counts as one column (one user-visible char).
            let start = Pos::new(line, col);
            col += 1;
            let end = Pos::new(line, col);
            tokens.push(Token {
                kind: TokenKind::Builtin,
                span: Span::new(start, end),
            });
            byte_pos += 2;
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
