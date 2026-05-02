pub struct Module {
    pub types: Vec<FuncType>,
    pub functions: Vec<u32>,
    pub memories: Vec<Memory>,
    pub globals: Vec<Global>,
    pub exports: Vec<Export>,
    pub code: Vec<FuncBody>,
    pub datas: Vec<Data>,
}

// Active-mode data segment for memory 0. `offset` is the absolute
// byte address; `bytes` is the payload. Encoded as section id 11.
pub struct Data {
    pub offset: u32,
    pub bytes: Vec<u8>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
    Unreachable,
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
    I32Mul,
    I32DivS,
    I32DivU,
    I32RemS,
    I32RemU,
    I32And,
    I32Or,
    I32Xor,
    I32Eqz,
    I32Eq,
    I32Ne,
    I32LtS,
    I32LtU,
    I32GtS,
    I32GtU,
    I32LeS,
    I32LeU,
    I32GeS,
    I32GeU,
    I64Add,
    I64Sub,
    I64Mul,
    I64DivS,
    I64DivU,
    I64RemS,
    I64RemU,
    I64And,
    I64Or,
    I64Xor,
    I64Eqz,
    I64Eq,
    I64Ne,
    I64LtS,
    I64LtU,
    I64GtS,
    I64GtU,
    I64LeS,
    I64LeU,
    I64GeS,
    I64GeU,
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
    // Structured control flow. `If(BlockType)` opens an if-block whose
    // result is described by the BlockType; `Else` separates the two
    // arms; `End` closes any structured block (`If`, `Block`, etc.).
    // `Block(BlockType)` opens a plain block whose result is described
    // by the BlockType. `Br(label)` jumps out of the labeled enclosing
    // block (label = depth, 0 = innermost); `BrIf(label)` does the same
    // when the i32 on top of the stack is non-zero.
    If(BlockType),
    Else,
    End,
    Block(BlockType),
    // `Loop(BlockType)` opens a loop block. Inside, `Br(0)` jumps
    // *back* to the loop's start (the loop instruction), unlike a
    // plain block where `Br(0)` jumps forward past the closing `End`.
    // Used by while-loop codegen for the back-edge.
    Loop(BlockType),
    Br(u32),
    BrIf(u32),
    // Wasm `return` — pops the function's expected results from the
    // stack and exits. Used by `return EXPR;` (after the value's flat
    // scalars are pushed) and by the `?` operator's Err-path.
    Return,
}

// Wasm structured-control-flow block result. Three encodings, matching
// the wasm core spec:
//   `Empty` — no result on the stack at block end (encoded as 0x40).
//   `Single(vt)` — exactly one scalar, inline-encoded as that valtype's
//     byte.
//   `TypeIdx(i)` — references a pre-registered FuncType (no params, N
//     results), encoded as the type's signed-LEB128 index. Used for
//     multi-value blocks (e.g. an `if` whose tail is a struct that
//     flattens to two or more wasm scalars).
pub enum BlockType {
    Empty,
    Single(ValType),
    TypeIdx(u32),
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
            datas: Vec::new(),
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
        if !self.datas.is_empty() {
            encode_data_section(&mut bytes, &self.datas);
        }
        bytes
    }
}

fn encode_data_section(out: &mut Vec<u8>, datas: &Vec<Data>) {
    let mut payload: Vec<u8> = Vec::new();
    write_uleb128(&mut payload, datas.len() as u32);
    let mut i = 0;
    while i < datas.len() {
        let d = &datas[i];
        // Active-mode segment in memory 0: flag byte = 0x00, offset
        // expression (i32.const N; end), then byte vector.
        payload.push(0x00);
        payload.push(0x41); // i32.const opcode
        write_sleb128(&mut payload, d.offset as i32);
        payload.push(0x0b); // end opcode
        write_uleb128(&mut payload, d.bytes.len() as u32);
        let mut k = 0;
        while k < d.bytes.len() {
            payload.push(d.bytes[k]);
            k += 1;
        }
        i += 1;
    }
    encode_section(out, 11, payload);
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
        Instruction::Unreachable => out.push(0x00),
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
        Instruction::I32Add => out.push(0x6a),
        Instruction::I32Sub => out.push(0x6b),
        Instruction::I32Mul => out.push(0x6c),
        Instruction::I32DivS => out.push(0x6d),
        Instruction::I32DivU => out.push(0x6e),
        Instruction::I32RemS => out.push(0x6f),
        Instruction::I32RemU => out.push(0x70),
        Instruction::I32And => out.push(0x71),
        Instruction::I32Or => out.push(0x72),
        Instruction::I32Xor => out.push(0x73),
        Instruction::I32Eqz => out.push(0x45),
        Instruction::I32Eq => out.push(0x46),
        Instruction::I32Ne => out.push(0x47),
        Instruction::I32LtS => out.push(0x48),
        Instruction::I32LtU => out.push(0x49),
        Instruction::I32GtS => out.push(0x4a),
        Instruction::I32GtU => out.push(0x4b),
        Instruction::I32LeS => out.push(0x4c),
        Instruction::I32LeU => out.push(0x4d),
        Instruction::I32GeS => out.push(0x4e),
        Instruction::I32GeU => out.push(0x4f),
        Instruction::I64Add => out.push(0x7c),
        Instruction::I64Sub => out.push(0x7d),
        Instruction::I64Mul => out.push(0x7e),
        Instruction::I64DivS => out.push(0x7f),
        Instruction::I64DivU => out.push(0x80),
        Instruction::I64RemS => out.push(0x81),
        Instruction::I64RemU => out.push(0x82),
        Instruction::I64And => out.push(0x83),
        Instruction::I64Or => out.push(0x84),
        Instruction::I64Xor => out.push(0x85),
        Instruction::I64Eqz => out.push(0x50),
        Instruction::I64Eq => out.push(0x51),
        Instruction::I64Ne => out.push(0x52),
        Instruction::I64LtS => out.push(0x53),
        Instruction::I64LtU => out.push(0x54),
        Instruction::I64GtS => out.push(0x55),
        Instruction::I64GtU => out.push(0x56),
        Instruction::I64LeS => out.push(0x57),
        Instruction::I64LeU => out.push(0x58),
        Instruction::I64GeS => out.push(0x59),
        Instruction::I64GeU => out.push(0x5a),
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
        Instruction::If(bt) => {
            out.push(0x04);
            encode_block_type(out, bt);
        }
        Instruction::Else => {
            out.push(0x05);
        }
        Instruction::End => {
            out.push(0x0b);
        }
        Instruction::Block(bt) => {
            out.push(0x02);
            encode_block_type(out, bt);
        }
        Instruction::Loop(bt) => {
            out.push(0x03);
            encode_block_type(out, bt);
        }
        Instruction::Br(label) => {
            out.push(0x0c);
            write_uleb128(out, *label);
        }
        Instruction::BrIf(label) => {
            out.push(0x0d);
            write_uleb128(out, *label);
        }
        Instruction::Return => {
            // Wasm core: `return` is opcode 0x0f, no immediates.
            out.push(0x0f);
        }
    }
}

fn encode_block_type(out: &mut Vec<u8>, bt: &BlockType) {
    match bt {
        BlockType::Empty => out.push(0x40),
        BlockType::Single(vt) => out.push(val_type_byte(vt)),
        BlockType::TypeIdx(i) => write_sleb128_64(out, *i as i64),
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
