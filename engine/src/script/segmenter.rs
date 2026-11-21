// Intl.Segmenter over the segmentation in segment.rs: segment() hands back a Segments object
// that iterates the graphemes, words or sentences of a string and finds the one at an index

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::plain_object;
use super::realm::{Builder, Native};
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmenterMember {
    Constructor,
    Segment,
    ResolvedOptions,
    Containing,
    Iterate,
    Next,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Granularity {
    Grapheme,
    Word,
    Sentence,
}

impl Granularity {
    fn name(self) -> &'static str {
        match self {
            Granularity::Grapheme => "grapheme",
            Granularity::Word => "word",
            Granularity::Sentence => "sentence",
        }
    }
}

pub(super) struct Segments {
    input: String,
    granularity: Granularity,
    // start and end code unit offsets, and for words whether the piece is a word
    pieces: Vec<(usize, usize, bool)>,
    units: Vec<u16>,
}

pub(super) enum SegmenterState {
    Segmenter(String, Granularity),
    Segments(Rc<Segments>),
    Cursor(Rc<Segments>, Cell<usize>),
}

pub(super) struct SegmenterInterfaces {
    segmenter: ObjectHandle,
    segments: ObjectHandle,
    cursor: ObjectHandle,
}

fn native(member: SegmenterMember) -> Native {
    Native::Segmenter(member)
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    iterator_prototype: &ObjectHandle,
    intl: &ObjectHandle,
) -> SegmenterInterfaces {
    let plain = |prototype: &ObjectHandle| {
        new_object(JsObject::with_prototype(
            Some(prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let method = |target: &ObjectHandle, name: &str, member: SegmenterMember, length: u32| {
        let function = build.function(native(member), name, length);
        target
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    };
    let segmenter = plain(object_prototype);
    method(&segmenter, "segment", SegmenterMember::Segment, 1);
    method(
        &segmenter,
        "resolvedOptions",
        SegmenterMember::ResolvedOptions,
        0,
    );
    segmenter.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("Intl.Segmenter".into()))
        },
    );
    let segments = plain(object_prototype);
    method(&segments, "containing", SegmenterMember::Containing, 1);
    let iterate = build.function(native(SegmenterMember::Iterate), "[Symbol.iterator]", 0);
    segments.borrow_mut().define(
        "@@iterator".to_string(),
        Property::hidden(Value::Object(iterate)),
    );
    let cursor = plain(iterator_prototype);
    method(&cursor, "next", SegmenterMember::Next, 0);
    cursor.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("Segmenter String Iterator".into()))
        },
    );
    let constructor = build.constructor(
        native(SegmenterMember::Constructor),
        "Segmenter",
        0,
        &segmenter,
    );
    let supported = build.function(
        Native::Intl(Rc::new(super::intl::IntlMethod::SupportedLocalesOf)),
        "supportedLocalesOf",
        1,
    );
    constructor.borrow_mut().define(
        "supportedLocalesOf".to_string(),
        Property::hidden(Value::Object(supported)),
    );
    intl.borrow_mut().define(
        "Segmenter".to_string(),
        Property::hidden(Value::Object(constructor)),
    );
    SegmenterInterfaces {
        segmenter,
        segments,
        cursor,
    }
}

impl ScriptRuntime {
    fn segmenter_state(value: &Value) -> Option<Rc<SegmenterState>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Segmenter(state) => Some(state.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn segmenter_object(&self, prototype: &ObjectHandle, state: SegmenterState) -> Value {
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype.clone()),
            ObjectClass::Segmenter(Rc::new(state)),
        )))
    }

    fn segment_data(&self, segments: &Segments, (start, end, word): (usize, usize, bool)) -> Value {
        let mut entries = vec![
            (
                "segment".to_string(),
                Value::String(String::from_utf16_lossy(&segments.units[start..end]).into()),
            ),
            ("index".to_string(), Value::Number(start as f64)),
            (
                "input".to_string(),
                Value::String(segments.input.clone().into()),
            ),
        ];
        if segments.granularity == Granularity::Word {
            entries.push(("isWordLike".to_string(), Value::Bool(word)));
        }
        plain_object(entries)
    }

    pub(super) fn construct_segmenter(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let locales = arguments.first().cloned().unwrap_or(Value::Undefined);
        let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        let locale = self.locale_argument(&locales)?;
        let granularity = match self.option_text(&options, "granularity")?.as_deref() {
            None | Some("grapheme") => Granularity::Grapheme,
            Some("word") => Granularity::Word,
            Some("sentence") => Granularity::Sentence,
            Some(other) => {
                return Err(self.range_error(format!(
                    "Value {other} out of range for Intl.Segmenter options property granularity"
                )));
            }
        };
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.segmenters.segmenter.clone(),
        };
        Ok(self.segmenter_object(&prototype, SegmenterState::Segmenter(locale, granularity)))
    }

    pub(super) fn call_segmenter(
        &mut self,
        member: SegmenterMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        if member == SegmenterMember::Constructor {
            return Err(self.type_error("Constructor Intl.Segmenter requires 'new'"));
        }
        let Some(state) = Self::segmenter_state(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        match (member, &*state) {
            (SegmenterMember::Segment, SegmenterState::Segmenter(_, granularity)) => {
                let input = self.coerce_string(first)?;
                let units: Vec<u16> = input.encode_utf16().collect();
                let pieces = match granularity {
                    Granularity::Grapheme => crate::segment::grapheme_boundaries(&units)
                        .windows(2)
                        .map(|pair| (pair[0], pair[1], false))
                        .collect(),
                    Granularity::Word => crate::segment::word_segments(&units),
                    Granularity::Sentence => crate::segment::sentence_segments(&units)
                        .into_iter()
                        .map(|(start, end)| (start, end, false))
                        .collect(),
                };
                let segments = Rc::new(Segments {
                    input,
                    granularity: *granularity,
                    pieces,
                    units,
                });
                let prototype = self.intrinsics.segmenters.segments.clone();
                Ok(self.segmenter_object(&prototype, SegmenterState::Segments(segments)))
            }
            (SegmenterMember::ResolvedOptions, SegmenterState::Segmenter(locale, granularity)) => {
                Ok(plain_object([
                    ("locale".to_string(), Value::String(locale.clone().into())),
                    (
                        "granularity".to_string(),
                        Value::String(granularity.name().into()),
                    ),
                ]))
            }
            (SegmenterMember::Containing, SegmenterState::Segments(segments)) => {
                let index = match first {
                    Value::Undefined => 0.0,
                    value => self.coerce_number(value)?,
                };
                let index = if index.is_nan() { 0.0 } else { index.trunc() };
                if index < 0.0 || index >= segments.units.len() as f64 {
                    return Ok(Value::Undefined);
                }
                let index = index as usize;
                Ok(segments
                    .pieces
                    .iter()
                    .find(|(start, end, _)| (*start..*end).contains(&index))
                    .map_or(Value::Undefined, |&piece| {
                        self.segment_data(segments, piece)
                    }))
            }
            (SegmenterMember::Iterate, SegmenterState::Segments(segments)) => {
                let prototype = self.intrinsics.segmenters.cursor.clone();
                Ok(self.segmenter_object(
                    &prototype,
                    SegmenterState::Cursor(segments.clone(), Cell::new(0)),
                ))
            }
            (SegmenterMember::Next, SegmenterState::Cursor(segments, position)) => {
                let at = position.get();
                match segments.pieces.get(at) {
                    Some(&piece) => {
                        position.set(at + 1);
                        let data = self.segment_data(segments, piece);
                        Ok(self.iterator_result(data, false))
                    }
                    None => Ok(self.iterator_result(Value::Undefined, true)),
                }
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmenters_iterate_graphemes_words_and_sentences() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const graphemes = [...new Intl.Segmenter("en").segment("é\u{1F44D}\u{1F3FD}!")].map((piece) => piece.segment);
                seen.push(graphemes.length, graphemes[0].length);
                const words = new Intl.Segmenter("en", { granularity: "word" });
                const pieces = [...words.segment("Hi, you!")];
                seen.push(pieces.filter((piece) => piece.isWordLike).map((piece) => piece.segment).join("+"));
                seen.push(pieces[1].index, pieces[1].isWordLike, pieces[1].input);
                const found = words.segment("Hello world").containing(7);
                seen.push(found.segment, found.index, words.segment("ab").containing(5));
                const sentences = [...new Intl.Segmenter("en", { granularity: "sentence" }).segment("One. Two!")].map((piece) => piece.segment);
                seen.push(JSON.stringify(sentences), words.resolvedOptions().granularity);
                try { new Intl.Segmenter("en", { granularity: "line" }); } catch (error) { seen.push(error.name); }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(r#"3|2|Hi+you|2|false|Hi, you!|world|6||["One. ","Two!"]|word|RangeError"#)
        );
    }
}
