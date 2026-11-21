// Uint8Array to and from base64 and hex: toBase64, toHex, fromBase64, fromHex, setFromBase64
// and setFromHex, decoding as the spec does so partial input reports how far it read

use super::operations::{is_primitive, plain_object};
use super::typed::{ElementKind, new_typed};
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum TextMethod {
    ToBase64,
    ToHex,
    FromBase64,
    FromHex,
    SetFromBase64,
    SetFromHex,
}

#[derive(Clone, Copy, PartialEq)]
enum LastChunk {
    Loose,
    Strict,
    StopBeforePartial,
}

// how far the input was read, the bytes decoded, and whether decoding failed
struct Decoded {
    read: usize,
    bytes: Vec<u8>,
    failed: bool,
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn skip_whitespace(input: &[u8], mut index: usize) -> usize {
    while index < input.len() && matches!(input[index], b'\t' | b'\n' | b'\x0c' | b'\r' | b' ') {
        index += 1;
    }
    index
}

// the bytes of a chunk of two to four sextets, none when strict decoding finds bits left over
fn decode_chunk(chunk: &[u8], strict: bool) -> Option<Vec<u8>> {
    let word = (0..4).fold(0_u32, |word, index| {
        word << 6 | u32::from(chunk.get(index).copied().unwrap_or(0))
    });
    let bytes = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
    let kept = chunk.len() - 1;
    if strict && bytes[kept..].iter().any(|&byte| byte != 0) {
        return None;
    }
    Some(bytes[..kept].to_vec())
}

fn from_base64(input: &[u8], url: bool, last: LastChunk, max_length: usize) -> Decoded {
    let mut decoded = Decoded {
        read: 0,
        bytes: Vec::new(),
        failed: false,
    };
    if max_length == 0 {
        return decoded;
    }
    let mut chunk = Vec::with_capacity(4);
    let mut index = 0;
    let fail = |mut decoded: Decoded| {
        decoded.failed = true;
        decoded
    };
    loop {
        index = skip_whitespace(input, index);
        if index == input.len() {
            if !chunk.is_empty() {
                match last {
                    LastChunk::StopBeforePartial => return decoded,
                    LastChunk::Loose if chunk.len() > 1 => {
                        decoded
                            .bytes
                            .extend(decode_chunk(&chunk, false).unwrap_or_default());
                    }
                    _ => return fail(decoded),
                }
            }
            decoded.read = input.len();
            return decoded;
        }
        let mut byte = input[index];
        index += 1;
        if byte == b'=' {
            if chunk.len() < 2 {
                return fail(decoded);
            }
            index = skip_whitespace(input, index);
            if chunk.len() == 2 {
                if index == input.len() {
                    return if last == LastChunk::StopBeforePartial {
                        decoded
                    } else {
                        fail(decoded)
                    };
                }
                if input[index] == b'=' {
                    index = skip_whitespace(input, index + 1);
                }
            }
            if index < input.len() {
                return fail(decoded);
            }
            let Some(bytes) = decode_chunk(&chunk, last == LastChunk::Strict) else {
                return fail(decoded);
            };
            decoded.bytes.extend(bytes);
            decoded.read = input.len();
            return decoded;
        }
        if url {
            byte = match byte {
                b'+' | b'/' => return fail(decoded),
                b'-' => b'+',
                b'_' => b'/',
                other => other,
            };
        }
        let Some(sextet) = ALPHABET.iter().position(|&letter| letter == byte) else {
            return fail(decoded);
        };
        let remaining = max_length - decoded.bytes.len();
        if (remaining == 1 && chunk.len() == 2) || (remaining == 2 && chunk.len() == 3) {
            return decoded;
        }
        chunk.push(sextet as u8);
        if chunk.len() == 4 {
            decoded
                .bytes
                .extend(decode_chunk(&chunk, false).unwrap_or_default());
            chunk.clear();
            decoded.read = index;
            if decoded.bytes.len() == max_length {
                return decoded;
            }
        }
    }
}

fn from_hex(input: &[u8], max_length: usize) -> Decoded {
    let mut decoded = Decoded {
        read: 0,
        bytes: Vec::new(),
        failed: !input.len().is_multiple_of(2),
    };
    while !decoded.failed && decoded.read < input.len() && decoded.bytes.len() < max_length {
        let pair = &input[decoded.read..decoded.read + 2];
        match std::str::from_utf8(pair)
            .ok()
            .filter(|pair| pair.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        {
            Some(byte) => {
                decoded.bytes.push(byte);
                decoded.read += 2;
            }
            None => decoded.failed = true,
        }
    }
    decoded
}

fn to_base64(bytes: &[u8], url: bool, omit_padding: bool) -> String {
    let mut text = super::canvas::base64(bytes);
    if omit_padding {
        text.truncate(text.trim_end_matches('=').len());
    }
    if url {
        text = text.replace('+', "-").replace('/', "_");
    }
    text
}

impl ScriptRuntime {
    fn this_uint8(&mut self, this: &Value) -> Result<TypedArrayHandle, ScriptError> {
        match this {
            Value::TypedArray(array) if array.borrow().kind == ElementKind::Uint8 => {
                Ok(array.clone())
            }
            _ => Err(self.type_error("this is not a Uint8Array")),
        }
    }

    fn base64_options(
        &mut self,
        options: Value,
        with_last_chunk: bool,
    ) -> Result<(bool, LastChunk, bool), ScriptError> {
        if !matches!(options, Value::Undefined) && is_primitive(&options) {
            return Err(self.type_error("options must be an object"));
        }
        let read = |runtime: &mut Self, key: &str| -> Result<Value, ScriptError> {
            match &options {
                Value::Undefined => Ok(Value::Undefined),
                options => runtime.get_property(options, key),
            }
        };
        let url = match read(self, "alphabet")? {
            Value::Undefined => false,
            Value::String(text) if &*text == "base64" => false,
            Value::String(text) if &*text == "base64url" => true,
            _ => return Err(self.type_error("alphabet must be \"base64\" or \"base64url\"")),
        };
        if !with_last_chunk {
            let omit_padding = read(self, "omitPadding")?.truthy();
            return Ok((url, LastChunk::Loose, omit_padding));
        }
        let last = match read(self, "lastChunkHandling")? {
            Value::Undefined => LastChunk::Loose,
            Value::String(text) if &*text == "loose" => LastChunk::Loose,
            Value::String(text) if &*text == "strict" => LastChunk::Strict,
            Value::String(text) if &*text == "stop-before-partial" => LastChunk::StopBeforePartial,
            _ => {
                return Err(self.type_error(
                    "lastChunkHandling must be \"loose\", \"strict\" or \"stop-before-partial\"",
                ));
            }
        };
        Ok((url, last, false))
    }

    fn text_argument(&mut self, value: Value) -> Result<String, ScriptError> {
        match value.resolve() {
            Value::String(text) => Ok(text.to_string()),
            _ => Err(self.type_error("the argument must be a string")),
        }
    }

    pub(super) fn call_uint8_text(
        &mut self,
        method: TextMethod,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use TextMethod as M;
        let mut arguments = arguments.into_iter();
        let first = arguments.next().unwrap_or(Value::Undefined);
        let options = arguments.next().unwrap_or(Value::Undefined);
        match method {
            M::ToBase64 | M::ToHex => {
                let array = self.this_uint8(&this)?;
                let (url, _, omit_padding) = match method {
                    M::ToBase64 => self.base64_options(first, false)?,
                    _ => (false, LastChunk::Loose, false),
                };
                let array = array.borrow();
                let buffer = array.buffer.borrow();
                let bytes = buffer
                    .get(super::typed::byte_range(&array))
                    .unwrap_or_default();
                Ok(Value::String(
                    match method {
                        M::ToBase64 => to_base64(bytes, url, omit_padding),
                        _ => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                    }
                    .into(),
                ))
            }
            M::FromBase64 | M::FromHex => {
                let text = self.text_argument(first)?;
                let decoded = match method {
                    M::FromBase64 => {
                        let (url, last, _) = self.base64_options(options, true)?;
                        from_base64(text.as_bytes(), url, last, usize::MAX)
                    }
                    _ => from_hex(text.as_bytes(), usize::MAX),
                };
                if decoded.failed {
                    return Err(self.throw_error("SyntaxError", "the text is not valid"));
                }
                let length = decoded.bytes.len();
                if length > MAX_TYPED_ARRAY_BYTES {
                    return Err(self.range_error("Array buffer allocation failed"));
                }
                Ok(new_typed(
                    ElementKind::Uint8,
                    Rc::new(RefCell::new(decoded.bytes.into())),
                    0,
                    length,
                ))
            }
            M::SetFromBase64 | M::SetFromHex => {
                let array = self.this_uint8(&this)?;
                let text = self.text_argument(first)?;
                let decoding = match method {
                    M::SetFromBase64 => Some(self.base64_options(options, true)?),
                    _ => None,
                };
                let (start, room) = {
                    let array = array.borrow();
                    {
                        let range = super::typed::byte_range(&array);
                        (range.start, range.len())
                    }
                };
                let decoded = match decoding {
                    Some((url, last, _)) => from_base64(text.as_bytes(), url, last, room),
                    None => from_hex(text.as_bytes(), room),
                };
                {
                    let array = array.borrow();
                    let mut buffer = array.buffer.borrow_mut();
                    if let Some(target) = buffer.get_mut(start..start + decoded.bytes.len()) {
                        target.copy_from_slice(&decoded.bytes);
                    }
                }
                if decoded.failed {
                    return Err(self.throw_error("SyntaxError", "the text is not valid"));
                }
                Ok(plain_object([
                    ("read".to_string(), Value::Number(decoded.read as f64)),
                    (
                        "written".to_string(),
                        Value::Number(decoded.bytes.len() as f64),
                    ),
                ]))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(text: &str, last: LastChunk, max_length: usize) -> (usize, Vec<u8>, bool) {
        let decoded = from_base64(text.as_bytes(), false, last, max_length);
        (decoded.read, decoded.bytes, decoded.failed)
    }

    #[test]
    fn base64_decodes_by_the_last_chunk_rule() {
        let all = usize::MAX;
        assert_eq!(
            decode("aGVsbG8=", LastChunk::Strict, all),
            (8, b"hello".to_vec(), false)
        );
        assert_eq!(
            decode(" aGVs\nbG8 ", LastChunk::Loose, all),
            (10, b"hello".to_vec(), false)
        );
        assert_eq!(
            decode("aGVsbG8", LastChunk::Loose, all),
            (7, b"hello".to_vec(), false)
        );
        assert!(decode("aGVsbG8", LastChunk::Strict, all).2);
        assert_eq!(
            decode("aGVsbG8", LastChunk::StopBeforePartial, all),
            (4, b"hel".to_vec(), false)
        );
        assert!(decode("aGVsbG9=", LastChunk::Strict, all).2, "extra bits");
        assert_eq!(
            decode("aGVsbG9=", LastChunk::Loose, all),
            (8, b"hello".to_vec(), false)
        );
        assert!(decode("aGVsbG8=x", LastChunk::Loose, all).2);
        assert!(decode("a", LastChunk::Loose, all).2);
        assert_eq!(
            decode("aGVsbG8=", LastChunk::Loose, 4),
            (4, b"hel".to_vec(), false)
        );
        assert_eq!(decode("!!!!", LastChunk::Loose, 0), (0, Vec::new(), false));
        let url = from_base64(b"-_8", true, LastChunk::Loose, usize::MAX);
        assert_eq!(url.bytes, vec![0xfb, 0xff]);
        assert!(from_base64(b"+/8", true, LastChunk::Loose, usize::MAX).failed);
    }

    #[test]
    fn hex_and_encoding_round_trip() {
        let decoded = from_hex(b"00ffA0", usize::MAX);
        assert_eq!(
            (decoded.read, decoded.bytes, decoded.failed),
            (6, vec![0, 255, 160], false)
        );
        assert!(from_hex(b"abc", usize::MAX).failed);
        let capped = from_hex(b"0102zz", 2);
        assert_eq!((capped.read, capped.failed), (4, false));
        assert!(from_hex(b"01zz", usize::MAX).failed);
        assert_eq!(to_base64(&[0xfb, 0xff], true, true), "-_8");
        assert_eq!(to_base64(b"hi", false, false), "aGk=");
    }

    #[test]
    fn uint8_arrays_speak_base64_and_hex_from_script() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                seen.push(Uint8Array.fromBase64("aGVsbG8").join());
                seen.push(new Uint8Array([104, 105]).toBase64(), new Uint8Array([251, 255]).toBase64({ alphabet: "base64url", omitPadding: true }));
                seen.push(Uint8Array.fromHex("cafe").toHex());
                const target = new Uint8Array(4);
                const result = target.setFromBase64("aGVsbG8=");
                seen.push(result.read, result.written, target.join());
                const hex = new Uint8Array(2);
                try { hex.setFromHex("01zz"); } catch (error) { seen.push(error.name, hex.join()); }
                try { Uint8Array.fromBase64("aGk", { lastChunkHandling: "strict" }); } catch (error) { seen.push(error.name); }
                try { new Int8Array(1).toHex(); } catch (error) { seen.push(error.name); }
                try { Uint8Array.fromBase64(1); } catch (error) { seen.push(error.name); }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "104,101,108,108,111|aGk=|-_8|cafe|4|3|104,101,108,0|SyntaxError|1,0|SyntaxError|TypeError|TypeError"
            )
        );
    }
}
