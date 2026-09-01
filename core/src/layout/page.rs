//! Page size and margin definitions. The inside of the page (its content area) is the containing block.

use super::geometry::EdgeSizes;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    /// 210mm x 297mm (at 96dpi).
    pub const A4: PageSize = PageSize {
        width: 793.7,
        height: 1122.5,
    };
    /// 297mm x 420mm (at 96dpi).
    pub const A3: PageSize = PageSize {
        width: 1122.5,
        height: 1587.4,
    };
    /// 148mm x 210mm (at 96dpi).
    pub const A5: PageSize = PageSize {
        width: 559.4,
        height: 793.7,
    };
    /// 8.5in x 11in (at 96dpi, for `size: letter` in `@page`).
    pub const LETTER: PageSize = PageSize {
        width: 816.0,
        height: 1056.0,
    };
    /// 8.5in x 14in (at 96dpi).
    pub const LEGAL: PageSize = PageSize {
        width: 816.0,
        height: 1344.0,
    };

    /// Swap width and height (for the `landscape` modifier).
    pub fn landscape(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSettings {
    pub size: PageSize,
    pub margin: EdgeSizes,
}

impl Default for PageSettings {
    /// Default margin, equal to one inch (96px).
    fn default() -> Self {
        Self {
            size: PageSize::A4,
            margin: EdgeSizes {
                top: 96.0,
                right: 96.0,
                bottom: 96.0,
                left: 96.0,
            },
        }
    }
}

impl PageSettings {
    pub fn content_width(&self) -> f32 {
        self.size.width - self.margin.left - self.margin.right
    }

    pub fn content_height(&self) -> f32 {
        self.size.height - self.margin.top - self.margin.bottom
    }
}
