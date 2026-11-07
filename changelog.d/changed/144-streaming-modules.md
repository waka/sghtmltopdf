The `Mode::Streaming` pipeline moved out of `engine.rs` and `layout/paginate.rs` into the
`engine::streaming` and `layout::paginate::streaming` submodules (#144). This is internal
only: `Engine`'s public API and the output are unchanged.
