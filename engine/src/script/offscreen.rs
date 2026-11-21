// OffscreenCanvas over a canvas element no document holds: its 2d context, exports and image
// bitmaps are the element's, and script sees only the OffscreenCanvas

use super::canvas::Bitmap;
use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::is_primitive;
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OffscreenMember {
    Constructor,
    Width,
    SetWidth,
    Height,
    SetHeight,
    GetContext,
    ConvertToBlob,
    TransferToImageBitmap,
}

pub(super) struct OffscreenInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> OffscreenInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, get, set) in [
        ("width", OffscreenMember::Width, OffscreenMember::SetWidth),
        (
            "height",
            OffscreenMember::Height,
            OffscreenMember::SetHeight,
        ),
    ] {
        let getter = build.function(Native::Offscreen(get), &format!("get {name}"), 0);
        let setter = build.function(Native::Offscreen(set), &format!("set {name}"), 1);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(Value::Object(getter)),
                    set: Some(Value::Object(setter)),
                },
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    }
    for (name, member, length) in [
        ("getContext", OffscreenMember::GetContext, 1),
        ("convertToBlob", OffscreenMember::ConvertToBlob, 0),
        (
            "transferToImageBitmap",
            OffscreenMember::TransferToImageBitmap,
            0,
        ),
    ] {
        let function = build.function(Native::Offscreen(member), name, length);
        prototype
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("OffscreenCanvas".into()))
        },
    );
    let constructor = build.constructor(
        Native::Offscreen(OffscreenMember::Constructor),
        "OffscreenCanvas",
        2,
        &prototype,
    );
    globals.push(("OffscreenCanvas", Value::Object(constructor)));
    OffscreenInterfaces { prototype }
}

impl ScriptRuntime {
    // the canvas element behind an OffscreenCanvas
    pub(super) fn offscreen_element(value: &Value) -> Option<ElementHandle> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Offscreen(handle) => Some(handle.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    // the OffscreenCanvas a context's canvas element stands behind, for context.canvas
    pub(super) fn offscreen_object(&self, node: NodeId) -> Option<Value> {
        self.offscreen_canvases
            .get(&node)
            .and_then(std::rc::Weak::upgrade)
            .map(Value::Object)
    }

    fn offscreen_size(&mut self, value: Value) -> Result<u32, ScriptError> {
        let number = self.coerce_number(value)?;
        if !number.is_finite() || number < 0.0 || number > f64::from(u32::MAX) {
            return Err(self.type_error(
                "Failed to construct 'OffscreenCanvas': Value is outside the 'unsigned long long' value range.",
            ));
        }
        Ok(number.trunc() as u32)
    }

    pub(super) fn construct_offscreen(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let width = arguments.first().cloned().unwrap_or(Value::Undefined);
        let height = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        let width = self.offscreen_size(width)?;
        let height = self.offscreen_size(height)?;
        let node = {
            let mut document = self.document.borrow_mut();
            let node = document.create_element("canvas");
            document.set_attribute(node, "width", width.to_string());
            document.set_attribute(node, "height", height.to_string());
            node
        };
        let handle = self.node_handle(node);
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.offscreen.prototype.clone(),
        };
        let object = new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Offscreen(handle),
        ));
        self.offscreen_canvases.insert(node, Rc::downgrade(&object));
        Ok(Value::Object(object))
    }

    pub(super) fn call_offscreen(
        &mut self,
        member: OffscreenMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if member == OffscreenMember::Constructor {
            return Err(self.type_error("Constructor OffscreenCanvas requires 'new'"));
        }
        let Some(handle) = Self::offscreen_element(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        let node = handle.id;
        match member {
            OffscreenMember::Width | OffscreenMember::Height => {
                let name = if member == OffscreenMember::Width {
                    "width"
                } else {
                    "height"
                };
                self.canvas_element_member(&handle, name, None)
            }
            OffscreenMember::SetWidth | OffscreenMember::SetHeight => {
                let name = if member == OffscreenMember::SetWidth {
                    "width"
                } else {
                    "height"
                };
                let size = self.offscreen_size(first)?;
                self.document
                    .borrow_mut()
                    .set_attribute(node, name, size.to_string());
                self.reset_canvas(node);
                Ok(Value::Undefined)
            }
            OffscreenMember::GetContext => {
                let kind = self.coerce_string(first)?;
                Ok(match kind.as_str() {
                    "2d" if !self.has_webgl_context(node) => {
                        Value::CanvasContext(self.canvas_context(node))
                    }
                    "webgl" | "webgl2" => {
                        let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                        return self.webgl_get_context(this.clone(), node, &kind, &options);
                    }
                    "2d" | "bitmaprenderer" | "webgpu" => Value::Null,
                    _ => {
                        return Err(self.type_error(format!(
                            "Failed to execute 'getContext' on 'OffscreenCanvas': The provided value '{kind}' is not a valid enum value of type OffscreenRenderingContextType."
                        )));
                    }
                })
            }
            OffscreenMember::ConvertToBlob => {
                let options = first;
                let (mut kind, mut quality) = (Value::Undefined, Value::Undefined);
                if !is_primitive(&options) {
                    kind = self.get_property(&options, "type")?;
                    quality = self.get_property(&options, "quality")?;
                }
                let (width, height) = super::canvas::canvas_size(&self.document.borrow(), node);
                if width == 0 || height == 0 {
                    return Ok(Value::Promise(self.rejected_promise_value(dom_exception(
                        "IndexSizeError",
                        "Failed to execute 'convertToBlob' on 'OffscreenCanvas': The size of the OffscreenCanvas is zero.",
                    ))));
                }
                self.canvas_blob(node, kind, quality)
            }
            _ => {
                let (width, height) = super::canvas::canvas_size(&self.document.borrow(), node);
                if width == 0 || height == 0 {
                    return Err(self.throw_value(dom_exception(
                        "InvalidStateError",
                        "Failed to execute 'transferToImageBitmap' on 'OffscreenCanvas': Cannot transfer an ImageBitmap from an OffscreenCanvas with no context or with a size of 0.",
                    )));
                }
                // the bitmap leaves with the ImageBitmap, and the canvas starts over blank
                let bitmap = self
                    .canvas_bitmap(node)
                    .unwrap_or_else(|| Bitmap::new(width, height));
                self.reset_canvas(node);
                Ok(self.image_bitmap_object(bitmap))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offscreen_canvases_draw_export_and_hand_off_bitmaps() {
        let document = crate::dom::Document::parse(
            r#"<canvas id="c" width="4" height="4"></canvas><p id="out"></p>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const seen = [];
                const offscreen = new OffscreenCanvas(4, 2);
                const context = offscreen.getContext("2d");
                seen.push(offscreen instanceof OffscreenCanvas, context.canvas === offscreen, offscreen.width, offscreen.height);
                context.fillStyle = "#ff0000";
                context.fillRect(0, 0, 4, 2);
                const visible = document.getElementById("c").getContext("2d");
                visible.drawImage(offscreen, 0, 0);
                seen.push(Array.from(visible.getImageData(1, 1, 1, 1).data).join());
                const bitmap = offscreen.transferToImageBitmap();
                seen.push(bitmap instanceof ImageBitmap, bitmap.width, Array.from(context.getImageData(0, 0, 1, 1).data).join());
                offscreen.width = 8;
                seen.push(offscreen.width, offscreen.getContext("webgl"));
                try { offscreen.getContext("nope"); } catch (error) { seen.push(error.name); }
                offscreen.convertToBlob({ type: "image/jpeg" }).then((blob) => {
                    seen.push(blob.type, blob.size > 0);
                    return new OffscreenCanvas(0, 1).convertToBlob();
                }).catch((error) => {
                    seen.push(error.name);
                    document.getElementById("out").textContent = seen.join("|");
                });
                "##,
            )
            .expect("script should run");
        runtime.advance_time(1).expect("tasks should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true|true|4|2|255,0,0,255|true|4|0,0,0,0|8||TypeError|image/jpeg|true|IndexSizeError"
            )
        );
    }
}
