pub struct Module {
    pub types: Vec<FuncType>,
    pub functions: Vec<u32>,
    pub memories: Vec<Memory>,
    pub globals: Vec<Global>,
    pub exports: Vec<Export>,
    pub code: Vec<FuncBody>,
}

pub struct Memory {
    pub min_pages: u32,
    pub max_pages: Option<u32>,
}

pub struct Global {
    pub ty: ValType,
    pub mutable: bool,
    pub init: Instruction,
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
    GlobalGet(u32),
    GlobalSet(u32),
    Drop,
    Call(u32),
    I32Add,
    I32Sub,
    I32WrapI64,
    I64ExtendI32S,
    I64ExtendI32U,
    I64ShrS,
    I32Load { align: u32, offset: u32 },
    I32Load8U { align: u32, offset: u32 },
    I32Load8S { align: u32, offset: u32 },
    I32Load16U { align: u32, offset: u32 },
    I32Load16S { align: u32, offset: u32 },
    I64Load { align: u32, offset: u32 },
    I32Store { align: u32, offset: u32 },
    I32Store8 { align: u32, offset: u32 },
    I32Store16 { align: u32, offset: u32 },
    I64Store { align: u32, offset: u32 },
}

impl Module {
    pub fn new() -> Module {
        Module {
            types: Vec::new(),
            functions: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
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
        if !self.memories.is_empty() {
            encode_memory_section(&mut bytes, &self.memories);
        }
        if !self.globals.is_empty() {
            encode_global_section(&mut bytes, &self.globals);
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

fn encode_memory_section(out: &mut Vec<u8>, mems: &Vec<Memory>) {
    let mut payload: Vec<u8> = Vec::new();
    write_uleb128(&mut payload, mems.len() as u32);
    let mut i = 0;
    while i < mems.len() {
        encode_limits(&mut payload, mems[i].min_pages, &mems[i].max_pages);
        i += 1;
    }
    encode_section(out, 5, payload);
}

fn encode_limits(out: &mut Vec<u8>, min: u32, max: &Option<u32>) {
    match max {
        Some(m) => {
            out.push(0x01);
            write_uleb128(out, min);
            write_uleb128(out, *m);
        }
        None => {
            out.push(0x00);
            write_uleb128(out, min);
        }
    }
}

fn encode_global_section(out: &mut Vec<u8>, globals: &Vec<Global>) {
    let mut payload: Vec<u8> = Vec::new();
    write_uleb128(&mut payload, globals.len() as u32);
    let mut i = 0;
    while i < globals.len() {
        payload.push(val_type_byte(&globals[i].ty));
        payload.push(if globals[i].mutable { 0x01 } else { 0x00 });
        encode_instruction(&mut payload, &globals[i].init);
        payload.push(0x0b); // end of init expr
        i += 1;
    }
    encode_section(out, 6, payload);
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
        Instruction::GlobalGet(idx) => {
            out.push(0x23);
            write_uleb128(out, *idx);
        }
        Instruction::GlobalSet(idx) => {
            out.push(0x24);
            write_uleb128(out, *idx);
        }
        Instruction::I32Add => {
            out.push(0x6a);
        }
        Instruction::I32Sub => {
            out.push(0x6b);
        }
        Instruction::I32WrapI64 => {
            out.push(0xa7);
        }
        Instruction::I64ExtendI32S => {
            out.push(0xac);
        }
        Instruction::I64ExtendI32U => {
            out.push(0xad);
        }
        Instruction::I64ShrS => {
            out.push(0x87);
        }
        Instruction::I32Load { align, offset } => {
            out.push(0x28);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I32Load8U { align, offset } => {
            out.push(0x2d);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I32Load8S { align, offset } => {
            out.push(0x2c);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I32Load16U { align, offset } => {
            out.push(0x2f);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I32Load16S { align, offset } => {
            out.push(0x2e);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I64Load { align, offset } => {
            out.push(0x29);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I32Store { align, offset } => {
            out.push(0x36);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I32Store8 { align, offset } => {
            out.push(0x3a);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I32Store16 { align, offset } => {
            out.push(0x3b);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
        }
        Instruction::I64Store { align, offset } => {
            out.push(0x37);
            write_uleb128(out, *align);
            write_uleb128(out, *offset);
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
