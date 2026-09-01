//! Block/inline layout and pagination (taffy plus our own implementation).

mod block;
mod box_tree;
mod flex;
mod float_ctx;
mod geometry;
mod grid;
mod inline;
mod page;
mod paginate;
mod table;
mod white_space;

pub(crate) use block::{
    has_visible_decoration, resolve_border, resolve_lpa_or_zero, resolve_padding,
    resolve_width_and_horizontal_margins,
};
pub use block::{
    layout_document, layout_document_from, LaidOutBox, LaidOutContent, LaidOutTable,
    LaidOutTableRow,
};
pub(crate) use box_tree::build_box_for_element;
pub use box_tree::{
    build_box_tree, resolve_background_images, resolve_images, BoxContent, ImageBoxContent,
    LayoutBox, TableBox, TableCell, TableRow,
};
pub use geometry::{EdgeSizes, FragmentPosition, Layout, Rect};
pub use inline::{shape_standalone_line, EmphasisMark, LineBox, TextRun};
pub use page::{PageSettings, PageSize};
pub(crate) use paginate::collect_completed_subtree_roots;
pub use paginate::{
    paginate, paginate_document, paginate_document_streaming, paginate_document_with_absolutes,
    paginate_streaming, Page, StreamingPaginator,
};
pub(crate) use white_space::is_collapsible_only;
