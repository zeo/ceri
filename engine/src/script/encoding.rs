// TextEncoder and TextDecoder for UTF-8, UTF-16 and windows-1252 (which the Encoding
// Standard also uses for the latin1 and ascii labels), with fatal mode, BOM handling and
// streaming decodes that carry incomplete sequences between calls

use super::object::{JsObject, ObjectClass, new_object};
use super::typed::{ElementKind, new_typed};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

impl Encoding {
    fn from_label(label: &str) -> Option<Self> {
        match label.trim().to_ascii_lowercase().as_str() {
            "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8"
            | "x-unicode20utf8" => Some(Encoding::Utf8),
            "utf-16le" | "utf-16" | "ucs-2" | "unicode" | "csunicode" | "iso-10646-ucs-2"
            | "unicodefeff" => Some(Encoding::Utf16Le),
            "utf-16be" | "unicodefffe" => Some(Encoding::Utf16Be),
            "windows-1252" | "latin1" | "iso-8859-1" | "iso8859-1" | "l1" | "ascii"
            | "us-ascii" | "cp1252" | "x-cp1252" | "cp819" | "ibm819" | "iso_8859-1"
            | "iso-ir-100" | "csisolatin1" | "ansi_x3.4-1968" => Some(Encoding::Windows1252),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Windows1252 => "windows-1252",
        }
    }
}

// the windows-1252 code points for bytes 0x80 to 0x9f; the rest map to themselves
const WINDOWS_1252: [u16; 32] = [
    0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039,
    0x152, 0x8d, 0x17d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x2dc,
    0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178,
];

pub(super) struct DecoderState {
    encoding: Encoding,
    fatal: bool,
    ignore_bom: bool,
    // bytes of an incomplete sequence from a streaming call
    pending: Vec<u8>,
    // whether the start of the stream (where a BOM may appear) has been passed
    started: bool,
}

// decodes as much as forms complete characters; the rest stays pending when streaming
fn decode_utf8(bytes: &[u8], fatal: bool, stream: bool) -> Result<(String, Vec<u8>), ()> {
    let mut output = String::with_capacity(bytes.len());
    let mut position = 0;
    while position < bytes.len() {
        match std::str::from_utf8(&bytes[position..]) {
            Ok(text) => {
                output.push_str(text);
                return Ok((output, Vec::new()));
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.push_str(
                    std::str::from_utf8(&bytes[position..position + valid])
                        .expect("validated prefix"),
                );
                position += valid;
                match error.error_len() {
                    Some(length) => {
                        if fatal {
                            return Err(());
                        }
                        output.push('\u{fffd}');
                        position += length;
                    }
                    None => {
                        // an incomplete sequence at the end
                        if stream {
                            return Ok((output, bytes[position..].to_vec()));
                        }
                        if fatal {
                            return Err(());
                        }
                        output.push('\u{fffd}');
                        position = bytes.len();
                    }
                }
            }
        }
    }
    Ok((output, Vec::new()))
}

fn decode_utf16(
    bytes: &[u8],
    big_endian: bool,
    fatal: bool,
    stream: bool,
) -> Result<(String, Vec<u8>), ()> {
    let whole = bytes.len() / 2 * 2;
    let mut units: Vec<u16> = bytes[..whole]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    let mut pending = bytes[whole..].to_vec();
    // a trailing high surrogate waits for its pair when streaming
    if stream
        && units
            .last()
            .is_some_and(|unit| (0xd800..0xdc00).contains(unit))
    {
        let unit = units.pop().expect("checked above");
        let mut carried = if big_endian {
            unit.to_be_bytes().to_vec()
        } else {
            unit.to_le_bytes().to_vec()
        };
        carried.extend(pending);
        pending = carried;
    }
    let mut output = String::with_capacity(units.len());
    for decoded in char::decode_utf16(units) {
        match decoded {
            Ok(character) => output.push(character),
            Err(_) if fatal => return Err(()),
            Err(_) => output.push('\u{fffd}'),
        }
    }
    if !stream && !pending.is_empty() {
        if fatal {
            return Err(());
        }
        output.push('\u{fffd}');
        pending.clear();
    }
    Ok((output, pending))
}

fn decode_windows_1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| match byte {
            0x80..=0x9f => char::from_u32(u32::from(WINDOWS_1252[usize::from(byte - 0x80)]))
                .unwrap_or('\u{fffd}'),
            byte => char::from(*byte),
        })
        .collect()
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

#[derive(Clone, Copy, Debug)]
pub(super) enum CodecMethod {
    EncoderConstructor,
    Encode,
    EncodeInto,
    EncoderEncoding,
    DecoderConstructor,
    Decode,
    DecoderEncoding,
    DecoderFatal,
    DecoderIgnoreBom,
}

impl ScriptRuntime {
    // the bytes of an ArrayBuffer or any view over one
    pub(super) fn buffer_source_bytes(&mut self, value: &Value) -> Result<Vec<u8>, ScriptError> {
        match value {
            Value::Undefined => Ok(Vec::new()),
            Value::Bytes(bytes) => Ok(bytes.borrow().to_vec()),
            Value::TypedArray(array) => {
                let array = array.borrow();
                Ok(array.buffer.borrow()[super::typed::byte_range(&array)].to_vec())
            }
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::DataView {
                    buffer,
                    start,
                    end,
                    tracking,
                } => {
                    let buffer = buffer.borrow();
                    let range = super::typed::view_range(&buffer, *start, *end, *tracking, 1);
                    Ok(range.map_or(Vec::new(), |(start, end)| buffer[start..end].to_vec()))
                }
                _ => Err(runtime_error(
                    "The provided value is not of type '(ArrayBuffer or ArrayBufferView)'",
                )),
            }
            .map_err(|error| self.type_error(error.message)),
            _ => Err(self.type_error(
                "The provided value is not of type '(ArrayBuffer or ArrayBufferView)'",
            )),
        }
    }

    fn decoder_state(&mut self, this: &Value) -> Result<Rc<RefCell<DecoderState>>, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::TextDecoder(state) = &object.borrow().class
        {
            return Ok(state.clone());
        }
        Err(self.type_error("Illegal invocation"))
    }

    pub(super) fn call_codec(
        &mut self,
        method: CodecMethod,
        this: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        use CodecMethod as M;
        let first = argument(&arguments, 0);
        match method {
            M::EncoderConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor TextEncoder requires 'new'"));
                }
                let prototype = self.intrinsics.text_encoder_prototype.clone();
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::Ordinary,
                ))))
            }
            M::EncoderEncoding => Ok(Value::String("utf-8".into())),
            M::Encode => {
                let text = match first {
                    Value::Undefined => String::new(),
                    other => self.coerce_string(other)?,
                };
                let bytes = text.into_bytes();
                let length = bytes.len();
                Ok(new_typed(
                    ElementKind::Uint8,
                    Rc::new(RefCell::new(bytes.into())),
                    0,
                    length,
                ))
            }
            M::EncodeInto => {
                let text = self.coerce_string(first)?;
                let Value::TypedArray(destination) = argument(&arguments, 1) else {
                    return Err(self.type_error("The destination must be a Uint8Array"));
                };
                let (buffer, start, end) = {
                    let destination = destination.borrow();
                    (
                        destination.buffer.clone(),
                        destination.start,
                        destination.end,
                    )
                };
                let capacity = end - start;
                let mut written = 0;
                let mut read = 0;
                for character in text.chars() {
                    let length = character.len_utf8();
                    if written + length > capacity {
                        break;
                    }
                    let mut encoded = [0u8; 4];
                    character.encode_utf8(&mut encoded);
                    buffer.borrow_mut()[start + written..start + written + length]
                        .copy_from_slice(&encoded[..length]);
                    written += length;
                    read += character.len_utf16();
                }
                Ok(self.object_from_entries(vec![
                    ("read".to_string(), Value::Number(read as f64)),
                    ("written".to_string(), Value::Number(written as f64)),
                ]))
            }
            M::DecoderConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor TextDecoder requires 'new'"));
                }
                let label = match first {
                    Value::Undefined => "utf-8".to_string(),
                    other => self.coerce_string(other)?,
                };
                let Some(encoding) = Encoding::from_label(&label) else {
                    return Err(self.range_error(format!(
                        "The encoding label provided ('{label}') is invalid."
                    )));
                };
                let options = argument(&arguments, 1);
                let (fatal, ignore_bom) = if super::operations::is_primitive(&options) {
                    (false, false)
                } else {
                    (
                        self.get_property(&options, "fatal")?.truthy(),
                        self.get_property(&options, "ignoreBOM")?.truthy(),
                    )
                };
                let prototype = self.intrinsics.text_decoder_prototype.clone();
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::TextDecoder(Rc::new(RefCell::new(DecoderState {
                        encoding,
                        fatal,
                        ignore_bom,
                        pending: Vec::new(),
                        started: false,
                    }))),
                ))))
            }
            M::DecoderEncoding => Ok(Value::String(
                self.decoder_state(&this)?.borrow().encoding.name().into(),
            )),
            M::DecoderFatal => Ok(Value::Bool(self.decoder_state(&this)?.borrow().fatal)),
            M::DecoderIgnoreBom => Ok(Value::Bool(self.decoder_state(&this)?.borrow().ignore_bom)),
            M::Decode => {
                let state = self.decoder_state(&this)?;
                let options = argument(&arguments, 1);
                let stream = !super::operations::is_primitive(&options)
                    && self.get_property(&options, "stream")?.truthy();
                let input = self.buffer_source_bytes(&first)?;
                let mut state = state.borrow_mut();
                let mut bytes = std::mem::take(&mut state.pending);
                bytes.extend(input);
                // the byte order mark is consumed once, at the start of the stream
                if !state.started && !state.ignore_bom {
                    let mark: &[u8] = match state.encoding {
                        Encoding::Utf8 => &[0xef, 0xbb, 0xbf],
                        Encoding::Utf16Le => &[0xff, 0xfe],
                        Encoding::Utf16Be => &[0xfe, 0xff],
                        Encoding::Windows1252 => &[],
                    };
                    if !mark.is_empty()
                        && bytes.len() < mark.len()
                        && mark.starts_with(&bytes)
                        && stream
                    {
                        state.pending = bytes;
                        return Ok(Value::String("".into()));
                    }
                    if !mark.is_empty() && bytes.starts_with(mark) {
                        bytes.drain(..mark.len());
                    }
                }
                state.started = !bytes.is_empty() || state.started;
                let decoded = match state.encoding {
                    Encoding::Utf8 => decode_utf8(&bytes, state.fatal, stream),
                    Encoding::Utf16Le => decode_utf16(&bytes, false, state.fatal, stream),
                    Encoding::Utf16Be => decode_utf16(&bytes, true, state.fatal, stream),
                    Encoding::Windows1252 => Ok((decode_windows_1252(&bytes), Vec::new())),
                };
                match decoded {
                    Ok((text, pending)) => {
                        state.pending = pending;
                        if !stream {
                            state.started = false;
                        }
                        Ok(Value::String(text.into()))
                    }
                    Err(()) => {
                        state.pending.clear();
                        state.started = false;
                        drop(state);
                        Err(self.type_error("The encoded data was not valid."))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoders_handle_invalid_and_partial_input() {
        assert_eq!(
            decode_utf8(&[0x68, 0xc3, 0xa9], false, false),
            Ok(("hé".to_string(), Vec::new()))
        );
        assert_eq!(
            decode_utf8(&[0x68, 0xff, 0x69], false, false),
            Ok(("h\u{fffd}i".to_string(), Vec::new()))
        );
        assert_eq!(decode_utf8(&[0x68, 0xff], true, false), Err(()));
        assert_eq!(
            decode_utf8(&[0x68, 0xe2, 0x82], false, true),
            Ok(("h".to_string(), vec![0xe2, 0x82]))
        );
        assert_eq!(
            decode_utf16(&[0x68, 0, 0x69, 0], false, false, false),
            Ok(("hi".to_string(), Vec::new()))
        );
        assert_eq!(decode_windows_1252(&[0x80, 0x41, 0xe9]), "€Aé");
        assert_eq!(
            Encoding::from_label(" Latin1 "),
            Some(Encoding::Windows1252)
        );
        assert_eq!(Encoding::from_label("shift_jis"), None);
    }
}
