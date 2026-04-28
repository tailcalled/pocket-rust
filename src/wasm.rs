pub struct Module {
    pub types: Vec<FuncType>,
    pub functions: Vec<u32>,
    pub exports: Vec<Export>,
    pub code: Vec<FuncBody>,
}

pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

pub enum ValType {
    I32,
    I64,
}

impl ValType {
    pub fn copy(&self) -> ValType {
        match self {
            ValType::I32 => ValType::I32,
            ValType::I64 => ValType::I64,
        }
    }
}

fn val_type_eq(a: &ValType, b: &ValType) -> bool {
    match (a, b) {
        (ValType::I32, ValType::I32) => true,
        (ValType::I64, ValType::I64) => true,
        _ => false,
    }
}

pub struct Export {
    pub name: String,
    pub kind: ExportKind,
    pub index: u32,
}

pub enum ExportKind {
    Func,
}

pub struct FuncBody {
    pub locals: Vec<ValType>,
    pub instructions: Vec<Instruction>,
}

pub enum Instruction {
    I32Const(i32),
    I64Const(i64),
    LocalGet(u32),
    LocalSet(u32),
    Drop,
    Call(u32),
}

impl Module {
    pub fn new() -> Module {
        Module {
            types: Vec::new(),
            functions: Vec::new(),
            exports: Vec::new(),
            code: Vec::new(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.push(0x00);
        bytes.push(0x61);
        bytes.push(0x73);
        bytes.push(0x6d);
        bytes.push(0x01);
        bytes.push(0x00);
        bytes.push(0x00);
        bytes.push(0x00);
        if !self.types.is_empty() {
            encode_type_section(&mut bytes, &self.types);
        }
        if !self.functions.is_empty() {
            encode_function_section(&mut bytes, &self.functions);
        }
        if !self.exports.is_empty() {
            encode_export_section(&mut bytes, &self.exports);
        }
        if !self.code.is_empty() {
            encode_code_section(&mut bytes, &self.code);
        }
        bytes
    }
}

fn encode_section(out: &mut Vec<u8>, id: u8, payload: Vec<u8>) {
    out.push(id);
    write_uleb128(out, payload.len() as u32);
    push_bytes(out, &payload);
}

fn encode_type_section(out: &mut Vec<u8>, types: &Vec<FuncType>) {
    let mut payload: Vec<u8> = Vec::new();
    write_uleb128(&mut payload, types.len() as u32);
    let mut i = 0;
    while i < types.len() {
        encode_func_type(&mut payload, &types[i]);
        i += 1;
    }
    encode_section(out, 1, payload);
}

fn encode_func_type(out: &mut Vec<u8>, ft: &FuncType) {
    out.push(0x60);
    write_uleb128(out, ft.params.len() as u32);
    let mut i = 0;
    while i < ft.params.len() {
        out.push(val_type_byte(&ft.params[i]));
        i += 1;
    }
    write_uleb128(out, ft.results.len() as u32);
    let mut j = 0;
    while j < ft.results.len() {
        out.push(val_type_byte(&ft.results[j]));
        j += 1;
    }
}

fn val_type_byte(t: &ValType) -> u8 {
    match t {
        ValType::I32 => 0x7f,
        ValType::I64 => 0x7e,
    }
}

fn encode_function_section(out: &mut Vec<u8>, funcs: &Vec<u32>) {
    let mut payload: Vec<u8> = Vec::new();
    write_uleb128(&mut payload, funcs.len() as u32);
    let mut i = 0;
    while i < funcs.len() {
        write_uleb128(&mut payload, funcs[i]);
        i += 1;
    }
    encode_section(out, 3, payload);
}

fn encode_export_section(out: &mut Vec<u8>, exports: &Vec<Export>) {
    let mut payload: Vec<u8> = Vec::new();
    write_uleb128(&mut payload, exports.len() as u32);
    let mut i = 0;
    while i < exports.len() {
        encode_export(&mut payload, &exports[i]);
        i += 1;
    }
    encode_section(out, 7, payload);
}

fn encode_export(out: &mut Vec<u8>, exp: &Export) {
    let name_bytes = exp.name.as_bytes();
    write_uleb128(out, name_bytes.len() as u32);
    push_bytes(out, name_bytes);
    out.push(export_kind_byte(&exp.kind));
    write_uleb128(out, exp.index);
}

fn export_kind_byte(k: &ExportKind) -> u8 {
    match k {
        ExportKind::Func => 0x00,
    }
}

fn encode_code_section(out: &mut Vec<u8>, bodies: &Vec<FuncBody>) {
    let mut payload: Vec<u8> = Vec::new();
    write_uleb128(&mut payload, bodies.len() as u32);
    let mut i = 0;
    while i < bodies.len() {
        encode_func_body(&mut payload, &bodies[i]);
        i += 1;
    }
    encode_section(out, 10, payload);
}

fn encode_func_body(out: &mut Vec<u8>, body: &FuncBody) {
    let mut buf: Vec<u8> = Vec::new();
    encode_locals(&mut buf, &body.locals);
    let mut i = 0;
    while i < body.instructions.len() {
        encode_instruction(&mut buf, &body.instructions[i]);
        i += 1;
    }
    buf.push(0x0b);
    write_uleb128(out, buf.len() as u32);
    push_bytes(out, &buf);
}

fn encode_locals(out: &mut Vec<u8>, locals: &Vec<ValType>) {
    if locals.is_empty() {
        write_uleb128(out, 0);
        return;
    }
    let mut group_count: u32 = 1;
    let mut i = 1;
    while i < locals.len() {
        if !val_type_eq(&locals[i], &locals[i - 1]) {
            group_count += 1;
        }
        i += 1;
    }
    write_uleb128(out, group_count);
    let mut start = 0;
    while start < locals.len() {
        let mut end = start + 1;
        while end < locals.len() && val_type_eq(&locals[end], &locals[start]) {
            end += 1;
        }
        write_uleb128(out, (end - start) as u32);
        out.push(val_type_byte(&locals[start]));
        start = end;
    }
}

fn encode_instruction(out: &mut Vec<u8>, inst: &Instruction) {
    match inst {
        Instruction::I32Const(n) => {
            out.push(0x41);
            write_sleb128(out, *n);
        }
        Instruction::I64Const(n) => {
            out.push(0x42);
            write_sleb128_64(out, *n);
        }
        Instruction::LocalGet(idx) => {
            out.push(0x20);
            write_uleb128(out, *idx);
        }
        Instruction::LocalSet(idx) => {
            out.push(0x21);
            write_uleb128(out, *idx);
        }
        Instruction::Drop => {
            out.push(0x1a);
        }
        Instruction::Call(idx) => {
            out.push(0x10);
            write_uleb128(out, *idx);
        }
    }
}

fn push_bytes(out: &mut Vec<u8>, src: &[u8]) {
    let mut i = 0;
    while i < src.len() {
        out.push(src[i]);
        i += 1;
    }
}

fn write_uleb128(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let low = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

fn write_sleb128(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let sign_bit_set = (byte & 0x40) != 0;
        let done = (value == 0 && !sign_bit_set) || (value == -1 && sign_bit_set);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_sleb128_64(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let sign_bit_set = (byte & 0x40) != 0;
        let done = (value == 0 && !sign_bit_set) || (value == -1 && sign_bit_set);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
