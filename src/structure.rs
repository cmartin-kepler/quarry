//! Structured text — build-plan Step G (the reading-order + paragraph layer).
//!
//! Turn a page's spans into **reading-order blocks** using the model-free XY-cut
//! segmenter ([`crate::segment`]) for block boundaries and a within-block
//! line/word ordering for reading order.
//!
//! Reading order is the crux the plan flags (multi-column scrambles *silently*):
//! XY-cut already emits leaves in reading order — vertical cuts run left→right
//! (columns), horizontal cuts top→bottom (rows) — so block order here IS reading
//! order, columns included. Within a block, words are grouped into lines by
//! vertical overlap, lines ordered top→bottom and words within a line left→right.
//!
//! Heading hierarchy (font-style runs) is deferred on purpose: the span model
//! carries no font metadata yet, so this layer delivers reading-order paragraphs;
//! headings land additively when spans carry size/weight.

use crate::artifact::Word;
use crate::core::BBox;
use crate::segment::{xy_cut, CutParams};

/// A reading-order block (a paragraph / column cell): its bounding box and its
/// words already ordered for reading.
pub struct Block {
    pub bbox: BBox,
    pub words: Vec<Word>,
}

impl Block {
    /// The block's text in reading order.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// Order a block's words for reading: cluster into lines by vertical overlap
/// (a word joins the current line if its vertical center sits within the line's
/// band), lines top→bottom, words within a line left→right.
fn order_words(mut words: Vec<Word>) -> Vec<Word> {
    words.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
    let mut lines: Vec<Vec<Word>> = Vec::new();
    for w in words {
        let (_, cy) = w.bbox.center();
        let joins = lines.last().is_some_and(|line| {
            let lo = line.iter().map(|x| x.bbox.y0).fold(f32::MAX, f32::min);
            let hi = line.iter().map(|x| x.bbox.y1).fold(f32::MIN, f32::max);
            cy >= lo && cy <= hi
        });
        if joins {
            lines.last_mut().unwrap().push(w);
        } else {
            lines.push(vec![w]);
        }
    }
    lines
        .into_iter()
        .flat_map(|mut line| {
            line.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
            line
        })
        .collect()
}

/// Segment a page's spans into reading-order blocks (paragraphs / column cells).
/// Blocks are returned in reading order.
pub fn blocks(spans: &[Word], p: &CutParams) -> Vec<Block> {
    xy_cut(spans, p)
        .into_iter()
        .map(|b| {
            let words: Vec<Word> =
                spans.iter().filter(|w| b.contains_center(&w.bbox)).cloned().collect();
            Block { bbox: b, words: order_words(words) }
        })
        .collect()
}

/// The page's text in reading order, paragraphs separated by a blank line.
pub fn document_text(spans: &[Word], p: &CutParams) -> String {
    blocks(spans, p).iter().map(Block::text).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Word {
        Word { text: text.into(), bbox: BBox::new(x0, y0, x1, y1) }
    }

    #[test]
    fn within_block_words_come_out_in_reading_order() {
        // deliberately scrambled input; two lines of two words each, tight spacing
        let spans = vec![
            w("B", 12.0, 0.0, 22.0, 10.0),
            w("D", 12.0, 12.0, 22.0, 22.0),
            w("A", 0.0, 0.0, 10.0, 10.0),
            w("C", 0.0, 12.0, 10.0, 22.0),
        ];
        let blocks = blocks(&spans, &CutParams::default());
        assert_eq!(blocks.len(), 1, "tight lines stay one paragraph block");
        assert_eq!(blocks[0].text(), "A B C D");
    }

    #[test]
    fn two_columns_read_left_column_fully_before_right() {
        // left column (x 0..30) and right column (x 120..150), wide gutter; two
        // tightly-spaced lines each → one block per column.
        let spans = vec![
            w("L1a", 0.0, 0.0, 10.0, 10.0),
            w("L1b", 12.0, 0.0, 22.0, 10.0),
            w("L2a", 0.0, 12.0, 10.0, 22.0),
            w("R1a", 120.0, 0.0, 130.0, 10.0),
            w("R2a", 120.0, 12.0, 130.0, 22.0),
        ];
        let text = document_text(&spans, &CutParams::default());
        // block text space-joins words in reading order; blocks split by blank line.
        // The point: the entire left column precedes the right (columns not scrambled).
        assert_eq!(text, "L1a L1b L2a\n\nR1a R2a");
    }

    #[test]
    fn stacked_paragraphs_split_on_the_blank_line() {
        // two paragraphs separated by a tall vertical gap → two blocks, top first
        let spans = vec![
            w("para", 0.0, 0.0, 30.0, 10.0),
            w("one", 32.0, 0.0, 50.0, 10.0),
            w("para", 0.0, 100.0, 30.0, 110.0),
            w("two", 32.0, 100.0, 50.0, 110.0),
        ];
        let text = document_text(&spans, &CutParams::default());
        assert_eq!(text, "para one\n\npara two");
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(document_text(&[], &CutParams::default()), "");
        assert!(blocks(&[], &CutParams::default()).is_empty());
    }
}
