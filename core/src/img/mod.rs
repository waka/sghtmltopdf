//! Handling of `<img>` elements.

mod attrs;
mod cache;
mod fetch;
mod resolve;

pub use attrs::{read_img_attrs, ImgAttrs};
pub use cache::DocumentImageCache;
pub use fetch::{FetchError, ImageFetcher};
pub use resolve::{
    classify_img_src, resolve_against_base_href, resolve_local_asset_path, ImgSrc,
    ResolvedAssetPath,
};
