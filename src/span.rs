#[derive(Clone)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub fn new(line: u32, col: u32) -> Pos {
        Pos { line, col }
    }

    pub fn copy(&self) -> Pos {
        Pos {
            line: self.line,
            col: self.col,
        }
    }
}

#[derive(Clone)]
pub struct Span {
    pub start: Pos,
    pub end: Pos,
}

impl Span {
    pub fn new(start: Pos, end: Pos) -> Span {
        Span { start, end }
    }

    pub fn copy(&self) -> Span {
        Span {
            start: self.start.copy(),
            end: self.end.copy(),
        }
    }
}

pub struct Error {
    pub file: String,
    pub message: String,
    pub span: Span,
}

pub fn format_error(err: &Error) -> String {
    format!(
        "{}:{}:{}: {}",
        err.file, err.span.start.line, err.span.start.col, err.message
    )
}
