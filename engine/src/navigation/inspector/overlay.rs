// the Overlay domain: highlights drawn over the page's frame, and the pointer picking a node
// for the elements panel instead of reaching the page

use super::dom::{self, Boxes, Mirror};
use super::{Failure, Session, post};
use crate::css::Color;
use crate::dom::NodeId;
use crate::navigation::Page;
use serde_json::{Value as Json, json};

// a viewport rectangle as left, top, right and bottom, and the colour over it
pub(crate) type Fill = ([f32; 4], Color);

pub(super) struct Highlight {
    // the node it follows as the layout moves, with the frontend's colours for it
    node: Option<(NodeId, Json)>,
    pub(super) fills: Vec<Fill>,
}

impl Highlight {
    pub(super) fn node(&self) -> Option<NodeId> {
        self.node.as_ref().map(|(node, _)| *node)
    }
}

// a session picking a node with the pointer, from setInspectMode to the click that picks
pub(super) struct Picking {
    pub(super) session: u64,
    // the hover highlight's colours, gone once the frontend ends the pick in the middle of a click
    pub(super) config: Option<Json>,
    pub(super) pressed: bool,
}

pub(super) fn following(node: NodeId, config: Json) -> Highlight {
    Highlight {
        node: Some((node, config)),
        fills: Vec::new(),
    }
}

fn color(config: &Json) -> Option<Color> {
    let channel = |name: &str| config[name].as_u64().map(|value| value.min(255) as u8);
    let alpha = config["a"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0);
    Some(Color {
        red: channel("r")?,
        green: channel("g")?,
        blue: channel("b")?,
        alpha: (alpha * 255.0).round() as u8,
    })
}

// the band between two rectangles as four rectangles, the outer's full width on top and
// bottom
fn ring(outer: [f64; 4], inner: [f64; 4]) -> [[f64; 4]; 4] {
    [
        [outer[0], outer[1], outer[2], inner[1]],
        [outer[0], inner[3], outer[2], outer[3]],
        [outer[0], inner[1], inner[0], inner[3]],
        [inner[2], inner[1], outer[2], inner[3]],
    ]
}

fn fills(boxes: &Boxes, config: &Json) -> Vec<Fill> {
    let areas = [
        ("contentColor", vec![boxes.content]),
        ("paddingColor", ring(boxes.padding, boxes.content).to_vec()),
        ("borderColor", ring(boxes.border, boxes.padding).to_vec()),
        ("marginColor", ring(boxes.margin, boxes.border).to_vec()),
    ];
    areas
        .into_iter()
        .filter_map(|(name, rects)| Some((color(&config[name])?, rects)))
        .flat_map(|(color, rects)| {
            rects
                .into_iter()
                .map(move |rect| (rect.map(|edge| edge as f32), color))
        })
        .collect()
}

// measures the followed node again, so the highlight stays on it as the page moves; a node
// that left the document takes its highlight along
pub(super) fn refresh(highlight: &mut Option<Highlight>, page: &mut Page) {
    let Some((node, config)) = highlight.as_ref().and_then(|held| held.node.clone()) else {
        return;
    };
    let attached = page.document.borrow().contains(node);
    let measured = attached.then(|| dom::measure(page, node).ok()).flatten();
    *highlight = measured.map(|boxes| Highlight {
        fills: fills(&boxes, &config),
        node: Some((node, config)),
    });
}

// answers one Overlay method, or None for a method this domain does not have
pub(super) fn answer(
    highlight: &mut Option<Highlight>,
    picking: &mut Option<Picking>,
    session: u64,
    mirror: &Mirror,
    page: &mut Page,
    method: &str,
    params: &Json,
) -> Option<Result<Json, Failure>> {
    let result = match method {
        "Overlay.enable" => Ok(json!({})),
        "Overlay.disable" | "Overlay.hideHighlight" => {
            let picker = picking.as_ref().is_some_and(|held| held.session == session);
            if method == "Overlay.disable" && picker {
                *picking = None;
            }
            *highlight = None;
            Ok(json!({}))
        }
        "Overlay.highlightNode" => dom::target(mirror, page, params).map(|node| {
            *highlight = Some(following(node, params["highlightConfig"].clone()));
            refresh(highlight, page);
            json!({})
        }),
        "Overlay.highlightRect" => {
            let number = |name: &str| params[name].as_f64().unwrap_or_default();
            let (left, top) = (number("x"), number("y"));
            let rect = [left, top, left + number("width"), top + number("height")];
            let inner = [rect[0] + 1.0, rect[1] + 1.0, rect[2] - 1.0, rect[3] - 1.0];
            let mut fills = Vec::new();
            if let Some(color) = color(&params["color"]) {
                fills.push((rect, color));
            }
            if let Some(outline) = color(&params["outlineColor"]) {
                fills.extend(ring(rect, inner).map(|band| (band, outline)));
            }
            *highlight = Some(Highlight {
                node: None,
                fills: fills
                    .into_iter()
                    .map(|(rect, color)| (rect.map(|edge| edge as f32), color))
                    .collect(),
            });
            Ok(json!({}))
        }
        "Overlay.setShowGridOverlays"
        | "Overlay.setShowFlexOverlays"
        | "Overlay.setShowScrollSnapOverlays"
        | "Overlay.setShowContainerQueryOverlays"
        | "Overlay.setShowIsolatedElements" => {
            // each takes one list of nodes to draw over; an empty one asks for nothing
            let configs = params.as_object().and_then(|params| params.values().next());
            match configs.and_then(Json::as_array).is_some_and(Vec::is_empty) {
                true => Ok(json!({})),
                false => Err((-32000, format!("'{method}' only clears its overlays"))),
            }
        }
        "Overlay.setInspectMode" => match params["mode"].as_str().unwrap_or_default() {
            "none" => {
                match picking {
                    // the click under way still belongs to the pick, not the page
                    Some(held) if held.pressed => held.config = None,
                    _ => *picking = None,
                }
                *highlight = None;
                Ok(json!({}))
            }
            "searchForNode" | "searchForUAShadowDOM" => {
                *picking = Some(Picking {
                    session,
                    config: Some(params["highlightConfig"].clone()),
                    pressed: false,
                });
                Ok(json!({}))
            }
            mode => Err((-32000, format!("Inspect mode {mode} isn't supported"))),
        },
        _ => return None,
    };
    Some(result)
}

// tells the picking session about the node under the pointer: hovered to highlight it in the
// tree, or pressed to inspect it
pub(super) fn report(session: &mut Session, page: &Page, url: &str, node: NodeId, pressed: bool) {
    if pressed {
        let id = session.dom.id(node);
        post(
            session,
            "Overlay.inspectNodeRequested",
            json!({ "backendNodeId": id }),
        );
        return;
    }
    let mut events = Vec::new();
    let pushed = session
        .dom
        .push(&page.document.borrow(), url, node, &mut events);
    for event in events {
        let _ = session.outgoing.send(event.to_string());
    }
    if let Some(id) = pushed {
        post(
            session,
            "Overlay.nodeHighlightRequested",
            json!({ "nodeId": id }),
        );
    }
}
