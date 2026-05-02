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
    // Compound-assignment operators. Each is a single token; the
    // parser desugars `a OP= b` to `a.op_assign(b)` (autoref'd
    // `&mut self`) for the matching `OpAssign<Rhs = Self>` trait
    // (`std::ops::AddAssign` / `SubAssign` / `MulAssign` /
    // `DivAssign` / `RemAssign`).
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
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
    // `'X'` / `'\n'` / `'¥'` — char literal. The u32 is the Unicode
    // codepoint after UTF-8 decoding (and escape resolution). Distinct
    // from `IntLit` so the parser/typeck can give it `RType::Char`
    // rather than the integer-literal default. Range is 0..=0x10FFFF
    // excluding the surrogate range 0xD800..=0xDFFF (validated at lex
    // time per Rust's `char::from_u32` rules).
    CharLit(u32),
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
        TokenKind::PlusEq => "`+=`",
        TokenKind::MinusEq => "`-=`",
        TokenKind::StarEq => "`*=`",
        TokenKind::SlashEq => "`/=`",
        TokenKind::PercentEq => "`%=`",
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
        TokenKind::CharLit(_) => "character literal",
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
        } else if b == b'*' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::StarEq, span: Span::new(start, end) });
            byte_pos += 2;
        } else if b == b'*' {
            push_single(&mut tokens, TokenKind::Star, line, &mut col);
            byte_pos += 1;
        } else if b == b'+' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::PlusEq, span: Span::new(start, end) });
            byte_pos += 2;
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
        } else if b == b'/' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::SlashEq, span: Span::new(start, end) });
            byte_pos += 2;
        } else if b == b'/' {
            push_single(&mut tokens, TokenKind::Slash, line, &mut col);
            byte_pos += 1;
        } else if b == b'%' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::PercentEq, span: Span::new(start, end) });
            byte_pos += 2;
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
            // Single-quote: either a char literal (`'X'`, `'\n'`,
            // `'¥'`, …) or a lifetime (`'a`). Disambiguate by peeking
            // past the apostrophe:
            // - if the next byte is `\`, char literal (escape).
            // - else if the next byte starts a non-ASCII UTF-8
            //   sequence (≥ 0x80), char literal (multi-byte char).
            // - else, peek the byte after the single ASCII char: if
            //   it's `'`, single-char literal; otherwise lifetime.
            let start = Pos::new(line, col);
            let next = if byte_pos + 1 < bytes.len() { Some(bytes[byte_pos + 1]) } else { None };
            let after = if byte_pos + 2 < bytes.len() { Some(bytes[byte_pos + 2]) } else { None };
            let is_char_lit = matches!(next, Some(b'\\'))
                || matches!(next, Some(n) if n >= 0x80)
                || matches!(after, Some(b'\''));
            if is_char_lit {
                byte_pos += 1;
                col += 1;
                let (value, byte_len) = if bytes[byte_pos] == b'\\' {
                    // Escape. Recognized: \n \r \t \\ \' \" \0 \xNN \u{HHHHHH}.
                    if byte_pos + 1 >= bytes.len() {
                        let end = Pos::new(line, col);
                        return Err(Error {
                            file: file.to_string(),
                            message: "unterminated char literal".to_string(),
                            span: Span::new(start, end),
                        });
                    }
                    let esc = bytes[byte_pos + 1];
                    let (v, used) = match esc {
                        b'n' => (b'\n' as u32, 2),
                        b'r' => (b'\r' as u32, 2),
                        b't' => (b'\t' as u32, 2),
                        b'\\' => (b'\\' as u32, 2),
                        b'\'' => (b'\'' as u32, 2),
                        b'"' => (b'"' as u32, 2),
                        b'0' => (0u32, 2),
                        b'x' => {
                            if byte_pos + 3 >= bytes.len() {
                                let end = Pos::new(line, col);
                                return Err(Error {
                                    file: file.to_string(),
                                    message: "unterminated `\\x` escape in char literal".to_string(),
                                    span: Span::new(start, end),
                                });
                            }
                            let hi = hex_digit_value(bytes[byte_pos + 2]);
                            let lo = hex_digit_value(bytes[byte_pos + 3]);
                            match (hi, lo) {
                                (Some(h), Some(l)) => {
                                    let v = (h as u32) * 16 + (l as u32);
                                    // `\xNN` in char literals is
                                    // restricted to 0x00..=0x7F per
                                    // Rust (so it always decodes to
                                    // a valid ASCII codepoint).
                                    if v > 0x7F {
                                        let end = Pos::new(line, col);
                                        return Err(Error {
                                            file: file.to_string(),
                                            message: "`\\xNN` char escape must be ASCII (0x00..=0x7F)".to_string(),
                                            span: Span::new(start, end),
                                        });
                                    }
                                    (v, 4)
                                }
                                _ => {
                                    let end = Pos::new(line, col);
                                    return Err(Error {
                                        file: file.to_string(),
                                        message: "invalid hex digit in `\\x` char escape".to_string(),
                                        span: Span::new(start, end),
                                    });
                                }
                            }
                        }
                        b'u' => {
                            // `\u{HHHHHH}` — 1..=6 hex digits, codepoint.
                            if byte_pos + 2 >= bytes.len() || bytes[byte_pos + 2] != b'{' {
                                let end = Pos::new(line, col);
                                return Err(Error {
                                    file: file.to_string(),
                                    message: "expected `{` after `\\u` in char literal".to_string(),
                                    span: Span::new(start, end),
                                });
                            }
                            let mut p = byte_pos + 3;
                            let mut cp: u32 = 0;
                            let mut digits = 0;
                            while p < bytes.len() && bytes[p] != b'}' {
                                let d = match hex_digit_value(bytes[p]) {
                                    Some(d) => d,
                                    None => {
                                        let end = Pos::new(line, col);
                                        return Err(Error {
                                            file: file.to_string(),
                                            message: "invalid hex digit in `\\u{…}` escape".to_string(),
                                            span: Span::new(start, end),
                                        });
                                    }
                                };
                                cp = cp.checked_mul(16).and_then(|v| v.checked_add(d as u32)).unwrap_or(0x110000);
                                digits += 1;
                                if digits > 6 {
                                    let end = Pos::new(line, col);
                                    return Err(Error {
                                        file: file.to_string(),
                                        message: "too many hex digits in `\\u{…}` escape (max 6)".to_string(),
                                        span: Span::new(start, end),
                                    });
                                }
                                p += 1;
                            }
                            if p >= bytes.len() || digits == 0 {
                                let end = Pos::new(line, col);
                                return Err(Error {
                                    file: file.to_string(),
                                    message: "unterminated `\\u{…}` escape".to_string(),
                                    span: Span::new(start, end),
                                });
                            }
                            (cp, p + 1 - byte_pos)
                        }
                        _ => {
                            let end = Pos::new(line, col);
                            return Err(Error {
                                file: file.to_string(),
                                message: format!("unknown char escape `\\{}`", esc as char),
                                span: Span::new(start, end),
                            });
                        }
                    };
                    (v, used)
                } else {
                    // UTF-8 sequence. Decode 1-4 bytes per the spec
                    // and produce the codepoint.
                    let lead = bytes[byte_pos];
                    let (cp, len) = decode_utf8_codepoint(bytes, byte_pos)
                        .map_err(|m| Error {
                            file: file.to_string(),
                            message: m,
                            span: Span::new(start.copy(), Pos::new(line, col)),
                        })?;
                    let _ = lead;
                    (cp, len)
                };
                byte_pos += byte_len;
                col += byte_len as u32;
                // Validate codepoint: 0..=0x10FFFF, excluding the
                // surrogate range 0xD800..=0xDFFF (per Rust's
                // `char::from_u32`).
                if value > 0x10FFFF || (value >= 0xD800 && value <= 0xDFFF) {
                    let end = Pos::new(line, col);
                    return Err(Error {
                        file: file.to_string(),
                        message: format!("invalid Unicode codepoint U+{:04X} in char literal", value),
                        span: Span::new(start, end),
                    });
                }
                if byte_pos >= bytes.len() || bytes[byte_pos] != b'\'' {
                    let end = Pos::new(line, col);
                    return Err(Error {
                        file: file.to_string(),
                        message: "expected `'` to close char literal".to_string(),
                        span: Span::new(start, end),
                    });
                }
                byte_pos += 1;
                col += 1;
                let end = Pos::new(line, col);
                tokens.push(Token {
                    kind: TokenKind::CharLit(value),
                    span: Span::new(start, end),
                });
            } else {
                // Lifetime: `'name`.
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
            }
        } else if b == b'-' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'>' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token {
                kind: TokenKind::Arrow,
                span: Span::new(start, end),
            });
            byte_pos += 2;
        } else if b == b'-' && (byte_pos + 1) < bytes.len() && bytes[byte_pos + 1] == b'=' {
            let start = Pos::new(line, col);
            col += 2;
            let end = Pos::new(line, col);
            tokens.push(Token { kind: TokenKind::MinusEq, span: Span::new(start, end) });
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

// Decode one UTF-8 codepoint starting at `bytes[start]`. Returns the
// codepoint plus the number of bytes consumed (1-4). Validates the
// continuation-byte pattern and the canonical-form rule (no
// over-long encodings); rejects invalid sequences.
fn decode_utf8_codepoint(bytes: &[u8], start: usize) -> Result<(u32, usize), String> {
    if start >= bytes.len() {
        return Err("unexpected end of input in char literal".to_string());
    }
    let b0 = bytes[start];
    // ASCII fast-path (0xxxxxxx).
    if b0 < 0x80 {
        return Ok((b0 as u32, 1));
    }
    // Multi-byte sequences. Determine length from the leading byte.
    let (len, mask, min_cp) = if b0 & 0b1110_0000 == 0b1100_0000 {
        (2, 0b0001_1111u32, 0x80u32)
    } else if b0 & 0b1111_0000 == 0b1110_0000 {
        (3, 0b0000_1111u32, 0x800u32)
    } else if b0 & 0b1111_1000 == 0b1111_0000 {
        (4, 0b0000_0111u32, 0x10000u32)
    } else {
        return Err(format!("invalid UTF-8 leading byte 0x{:02X}", b0));
    };
    if start + len > bytes.len() {
        return Err("truncated UTF-8 sequence in char literal".to_string());
    }
    let mut cp: u32 = (b0 as u32) & mask;
    let mut i = 1;
    while i < len {
        let b = bytes[start + i];
        if b & 0b1100_0000 != 0b1000_0000 {
            return Err(format!("invalid UTF-8 continuation byte 0x{:02X}", b));
        }
        cp = (cp << 6) | ((b as u32) & 0b0011_1111);
        i += 1;
    }
    if cp < min_cp {
        return Err(format!("over-long UTF-8 encoding for codepoint U+{:04X}", cp));
    }
    Ok((cp, len))
}

// Hex digit (0-9, a-f, A-F) → integer value 0..15. None for non-hex.
// Used by char literal `\xNN` escape.
fn hex_digit_value(b: u8) -> Option<u8> {
    if b >= b'0' && b <= b'9' {
        Some(b - b'0')
    } else if b >= b'a' && b <= b'f' {
        Some(b - b'a' + 10)
    } else if b >= b'A' && b <= b'F' {
        Some(b - b'A' + 10)
    } else {
        None
    }
}
