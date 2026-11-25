// the devtools protocol calls a window's host answers: input as the window would deliver it,
// and screenshots of what it would show

use super::{EngineHost, InputEvent, InputResult, KeyModifiers, PointerButton, Press};
use crate::png::encode_png;
use crate::script::base64;
use serde_json::{Value as Json, json};

type Failure = (i64, String);

const MAX_SHOT_SIDE: usize = 16_384;

pub(super) struct AutomationReply {
    pub(super) id: u64,
    pub(super) answer: Result<Json, Failure>,
    pub(super) input: Option<InputResult>,
}

fn merge_input(first: InputResult, second: InputResult) -> InputResult {
    InputResult {
        handled: first.handled || second.handled,
        tasks_run: first.tasks_run + second.tasks_run,
        frame_requested: first.frame_requested || second.frame_requested,
        cursor: second.cursor.or(first.cursor),
        region: second.region.or(first.region),
    }
}

impl EngineHost {
    // the id, answer and any host input result of a message the host owns, or none for the browser
    pub(super) fn automation_call(&mut self, text: &str) -> Option<AutomationReply> {
        let message = serde_json::from_str::<Json>(text).ok()?;
        let method = message["method"].as_str()?;
        if !method.starts_with("Input.")
            && !matches!(method, "Page.captureScreenshot" | "Page.getLayoutMetrics")
        {
            return None;
        }
        let id = message["id"].as_u64()?;
        let params = &message["params"];
        let finish_input = |result: Result<InputResult, Failure>| match result {
            Ok(input) => (Ok(json!({})), Some(input)),
            Err(error) => (Err(error), None),
        };
        let (answer, input) = match method {
            "Page.captureScreenshot" => (self.screenshot(params), None),
            "Page.getLayoutMetrics" => (self.layout_metrics(), None),
            "Input.dispatchMouseEvent" => finish_input(self.mouse(params)),
            "Input.dispatchKeyEvent" => finish_input(self.key(params)),
            "Input.insertText" => {
                let text = params["text"].as_str().unwrap_or_default().to_string();
                finish_input(self.deliver(InputEvent::TextInput { text }))
            }
            _ => (Err((-32601, format!("'{method}' wasn't found"))), None),
        };
        Some(AutomationReply { id, answer, input })
    }

    fn deliver(&mut self, event: InputEvent) -> Result<InputResult, Failure> {
        self.input(event)
            .map_err(|error| (-32000, error.to_string()))
    }

    fn mouse(&mut self, params: &Json) -> Result<InputResult, Failure> {
        let (x, y) = (number(&params["x"])?, number(&params["y"])?);
        let button = match params["button"].as_str().unwrap_or("none") {
            "left" => Some(PointerButton::Primary),
            "middle" => Some(PointerButton::Auxiliary),
            "right" => Some(PointerButton::Secondary),
            _ => None,
        };
        let pressed = |button: Option<PointerButton>| {
            button.ok_or((-32602, "Invalid parameters: button".to_string()))
        };
        match params["type"].as_str().unwrap_or_default() {
            "mouseMoved" => self.deliver(InputEvent::PointerMove { x, y }),
            "mousePressed" => {
                let button = pressed(button)?;
                // the press series is the protocol's clickCount, not the host clock's guess
                let count = params["clickCount"].as_u64().unwrap_or(1).clamp(1, 3) as u32;
                self.last_press = (count > 1).then_some(Press {
                    time: self.clock,
                    x,
                    y,
                    count: count - 1,
                });
                self.deliver(InputEvent::PointerDown { x, y, button })
            }
            // a window follows a primary release with its click, and so does this
            "mouseReleased" => {
                let button = pressed(button)?;
                let up = self.deliver(InputEvent::PointerUp { x, y, button })?;
                match button {
                    PointerButton::Primary => {
                        Ok(merge_input(up, self.deliver(InputEvent::Click { x, y })?))
                    }
                    _ => Ok(up),
                }
            }
            "mouseWheel" => self.deliver(InputEvent::Scroll {
                x,
                y,
                delta_x: params["deltaX"].as_f64().unwrap_or_default() as f32,
                delta_y: params["deltaY"].as_f64().unwrap_or_default() as f32,
            }),
            _ => Err((-32602, "Invalid parameters: type".to_string())),
        }
    }

    fn key(&mut self, params: &Json) -> Result<InputResult, Failure> {
        let flags = params["modifiers"].as_u64().unwrap_or_default();
        let modifiers = KeyModifiers {
            alt: flags & 1 != 0,
            control: flags & 2 != 0,
            meta: flags & 4 != 0,
            shift: flags & 8 != 0,
        };
        let text = params["text"].as_str().unwrap_or_default().to_string();
        let key = match params["key"].as_str() {
            Some(key) if !key.is_empty() => key.to_string(),
            _ => text.clone(),
        };
        let code = params["code"].as_str().unwrap_or_default().to_string();
        let repeat = params["autoRepeat"] == true;
        let kind = params["type"].as_str().unwrap_or_default();
        match kind {
            // a keyDown carrying text is a raw key and its character, as a window gets them;
            // enter's "\r" and other control text are the key's own action, not typed text
            "keyDown" | "rawKeyDown" => {
                let down = self.deliver(InputEvent::Key {
                    key,
                    code,
                    modifiers,
                    repeat,
                    pressed: true,
                })?;
                match kind == "keyDown" && !text.is_empty() && !text.chars().all(char::is_control) {
                    true => Ok(merge_input(
                        down,
                        self.deliver(InputEvent::TextInput { text })?,
                    )),
                    false => Ok(down),
                }
            }
            "keyUp" => self.deliver(InputEvent::Key {
                key,
                code,
                modifiers,
                repeat,
                pressed: false,
            }),
            "char" => self.deliver(InputEvent::TextInput { text }),
            _ => Err((-32602, "Invalid parameters: type".to_string())),
        }
    }

    // the viewport and the document's scrolled extent, in css pixels and, for the older names,
    // device pixels
    fn layout_metrics(&mut self) -> Result<Json, Failure> {
        let layout = self
            .browser
            .layout(self.viewport)
            .map_err(|error| (-32000, error.to_string()))?;
        let (x, y) = self.browser.document_scroll();
        // the client size leaves out the root's scrollbars, as chrome reports it
        let port = crate::layout::scrollbar_metrics(&layout.root).scrollport;
        let (width, height) = (port.width, port.height);
        let content = layout.root.scroll_content;
        let (content_width, content_height) =
            (content.width.max(width), content.height.max(height));
        let viewport = |scale: f32| {
            json!({
                "pageX": x * scale,
                "pageY": y * scale,
                "clientWidth": width * scale,
                "clientHeight": height * scale,
            })
        };
        let visual = |scale: f32| {
            json!({
                "offsetX": 0,
                "offsetY": 0,
                "pageX": x * scale,
                "pageY": y * scale,
                "clientWidth": width * scale,
                "clientHeight": height * scale,
                "scale": 1,
                "zoom": 1,
            })
        };
        let size = |scale: f32| json!({ "x": 0, "y": 0, "width": content_width * scale, "height": content_height * scale });
        Ok(json!({
            "layoutViewport": viewport(self.scale),
            "visualViewport": visual(self.scale),
            "contentSize": size(self.scale),
            "cssLayoutViewport": viewport(1.0),
            "cssVisualViewport": visual(1.0),
            "cssContentSize": size(1.0),
        }))
    }

    // the viewport as the window shows it, or the part of it a clip names in page pixels
    fn screenshot(&mut self, params: &Json) -> Result<Json, Failure> {
        let jpeg = match params["format"].as_str() {
            None | Some("png") => false,
            Some("jpeg") => true,
            Some(_) => {
                return Err((
                    -32000,
                    "Only png and jpeg screenshots are supported".to_string(),
                ));
            }
        };
        let failed = |error: crate::navigation::NavigationError| (-32000, error.to_string());
        self.pump_workers().map_err(failed)?;
        let (canvas, _) = self
            .browser
            .paint_planes(self.viewport, self.scale, false)
            .map_err(failed)?;
        let (width, height) = (canvas.width() as usize, canvas.height() as usize);
        let pixels = canvas.rgba_pixels();
        let clip = &params["clip"];
        let (left, top, right, bottom, zoom) = if clip.is_object() {
            let (scroll_x, scroll_y) = self.browser.document_scroll();
            let x = (number(&clip["x"])? - scroll_x) * self.scale;
            let y = (number(&clip["y"])? - scroll_y) * self.scale;
            let (w, h) = (
                number(&clip["width"])? * self.scale,
                number(&clip["height"])? * self.scale,
            );
            let zoom = clip["scale"].as_f64().unwrap_or(1.0) as f32;
            let outside =
                x < -0.5 || y < -0.5 || x + w > width as f32 + 0.5 || y + h > height as f32 + 0.5;
            // the output side is capped as the decoders cap images, so a scale can't blow it up
            let too_big = (w * zoom).max(h * zoom) > MAX_SHOT_SIDE as f32;
            if outside || zoom.is_nan() || zoom <= 0.0 || w < 1.0 || h < 1.0 || too_big {
                return Err((
                    -32000,
                    "Only clips inside the viewport can be captured".to_string(),
                ));
            }
            let left = (x.round().max(0.0) as usize).min(width - 1);
            let top = (y.round().max(0.0) as usize).min(height - 1);
            let right = ((x + w).round() as usize).clamp(left + 1, width);
            let bottom = ((y + h).round() as usize).clamp(top + 1, height);
            (left, top, right, bottom, zoom)
        } else {
            (0, 0, width, height, 1.0)
        };
        // a clip's scale resamples the cut to the nearest pixel, as a css zoom of it would show
        let out_width = (((right - left) as f32 * zoom).round() as usize).max(1);
        let out_height = (((bottom - top) as f32 * zoom).round() as usize).max(1);
        let mut out = Vec::with_capacity(out_width * out_height * 3);
        for row in 0..out_height {
            let source_y = top + (row * (bottom - top) / out_height);
            for column in 0..out_width {
                let source_x = left + (column * (right - left) / out_width);
                let at = (source_y * width + source_x) * 4;
                out.extend_from_slice(&pixels[at..at + 3]);
            }
        }
        let (out_width, out_height) = (out_width as u32, out_height as u32);
        let encoded = if jpeg {
            // chromium's default quality when the caller names none
            let quality = params["quality"].as_u64().unwrap_or(80).min(100) as u8;
            crate::jpeg::encode_jpeg(out_width, out_height, &out, quality)
                .ok_or((-32000, "The screenshot could not be encoded".to_string()))?
        } else {
            encode_png(out_width, out_height, 3, &out)
        };
        Ok(json!({ "data": base64(&encoded) }))
    }
}

fn number(value: &Json) -> Result<f32, Failure> {
    value.as_f64().map(|number| number as f32).ok_or((
        -32602,
        "Invalid parameters: a number is missing".to_string(),
    ))
}
