//! A simple context tracking `float` placement (a simplified shelf-packing of CSS2.1 9.5.1).
//!
//! One [`FloatContext`] is shared across an entire call to
//! `layout_document`/`layout_document_from` (this repository implements no property other
//! than `float` that establishes a Block Formatting Context). A fresh, empty context is
//! passed for the contents of a `float` itself and for the contents of a `display: table`
//! cell (both of which establish a new BFC).

use crate::style::{Clear, Float};

/// One float's rectangle in absolute (within-page) coordinates. `inner_edge_x` holds only
/// the inner boundary needed for flow-around (the right edge for a left float, the left edge for a right float).
#[derive(Debug, Clone, Copy)]
struct FloatEntry {
    top: f32,
    bottom: f32,
    inner_edge_x: f32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FloatContext {
    left: Vec<FloatEntry>,
    right: Vec<FloatEntry>,
}

impl FloatContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Find the absolute coordinates (the top left of the margin box) at which to place a
    /// float on the `side` side. The search starts at `preferred_top` (the cursor_y where
    /// the float appeared in the DOM flow) and takes the first Y at which the width
    /// occupied by same-side floats plus the new float's width does not exceed
    /// `containing_right - containing_left`. Where it does, Y advances to the shallowest
    /// bottom edge among the same-side floats overlapping that Y, and it tries again (shelf-packing).
    pub fn place(
        &self,
        side: Float,
        preferred_top: f32,
        containing_left: f32,
        containing_right: f32,
        margin_box_width: f32,
    ) -> (f32, f32) {
        let entries: &[FloatEntry] = match side {
            Float::Left => &self.left,
            Float::Right => &self.right,
            Float::None => return (containing_left, preferred_top),
        };

        let mut y = preferred_top;
        loop {
            let overlapping: Vec<&FloatEntry> = entries
                .iter()
                .filter(|e| e.top <= y && y < e.bottom)
                .collect();

            let (available_left, available_right) = if side == Float::Left {
                (
                    overlapping
                        .iter()
                        .map(|e| e.inner_edge_x)
                        .fold(containing_left, f32::max),
                    containing_right,
                )
            } else {
                (
                    containing_left,
                    overlapping
                        .iter()
                        .map(|e| e.inner_edge_x)
                        .fold(containing_right, f32::min),
                )
            };

            if available_right - available_left >= margin_box_width {
                let x = if side == Float::Left {
                    available_left
                } else {
                    available_right - margin_box_width
                };
                return (x, y);
            }

            let next_y = overlapping
                .iter()
                .map(|e| e.bottom)
                .fold(f32::INFINITY, f32::min);
            if next_y.is_finite() && next_y > y {
                y = next_y;
                continue;
            }

            // Nowhere left to advance to (margin_box_width itself exceeds the containing width, say):
            // avoid an infinite loop and settle here as a best effort, allowing the overflow.
            let x = if side == Float::Left {
                available_left
            } else {
                available_right - margin_box_width
            };
            return (x, y);
        }
    }

    /// Register the float once its placement is settled.
    pub fn register(
        &mut self,
        side: Float,
        x: f32,
        y: f32,
        margin_box_width: f32,
        margin_box_height: f32,
    ) {
        let entry = FloatEntry {
            top: y,
            bottom: y + margin_box_height,
            inner_edge_x: if side == Float::Left {
                x + margin_box_width
            } else {
                x
            },
        };
        match side {
            Float::Left => self.left.push(entry),
            Float::Right => self.right.push(entry),
            Float::None => {}
        }
    }

    /// Return the `(available_left, available_width)` not occupied by floats in the band
    /// from `y` to `y+height` (used by `inline.rs` to decide line wrapping).
    pub fn available_band(
        &self,
        y: f32,
        height: f32,
        containing_left: f32,
        containing_right: f32,
    ) -> (f32, f32) {
        let overlaps = |e: &&FloatEntry| e.top < y + height && y < e.bottom;

        let left_edge = self
            .left
            .iter()
            .filter(overlaps)
            .map(|e| e.inner_edge_x)
            .fold(containing_left, f32::max);
        let right_edge = self
            .right
            .iter()
            .filter(overlaps)
            .map(|e| e.inner_edge_x)
            .fold(containing_right, f32::min);

        (left_edge, (right_edge - left_edge).max(0.0))
    }

    /// The Y after pushing down past the lowest float on the `clear` side (or `current_y` if there is none).
    pub fn clearance(&self, clear: Clear, current_y: f32) -> f32 {
        let max_bottom =
            |entries: &[FloatEntry]| entries.iter().map(|e| e.bottom).fold(current_y, f32::max);
        match clear {
            Clear::None => current_y,
            Clear::Left => max_bottom(&self.left),
            Clear::Right => max_bottom(&self.right),
            Clear::Both => max_bottom(&self.left).max(max_bottom(&self.right)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_float_places_at_preferred_top_against_containing_edge() {
        let ctx = FloatContext::new();
        assert_eq!(
            ctx.place(Float::Left, 100.0, 0.0, 500.0, 120.0),
            (0.0, 100.0)
        );
        assert_eq!(
            ctx.place(Float::Right, 100.0, 0.0, 500.0, 120.0),
            (380.0, 100.0)
        );
    }

    #[test]
    fn second_left_float_packs_next_to_the_first() {
        let mut ctx = FloatContext::new();
        ctx.register(Float::Left, 0.0, 0.0, 100.0, 50.0);
        assert_eq!(ctx.place(Float::Left, 0.0, 0.0, 500.0, 100.0), (100.0, 0.0));
    }

    #[test]
    fn third_float_wraps_to_next_shelf_when_it_does_not_fit() {
        let mut ctx = FloatContext::new();
        // A short but wide float (occupying x:100-300 over y:0-30).
        ctx.register(Float::Left, 100.0, 0.0, 200.0, 30.0);
        // A tall but narrow float (occupying x:0-50 over y:0-200).
        ctx.register(Float::Left, 0.0, 0.0, 50.0, 200.0);

        // A float of width 400 does not fit at y=0 (300 occupied, 200 free). Advancing to
        // y=30, where the first float ends, leaves only 50 occupied, freeing 450, so it fits.
        assert_eq!(ctx.place(Float::Left, 0.0, 0.0, 500.0, 400.0), (50.0, 30.0));
    }

    #[test]
    fn place_overflows_when_float_is_wider_than_containing_block() {
        let ctx = FloatContext::new();
        assert_eq!(ctx.place(Float::Left, 0.0, 0.0, 100.0, 200.0), (0.0, 0.0));
    }

    #[test]
    fn available_band_narrows_around_overlapping_floats() {
        let mut ctx = FloatContext::new();
        ctx.register(Float::Left, 0.0, 0.0, 100.0, 50.0);
        ctx.register(Float::Right, 400.0, 0.0, 100.0, 50.0);
        assert_eq!(ctx.available_band(10.0, 20.0, 0.0, 500.0), (100.0, 300.0));
    }

    #[test]
    fn available_band_ignores_floats_outside_the_vertical_band() {
        let mut ctx = FloatContext::new();
        ctx.register(Float::Left, 0.0, 0.0, 100.0, 50.0);
        assert_eq!(ctx.available_band(100.0, 20.0, 0.0, 500.0), (0.0, 500.0));
    }

    #[test]
    fn clearance_pushes_down_to_the_bottom_of_relevant_floats() {
        let mut ctx = FloatContext::new();
        ctx.register(Float::Left, 0.0, 0.0, 100.0, 50.0);
        ctx.register(Float::Right, 400.0, 10.0, 100.0, 80.0);

        assert_eq!(ctx.clearance(Clear::None, 5.0), 5.0);
        assert_eq!(ctx.clearance(Clear::Left, 5.0), 50.0);
        assert_eq!(ctx.clearance(Clear::Right, 5.0), 90.0);
        assert_eq!(ctx.clearance(Clear::Both, 5.0), 90.0);
    }
}
