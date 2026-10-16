// document selection over rendered prose: the text a reader sees, in document
// order, with flat UTF-16 positions that map back to the DOM text nodes

use crate::dom::{Document, NodeId};
use crate::layout::{LayoutDocument, TextRun};
use crate::text::{next_word_boundary, previous_word_boundary, word_at};

struct Segment {
    start: usize,
    end: usize,
    node_start: usize,
    box_node: NodeId,
    run: TextRun,
}

pub(crate) struct ProseText {
    segments: Vec<Segment>,
    // rendered units with the flat position each one starts at, plus the end position
    units: Vec<u16>,
    positions: Vec<usize>,
}

impl ProseText {
    pub(crate) fn new(layout: &LayoutDocument, document: &Document) -> Self {
        let node_starts = document.text_node_offsets();
        let mut segments = layout
            .prose_runs()
            .into_iter()
            .filter_map(|prose| {
                let node_start = *node_starts.get(&prose.node)?;
                let start = node_start + prose.run.node_offset;
                Some(Segment {
                    start,
                    end: start + prose.run.source_len,
                    node_start,
                    box_node: prose.box_node,
                    run: prose.run,
                })
            })
            .collect::<Vec<_>>();
        segments.sort_by_key(|segment| segment.start);
        let text = document.document_text().encode_utf16().collect::<Vec<_>>();
        let mut units = Vec::new();
        let mut positions = Vec::new();
        let mut previous: Option<&Segment> = None;
        for segment in &segments {
            if let Some(previous) = previous
                && (segment.start > previous.end || segment.box_node != previous.box_node)
            {
                units.push(if segment.box_node == previous.box_node {
                    u16::from(b' ')
                } else {
                    u16::from(b'\n')
                });
                positions.push(previous.end);
            }
            let covered = text
                .get(segment.start..segment.end.min(text.len()))
                .unwrap_or_default();
            units.extend_from_slice(covered);
            positions.extend(segment.start..segment.start + covered.len());
            previous = Some(segment);
        }
        positions.push(previous.map(|segment| segment.end).unwrap_or_default());
        Self {
            segments,
            units,
            positions,
        }
    }

    pub(crate) fn bounds(&self) -> Option<(usize, usize)> {
        Some((self.segments.first()?.start, self.segments.last()?.end))
    }

    fn index_of(&self, position: usize) -> usize {
        self.positions
            .partition_point(|&start| start < position)
            .min(self.units.len())
    }

    fn position_at(&self, index: usize) -> usize {
        self.positions[index.min(self.positions.len() - 1)]
    }

    pub(crate) fn step_character(&self, position: usize, forward: bool) -> usize {
        let index = self.index_of(position);
        let next = if forward {
            let wide = index + 1 < self.units.len()
                && is_high_surrogate(self.units[index])
                && is_low_surrogate(self.units[index + 1]);
            (index + if wide { 2 } else { 1 }).min(self.units.len())
        } else {
            let wide = index >= 2
                && is_low_surrogate(self.units[index - 1])
                && is_high_surrogate(self.units[index - 2]);
            index.saturating_sub(if wide { 2 } else { 1 })
        };
        self.position_at(next)
    }

    pub(crate) fn step_word(&self, position: usize, forward: bool) -> usize {
        let index = self.index_of(position);
        self.position_at(if forward {
            next_word_boundary(&self.units, index)
        } else {
            previous_word_boundary(&self.units, index)
        })
    }

    // the start or end of the visual line that holds the position
    pub(crate) fn line_edge(&self, position: usize, forward: bool) -> usize {
        // a position shared by two lines belongs to the one that starts there
        let Some(holder) = self
            .segments
            .iter()
            .find(|segment| segment.start <= position && position < segment.end)
            .or_else(|| {
                self.segments
                    .iter()
                    .find(|segment| segment.start <= position && position <= segment.end)
            })
            .or_else(|| {
                self.segments
                    .iter()
                    .rev()
                    .find(|segment| segment.start <= position)
            })
        else {
            return position;
        };
        let line = self
            .segments
            .iter()
            .filter(|segment| segment.box_node == holder.box_node && segment.run.y == holder.run.y);
        if forward {
            line.map(|segment| segment.end).max().unwrap_or(position)
        } else {
            line.map(|segment| segment.start).min().unwrap_or(position)
        }
    }

    // the block of text a triple click selects: every run laid out in the same box
    pub(crate) fn block_around(&self, position: usize) -> Option<(usize, usize)> {
        let holder = self
            .segments
            .iter()
            .find(|segment| segment.start <= position && position <= segment.end)?;
        let block = self
            .segments
            .iter()
            .filter(|segment| segment.box_node == holder.box_node);
        let start = block.clone().map(|segment| segment.start).min()?;
        let end = block.map(|segment| segment.end).max()?;
        Some((start, end))
    }

    // the rendered text between two flat positions, with a line break between blocks
    // and a space where a line wrapped
    pub(crate) fn text_between(&self, start: usize, end: usize) -> String {
        let mut output = String::new();
        let mut previous: Option<&Segment> = None;
        for segment in &self.segments {
            if segment.end < start || segment.start > end {
                continue;
            }
            let continued = start < segment.node_start;
            let local_start = start.saturating_sub(segment.node_start);
            let local_end = end.saturating_sub(segment.node_start);
            let Some((from, to)) = segment
                .run
                .selected_units(local_start, local_end, continued)
            else {
                continue;
            };
            let units = segment.run.text.encode_utf16().collect::<Vec<_>>();
            let piece = String::from_utf16_lossy(&units[from..to]);
            if let Some(previous) = previous {
                if previous.box_node != segment.box_node {
                    output.push('\n');
                } else if previous.run.y != segment.run.y && !piece.starts_with(' ') {
                    output.push(' ');
                }
            }
            output.push_str(&piece);
            previous = Some(segment);
        }
        output
    }
}

// the word around a node offset, as flat positions
pub(crate) fn word_around(
    document: &Document,
    node: NodeId,
    offset: usize,
) -> Option<(usize, usize)> {
    let node_start = document.node_text_offset(node)?;
    let units = document
        .character_data(node)?
        .encode_utf16()
        .collect::<Vec<_>>();
    let (start, end) = word_at(&units, offset);
    Some((node_start + start, node_start + end))
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xd800..0xdc00).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xdc00..0xe000).contains(&unit)
}
