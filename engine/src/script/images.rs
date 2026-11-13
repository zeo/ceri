// img elements loading their source the way a browser does: a request starts when src is set
// or an element with one arrives, and its outcome is reported from a task as load or error

use super::realm::Native;
use super::*;
use crate::dom::Namespace;
use crate::image::Image;

pub(crate) type ImageCache = RefCell<HashMap<String, Option<Arc<Image>>>>;

#[derive(Default)]
pub(super) struct ImageLoad {
    generation: u64,
    // the resolved source, None when src is missing or names nothing usable
    url: Option<String>,
    natural: Option<(u32, u32)>,
    pending: bool,
    waiters: Vec<PromiseHandle>,
}

// the key layout and script both cache a decoded image reference under
pub(crate) fn image_key(base: &AppUrl, reference: &str) -> Option<String> {
    let reference = reference.trim();
    if reference
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
    {
        return Some(reference.to_string());
    }
    base.resolve(reference).ok().map(|url| url.to_string())
}

fn decode_failure() -> Value {
    dom_exception("EncodingError", "The source image cannot be decoded.")
}

impl ScriptRuntime {
    pub(crate) fn image_cache(&self) -> &ImageCache {
        &self.images
    }

    pub(crate) fn load_document_images(&mut self) {
        let root = self.document_node();
        self.load_images_in(root);
    }

    // starts a load for every img, audio and video with a source in a tree that just came
    // into being
    pub(super) fn load_images_in(&mut self, root: NodeId) {
        let nodes = {
            let document = self.document.borrow();
            std::iter::once(root)
                .chain(document.descendants(root))
                .filter(|&node| {
                    is_image(&document, node) && document.attribute(node, "src").is_some()
                })
                .collect::<Vec<_>>()
        };
        for node in nodes {
            self.update_image(node);
        }
        self.start_media_loads_in(root);
    }

    // runs when src is set, even to the value it had, or removed
    pub(super) fn update_image(&mut self, node: NodeId) {
        if !is_image(&self.document.borrow(), node) {
            return;
        }
        let source = self
            .document
            .borrow()
            .attribute(node, "src")
            .map(str::to_string);
        let url = source.as_deref().and_then(|source| {
            let base = self.base_url.as_ref()?;
            image_key(base, source).filter(|_| !source.trim().is_empty())
        });
        let cached = url
            .as_ref()
            .and_then(|url| self.images.borrow().get(url).cloned().flatten());
        let load = self.image_loads.entry(node).or_default();
        load.generation += 1;
        let generation = load.generation;
        let abandoned = std::mem::take(&mut load.waiters);
        // an image this document already decoded is available at once, as in a browser
        load.natural = cached.map(|image| (image.width, image.height));
        load.pending = source.is_some() && load.natural.is_none();
        load.url = url;
        for waiter in abandoned {
            self.settle_promise(&waiter, PromiseOutcome::Rejected(decode_failure()));
        }
        // a missing src reports nothing, an empty or unusable one reports an error
        if source.is_some() {
            self.schedule_native_task(Native::ImageDeliver(node, generation));
        }
    }

    pub(super) fn deliver_image(
        &mut self,
        node: NodeId,
        generation: u64,
    ) -> Result<Value, ScriptError> {
        let url = match self.image_loads.get(&node) {
            Some(load) if load.generation == generation => load.url.clone(),
            _ => return Ok(Value::Undefined),
        };
        let image = url.and_then(|url| self.decoded_image(&url));
        let load = self.image_loads.get_mut(&node).expect("checked above");
        load.natural = image.map(|image| (image.width, image.height));
        load.pending = false;
        let loaded = load.natural.is_some();
        for waiter in std::mem::take(&mut load.waiters) {
            let outcome = match loaded {
                true => PromiseOutcome::Fulfilled(Value::Undefined),
                false => PromiseOutcome::Rejected(decode_failure()),
            };
            self.settle_promise(&waiter, outcome);
        }
        let event_type = if loaded { "load" } else { "error" };
        let event = self.new_event("Event", event_type.to_string(), false, false, false, true);
        self.dispatch(&event, events::ListenerTarget::Node(node))?;
        Ok(Value::Undefined)
    }

    // decodes once per document; a failure is tried again on the next request for it
    fn decoded_image(&mut self, url: &str) -> Option<Arc<Image>> {
        if let Some(Some(image)) = self.images.borrow().get(url) {
            return Some(image.clone());
        }
        let decoded = match crate::image::decode_data_url(url) {
            Some((mime_type, bytes)) => {
                let check = self.content_security_policy().url(
                    crate::csp::Directive::Img,
                    url,
                    crate::csp::Initiator::default(),
                );
                self.permits(check)
                    .then(|| crate::image::decode_image(&bytes, &mime_type).ok())
                    .flatten()
            }
            None => {
                let promise = self.fetch(
                    &[Value::String(url.to_string().into())],
                    crate::csp::Directive::Img,
                    "Image",
                );
                let response = match &promise.borrow().outcome {
                    PromiseOutcome::Fulfilled(Value::Response(response))
                        if (200..300).contains(&response.status) =>
                    {
                        Some(response.clone())
                    }
                    _ => None,
                };
                response.and_then(|response| {
                    crate::image::decode_image(&response.body.borrow().bytes, &response.mime_type)
                        .ok()
                })
            }
        }
        .map(Arc::new);
        self.images
            .borrow_mut()
            .insert(url.to_string(), decoded.clone());
        decoded
    }

    // the decoded picture drawImage reads; Err when the load it made failed, which leaves
    // the element broken rather than merely empty
    pub(super) fn image_content(&self, node: NodeId) -> Result<Option<Arc<Image>>, ()> {
        let Some(load) = self.image_loads.get(&node).filter(|load| !load.pending) else {
            return Ok(None);
        };
        let Some(url) = &load.url else {
            return Ok(None);
        };
        match self.images.borrow().get(url).cloned().flatten() {
            Some(image) if load.natural.is_some() => Ok(Some(image)),
            _ => Err(()),
        }
    }

    pub(super) fn image_member(&mut self, node: NodeId, name: &str) -> Value {
        let load = self.image_loads.get(&node);
        let natural = load.and_then(|load| load.natural).unwrap_or_default();
        match name {
            "naturalWidth" => Value::Number(f64::from(natural.0)),
            "naturalHeight" => Value::Number(f64::from(natural.1)),
            "complete" => Value::Bool(!load.is_some_and(|load| load.pending)),
            "currentSrc" => Value::String(
                load.and_then(|load| load.url.clone())
                    .unwrap_or_default()
                    .into(),
            ),
            "decode" => {
                let promise = match load {
                    Some(load) if load.pending => {
                        let promise = Self::pending_promise();
                        self.image_loads
                            .get_mut(&node)
                            .expect("matched above")
                            .waiters
                            .push(promise.clone());
                        promise
                    }
                    Some(load) if load.natural.is_some() => {
                        self.fulfilled_promise(Value::Undefined)
                    }
                    _ => self.rejected_promise_value(decode_failure()),
                };
                Value::Promise(promise)
            }
            _ => Value::Undefined,
        }
    }

    // new Image(width, height)
    pub(super) fn construct_image(&mut self, arguments: &[Value]) -> Result<Value, ScriptError> {
        let node = self.document.borrow_mut().create_element("img");
        for (name, value) in ["width", "height"].into_iter().zip(arguments) {
            if !matches!(value, Value::Undefined) {
                let size = self.coerce_uint32(value.clone())?;
                self.document
                    .borrow_mut()
                    .set_attribute(node, name, size.to_string());
            }
        }
        Ok(self.node_value(node))
    }
}

fn is_image(document: &Document, node: NodeId) -> bool {
    document
        .element(node)
        .is_some_and(|element| element.name == "img" && element.namespace == Namespace::Html)
}
