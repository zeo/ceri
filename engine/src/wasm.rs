use std::collections::BTreeMap;

const MAX_WASM_BYTES: usize = 16 * 1024 * 1024;
const MAX_WASM_VECTOR: u32 = 100_000;
const MAX_WASM_FUNCTIONS: usize = 10_000;
const MAX_WASM_LOCALS: usize = 100_000;
const MAX_WASM_CALL_DEPTH: usize = 256;
const MAX_WASM_STACK: usize = 1_000_000;

#[derive(Clone)]
pub(crate) struct Module {
    types: Vec<FuncType>,
    functions: Vec<Function>,
    exports: BTreeMap<String, u32>,
}

#[derive(Clone)]
struct FuncType {
    params: Vec<ValueType>,
    results: Vec<ValueType>,
}

#[derive(Clone)]
struct Function {
    type_index: u32,
    locals: Vec<ValueType>,
    code: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueType {
    I32,
}

impl Module {
    pub(crate) fn exported_functions(&self) -> impl Iterator<Item = (&str, u32)> {
        self.exports
            .iter()
            .map(|(name, index)| (name.as_str(), *index))
    }

    pub(crate) fn instantiate(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_WASM_BYTES {
            return Err("WebAssembly module exceeds the 16 MiB limit".to_string());
        }
        let mut reader = Reader::new(bytes);
        if reader.read_bytes::<4>()? != *b"\0asm" {
            return Err("WebAssembly module has an invalid header".to_string());
        }
        if reader.read_bytes::<4>()? != [1, 0, 0, 0] {
            return Err("only WebAssembly version 1 is supported".to_string());
        }

        let mut types = Vec::new();
        let mut function_types = Vec::new();
        let mut exports = BTreeMap::new();
        let mut functions = Vec::new();
        let mut last_section_id = 0;
        while !reader.is_empty() {
            let section_id = reader.read_u8()?;
            let section_length = reader.read_u32()? as usize;
            let mut section = reader.take(section_length)?;
            if section_id != 0 {
                if section_id <= last_section_id {
                    return Err(format!(
                        "WebAssembly section {section_id} is duplicated or out of order"
                    ));
                }
                last_section_id = section_id;
            }
            match section_id {
                0 => {}
                1 => parse_type_section(&mut section, &mut types)?,
                2 => return Err("WebAssembly imports are not supported in this slice".to_string()),
                3 => parse_function_section(&mut section, &mut function_types)?,
                7 => parse_export_section(&mut section, &mut exports)?,
                10 => parse_code_section(&mut section, &mut functions)?,
                _ => {
                    return Err(format!(
                        "WebAssembly section {section_id} is outside this slice"
                    ));
                }
            }
            if section_id != 0 && !section.is_empty() {
                return Err(format!(
                    "WebAssembly section {section_id} has trailing bytes"
                ));
            }
        }

        if function_types.len() != functions.len() {
            return Err(
                "WebAssembly function and code sections have different lengths".to_string(),
            );
        }
        if functions.len() > MAX_WASM_FUNCTIONS {
            return Err("WebAssembly module has too many functions".to_string());
        }
        for (function, type_index) in functions.iter_mut().zip(function_types) {
            if usize::try_from(type_index)
                .ok()
                .and_then(|index| types.get(index))
                .is_none()
            {
                return Err("WebAssembly function refers to a missing type".to_string());
            }
            function.type_index = type_index;
        }
        for index in exports.values() {
            if usize::try_from(*index)
                .ok()
                .and_then(|function| functions.get(function))
                .is_none()
            {
                return Err("WebAssembly export refers to a missing function".to_string());
            }
        }
        Ok(Self {
            types,
            functions,
            exports,
        })
    }

    pub(crate) fn invoke(
        &self,
        function_index: u32,
        arguments: &[i32],
    ) -> Result<Option<i32>, String> {
        self.execute(function_index, arguments, 0)
    }

    fn execute(
        &self,
        function_index: u32,
        arguments: &[i32],
        depth: usize,
    ) -> Result<Option<i32>, String> {
        if depth > MAX_WASM_CALL_DEPTH {
            return Err("WebAssembly call stack limit exceeded".to_string());
        }
        let function = self
            .functions
            .get(function_index as usize)
            .ok_or_else(|| "WebAssembly call refers to a missing function".to_string())?;
        let function_type = self
            .types
            .get(function.type_index as usize)
            .ok_or_else(|| "WebAssembly function refers to a missing type".to_string())?;
        if arguments.len() != function_type.params.len() {
            return Err(format!(
                "WebAssembly function expected {} arguments, received {}",
                function_type.params.len(),
                arguments.len()
            ));
        }
        let mut locals = arguments.to_vec();
        locals.extend(std::iter::repeat_n(0, function.locals.len()));
        let mut stack = Vec::new();
        let mut cursor = 0;
        let mut returned = false;
        while cursor < function.code.len() {
            let opcode = function.code[cursor];
            cursor += 1;
            match opcode {
                0x00 => return Err("WebAssembly unreachable instruction trapped".to_string()),
                0x01 => {}
                0x05 => {
                    return Err("WebAssembly else instruction is outside this slice".to_string());
                }
                0x0b | 0x0f => {
                    returned = true;
                    break;
                }
                0x10 => {
                    let target = read_code_u32(&function.code, &mut cursor)?;
                    let target_type = self.function_type(target).ok_or_else(|| {
                        "WebAssembly call refers to a missing function".to_string()
                    })?;
                    let argument_count = target_type.params.len();
                    if stack.len() < argument_count {
                        return Err("WebAssembly stack underflow at call".to_string());
                    }
                    let call_arguments = stack.split_off(stack.len() - argument_count);
                    let result = self.execute(target, &call_arguments, depth + 1)?;
                    if let Some(result) = result {
                        push_stack(&mut stack, result)?;
                    }
                }
                0x1a => {
                    pop_stack(&mut stack)?;
                }
                0x20 => {
                    let index = read_code_u32(&function.code, &mut cursor)? as usize;
                    push_stack(
                        &mut stack,
                        *locals.get(index).ok_or_else(|| {
                            "WebAssembly local.get index is out of range".to_string()
                        })?,
                    )?;
                }
                0x21 => {
                    let index = read_code_u32(&function.code, &mut cursor)? as usize;
                    let value = pop_stack(&mut stack)?;
                    *locals.get_mut(index).ok_or_else(|| {
                        "WebAssembly local.set index is out of range".to_string()
                    })? = value;
                }
                0x22 => {
                    let index = read_code_u32(&function.code, &mut cursor)? as usize;
                    let value = *stack
                        .last()
                        .ok_or_else(|| "WebAssembly stack underflow at local.tee".to_string())?;
                    *locals.get_mut(index).ok_or_else(|| {
                        "WebAssembly local.tee index is out of range".to_string()
                    })? = value;
                }
                0x41 => push_stack(&mut stack, read_code_i32(&function.code, &mut cursor)?)?,
                0x45 => {
                    let value = pop_stack(&mut stack)?;
                    push_stack(&mut stack, i32::from(value == 0))?;
                }
                0x46 => {
                    let right = pop_stack(&mut stack)?;
                    let left = pop_stack(&mut stack)?;
                    push_stack(&mut stack, i32::from(left == right))?;
                }
                0x47 => {
                    let right = pop_stack(&mut stack)?;
                    let left = pop_stack(&mut stack)?;
                    push_stack(&mut stack, i32::from(left != right))?;
                }
                0x48 => push_comparison(&mut stack, |left, right| left < right)?,
                0x4a => push_comparison(&mut stack, |left, right| left > right)?,
                0x4c => push_comparison(&mut stack, |left, right| left <= right)?,
                0x4e => push_comparison(&mut stack, |left, right| left >= right)?,
                0x6a => push_binary(&mut stack, i32::wrapping_add)?,
                0x6b => push_binary(&mut stack, i32::wrapping_sub)?,
                0x6c => push_binary(&mut stack, i32::wrapping_mul)?,
                0x6d => {
                    let right = pop_stack(&mut stack)?;
                    let left = pop_stack(&mut stack)?;
                    if right == 0 {
                        return Err("WebAssembly i32.div_s divided by zero".to_string());
                    }
                    if left == i32::MIN && right == -1 {
                        return Err("WebAssembly i32.div_s overflowed".to_string());
                    }
                    push_stack(&mut stack, left / right)?;
                }
                0x71 => push_binary(&mut stack, |left, right| left & right)?,
                0x72 => push_binary(&mut stack, |left, right| left | right)?,
                0x73 => push_binary(&mut stack, |left, right| left ^ right)?,
                0x74 => push_binary(&mut stack, |left, right| {
                    left.wrapping_shl((right as u32) & 31)
                })?,
                0x75 => push_binary(&mut stack, |left, right| {
                    left.wrapping_shr((right as u32) & 31)
                })?,
                0x76 => push_binary(&mut stack, |left, right| {
                    ((left as u32).wrapping_shr((right as u32) & 31)) as i32
                })?,
                _ => {
                    return Err(format!(
                        "WebAssembly opcode 0x{opcode:02x} is outside this slice"
                    ));
                }
            }
        }
        if !returned && function.code.last() != Some(&0x0b) {
            return Err("WebAssembly function did not end".to_string());
        }
        match function_type.results.as_slice() {
            [] if stack.is_empty() => Ok(None),
            [] => Err("WebAssembly function left values on the stack".to_string()),
            [_] => {
                let result = pop_stack(&mut stack)?;
                if stack.is_empty() {
                    Ok(Some(result))
                } else {
                    Err("WebAssembly function left values on the stack".to_string())
                }
            }
            _ => Err("WebAssembly functions with multiple results are not supported".to_string()),
        }
    }

    fn function_type(&self, function_index: u32) -> Option<&FuncType> {
        self.functions
            .get(function_index as usize)
            .and_then(|function| self.types.get(function.type_index as usize))
    }
}

fn parse_type_section(reader: &mut Reader<'_>, types: &mut Vec<FuncType>) -> Result<(), String> {
    let count = reader.read_vector_length()?;
    for _ in 0..count {
        if reader.read_u8()? != 0x60 {
            return Err("WebAssembly type is not a function type".to_string());
        }
        let params = read_value_types(reader)?;
        let results = read_value_types(reader)?;
        if results.len() > 1 {
            return Err(
                "WebAssembly functions with multiple results are not supported".to_string(),
            );
        }
        types.push(FuncType { params, results });
    }
    Ok(())
}

fn parse_function_section(
    reader: &mut Reader<'_>,
    function_types: &mut Vec<u32>,
) -> Result<(), String> {
    let count = reader.read_vector_length()?;
    if count as usize > MAX_WASM_FUNCTIONS {
        return Err("WebAssembly module has too many functions".to_string());
    }
    function_types.reserve(count as usize);
    for _ in 0..count {
        function_types.push(reader.read_u32()?);
    }
    Ok(())
}

fn parse_export_section(
    reader: &mut Reader<'_>,
    exports: &mut BTreeMap<String, u32>,
) -> Result<(), String> {
    let count = reader.read_vector_length()?;
    for _ in 0..count {
        let name = reader.read_name()?;
        if reader.read_u8()? != 0 {
            return Err("only WebAssembly function exports are supported".to_string());
        }
        let index = reader.read_u32()?;
        if exports.insert(name.clone(), index).is_some() {
            return Err(format!("WebAssembly export {name} is duplicated"));
        }
    }
    Ok(())
}

fn parse_code_section(
    reader: &mut Reader<'_>,
    functions: &mut Vec<Function>,
) -> Result<(), String> {
    let count = reader.read_vector_length()?;
    if count as usize > MAX_WASM_FUNCTIONS {
        return Err("WebAssembly module has too many functions".to_string());
    }
    functions.reserve(count as usize);
    for _ in 0..count {
        let body_size = reader.read_u32()? as usize;
        let mut body = reader.take(body_size)?;
        let declaration_count = body.read_vector_length()?;
        let mut locals = Vec::new();
        for _ in 0..declaration_count {
            let count = body.read_u32()? as usize;
            let value_type = read_value_type(&mut body)?;
            if locals.len().saturating_add(count) > MAX_WASM_LOCALS {
                return Err("WebAssembly function has too many locals".to_string());
            }
            locals.extend(std::iter::repeat_n(value_type, count));
        }
        let code = body.remaining().to_vec();
        if code.last() != Some(&0x0b) {
            return Err("WebAssembly function body is missing its end instruction".to_string());
        }
        functions.push(Function {
            type_index: 0,
            locals,
            code,
        });
    }
    Ok(())
}

fn read_value_types(reader: &mut Reader<'_>) -> Result<Vec<ValueType>, String> {
    let count = reader.read_vector_length()?;
    (0..count).map(|_| read_value_type(reader)).collect()
}

fn read_value_type(reader: &mut Reader<'_>) -> Result<ValueType, String> {
    match reader.read_u8()? {
        0x7f => Ok(ValueType::I32),
        other => Err(format!(
            "WebAssembly value type 0x{other:02x} is outside this slice"
        )),
    }
}

fn push_binary(
    stack: &mut Vec<i32>,
    operation: impl FnOnce(i32, i32) -> i32,
) -> Result<(), String> {
    let right = pop_stack(stack)?;
    let left = pop_stack(stack)?;
    push_stack(stack, operation(left, right))
}

fn push_comparison(
    stack: &mut Vec<i32>,
    operation: impl FnOnce(i32, i32) -> bool,
) -> Result<(), String> {
    let right = pop_stack(stack)?;
    let left = pop_stack(stack)?;
    push_stack(stack, i32::from(operation(left, right)))
}

fn push_stack(stack: &mut Vec<i32>, value: i32) -> Result<(), String> {
    if stack.len() >= MAX_WASM_STACK {
        return Err("WebAssembly operand stack limit exceeded".to_string());
    }
    stack.push(value);
    Ok(())
}

fn pop_stack(stack: &mut Vec<i32>) -> Result<i32, String> {
    stack
        .pop()
        .ok_or_else(|| "WebAssembly stack underflow".to_string())
}

fn read_code_u32(code: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let mut reader = Reader::new(code.get(*cursor..).unwrap_or_default());
    let value = reader.read_u32()?;
    *cursor = cursor.saturating_add(reader.offset);
    Ok(value)
}

fn read_code_i32(code: &[u8], cursor: &mut usize) -> Result<i32, String> {
    let mut reader = Reader::new(code.get(*cursor..).unwrap_or_default());
    let value = reader.read_i32()?;
    *cursor = cursor.saturating_add(reader.offset);
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| "WebAssembly binary ended unexpectedly".to_string())?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_bytes<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let bytes = self.take(N)?;
        bytes
            .remaining()
            .try_into()
            .map_err(|_| "WebAssembly binary ended unexpectedly".to_string())
    }

    fn take(&mut self, length: usize) -> Result<Reader<'a>, String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "WebAssembly section length overflowed".to_string())?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "WebAssembly section exceeds the module".to_string())?;
        self.offset = end;
        Ok(Reader::new(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.read_u8()?;
            let bits = u32::from(byte & 0x7f);
            if shift == 28 && bits > 0x0f {
                return Err("WebAssembly unsigned integer is too large".to_string());
            }
            value |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("WebAssembly unsigned integer is too long".to_string())
    }

    fn read_i32(&mut self) -> Result<i32, String> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.read_u8()?;
            let payload = u32::from(byte & 0x7f);
            if shift == 28 {
                let sign_bit = byte & 0x40 != 0;
                if (!sign_bit && payload > 0x07) || (sign_bit && payload < 0x78) {
                    return Err("WebAssembly signed integer is too large".to_string());
                }
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                if shift < 28 && byte & 0x40 != 0 {
                    value |= (!0u32) << (shift + 7);
                }
                return Ok(value as i32);
            }
        }
        Err("WebAssembly signed integer is too long".to_string())
    }

    fn read_vector_length(&mut self) -> Result<u32, String> {
        let length = self.read_u32()?;
        if length > MAX_WASM_VECTOR {
            return Err("WebAssembly vector is too large".to_string());
        }
        Ok(length)
    }

    fn read_name(&mut self) -> Result<String, String> {
        let length = usize::try_from(self.read_u32()?)
            .map_err(|_| "WebAssembly name is too long".to_string())?;
        let bytes = self.take(length)?.remaining().to_vec();
        String::from_utf8(bytes).map_err(|_| "WebAssembly export name is not UTF-8".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Module;

    const ADD_MODULE: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f,
        0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00,
        0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
    ];

    #[test]
    fn parses_and_invokes_an_i32_export() {
        let module = Module::instantiate(ADD_MODULE).expect("module should parse");
        assert_eq!(
            module.exported_functions().collect::<Vec<_>>(),
            vec![("add", 0)]
        );
        assert_eq!(
            module.invoke(0, &[2, 3]).expect("function should run"),
            Some(5)
        );
    }

    #[test]
    fn rejects_imports_before_execution() {
        let module = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x02, 0x01, 0x00,
        ];
        let error = match Module::instantiate(&module) {
            Ok(_) => panic!("imports must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error, "WebAssembly imports are not supported in this slice");
    }

    #[test]
    fn rejects_stack_values_left_after_a_void_function() {
        let module = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x01, 0x0b,
        ];
        let module = Module::instantiate(&module).expect("module should parse");
        assert_eq!(
            module
                .invoke(0, &[])
                .expect_err("stack leftovers must be rejected"),
            "WebAssembly function left values on the stack"
        );
    }
}
