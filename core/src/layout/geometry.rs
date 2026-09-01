//! Coordinate and rectangle types for layout results.

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EdgeSizes {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// Whether pagination has split the original box into fragments spanning several pages.
///
/// `border-radius` is looked up from the element's computed style
/// (`border_top_left_radius` and friends) each time, so on a "continuing" fragment of a
/// box split across pages (a `Middle`, which is neither first nor last; the bottom of a
/// `First`; the top of a `Last`) the corners of an edge that has no border must not be
/// rounded. The drawing side ([`crate::pdf::document`]) cannot tell the difference without
/// this, so it is carried on [`Layout`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FragmentPosition {
    /// An ordinary, unsplit box. `border-radius` applies to every corner.
    #[default]
    Whole,
    /// The first of the fragments. Only the top corners get `border-radius`.
    First,
    /// A fragment that is neither first nor last. No corner is rounded.
    Middle,
    /// The last of the fragments. Only the bottom corners get `border-radius`.
    Last,
}

/// The areas of the box model. Only `content` carries absolute coordinates (within the
/// page); the other edges hold only their thickness.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Layout {
    pub content: Rect,
    pub padding: EdgeSizes,
    pub border: EdgeSizes,
    pub margin: EdgeSizes,
    pub fragment: FragmentPosition,
}

impl Layout {
    /// Vertical space occupied up to the next sibling box (the margin box height).
    pub fn margin_box_height(&self) -> f32 {
        self.margin.top
            + self.border.top
            + self.padding.top
            + self.content.height
            + self.padding.bottom
            + self.border.bottom
            + self.margin.bottom
    }

    /// The border box, used for drawing backgrounds and borders.
    pub fn border_box(&self) -> Rect {
        Rect {
            x: self.content.x - self.padding.left - self.border.left,
            y: self.content.y - self.padding.top - self.border.top,
            width: self.border.left
                + self.padding.left
                + self.content.width
                + self.padding.right
                + self.border.right,
            height: self.border.top
                + self.padding.top
                + self.content.height
                + self.padding.bottom
                + self.border.bottom,
        }
    }

    /// The padding box (content plus padding, inside the border line), used as the clipping
    /// boundary for `overflow`.
    pub fn padding_box(&self) -> Rect {
        Rect {
            x: self.content.x - self.padding.left,
            y: self.content.y - self.padding.top,
            width: self.padding.left + self.content.width + self.padding.right,
            height: self.padding.top + self.content.height + self.padding.bottom,
        }
    }
}
