# Plan: Tagged PDF in streaming mode

## Goal

Make `--tagged` work with `--streaming` (`Mode::Streaming`), producing the same
structure tree, marked content, ParentTree and link annotations as batch mode
for the same document, without giving up incremental page output.

Explicitly out of scope for this work:

- Fixing the two known batch-mode structure limitations (paint order instead of
  DOM order; a `Link` that is a sibling of, not a parent of, a linked `Figure`).
  Streaming inherits batch semantics so the two modes stay comparable.
- A backend-independent display list, Stylo integration, and PDF/UA-2
  conformance. Tagged output is a prerequisite for PDF/UA, not proof of it.
- Tagging cover and TOC pages. They are separate documents whose `NodeId`s
  alias the body's; batch already risks aliasing there, streaming will instead
  suspend tagging for them.

## Ground truth (verified in source, not just README/changelog)

- The structure machinery already exists and is already streaming-shaped.
  `Tagger` in `core/src/pdf/tagging.rs` builds the tree incrementally while
  pages are painted, allocates MCIDs per stream, and writes the
  `/StructTreeRoot` plus `/ParentTree` at the end.
- `StreamingPdfWriter` in `core/src/pdf/streaming.rs` already owns an optional
  `Tagger`, tags each page as it is written, and writes the structure tree in
  `finish`. Batch mode uses this same writer.
- The only hard blocker is in `core/src/engine/streaming.rs`: streaming rejects
  `output.tagged`, because `collect_tag_roles(&dom)` needs the whole DOM while
  streaming frees each top-level subtree after layout.
- `LayoutBox.node` already carries `NodeId` through layout and pagination, so
  no new identity plumbing is needed. Painting already drives `Tagger`
  begin/end calls in `core/src/pdf/document.rs`.

## Design

### 1. Incremental tag-role collection

Split `collect_tag_roles` in `core/src/pdf/tagging.rs` into a whole-document
entry point (unchanged behaviour for batch) and a subtree entry point that
collects roles for one top-level element and its descendants.

- The subtree walk must reproduce whole-document semantics, including the
  `in_head` (`<thead>`) tracking used for `TH` scope inference. A top-level
  element is a child of `<body>`, so starting `in_head` as `false` at the
  subtree root is equivalent.
- Add unit tests proving subtree collection over the children of `<body>`
  unions to the same map as whole-document collection, including tables with
  `<thead>`, images with and without `alt`, and nested structure.

### 2. Feed roles to the writer as elements are processed

- Add a `Tagger`/`StreamingPdfWriter` API to extend the role map after writer
  creation (roles arrive incrementally; the writer is built before the first
  top-level element is processed).
- In `StreamingState::process_top_level_element`, collect the subtree's roles
  while the DOM subtree is still intact (before layout and before
  `release_processed`), and hand them to the writer. This covers block items
  and held inline-run items alike, since both pass through that function.
- Remove the `tagged` rejection in `init_streaming_state` and pass the real
  `output` options (including `tagged`) to `StreamingPdfWriter::with_options`
  instead of an empty role map.

### 3. Keep separate documents untagged

- Cover pages are written before any body roles exist, so they stay untagged
  naturally.
- TOC pages are written at `finish_streaming` time, after body roles have been
  collected, and their synthetic `NodeId`s can alias body nodes. Suspend the
  `Tagger` around TOC `write_page` calls via a writer-level suspend/resume,
  mirroring how header/footer overlays are already treated as artifacts.
- Header/footer and margin-box overlays already render as artifacts; no change.

### 4. Memory contract

Streaming tagged mode cannot be strictly bounded: the `Tagger` retains one
structure element per tagged node, one owner entry per MCID, and the
ParentTree data until `finish`, alongside the writer's existing per-object
offsets, font glyph usage, destinations and outline entries.

That retention is proportional to structure size, not to laid-out page
content. This trade-off must be stated in the streaming docs instead of
claiming memory "does not grow with document size" for tagged mode. Untagged
streaming keeps its existing memory profile.

## Logical blocks of work (one commit each)

1. **This plan.** Revised, source-grounded plan only.
2. **Incremental tag-role collection.** Subtree collector in
   `core/src/pdf/tagging.rs` plus unit tests; batch behaviour unchanged.
3. **Streaming enablement.** Writer role-extension and tagging-suspension APIs,
   engine wiring, removal of the rejection, and end-to-end tests in
   `core/tests/tagged.rs` (streaming produces a structure tree; batch and
   streaming agree on structure types, MCID/ParentTree integrity, table,
   list, link-annotation and figure cases; cover/TOC stay untagged).
   The failing streaming test is observed before the engine change.
4. **Docs and changelog.** `changelog.d/` entry, CLI reference and streaming
   docs updated to drop the "cannot be combined" wording and state the
   tagged-mode memory retention; English `.po` strings kept in step.

## Verification

- `cargo test -p sghtmltopdf --test tagged` for the tagged suite.
- `cargo test --workspace` plus `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo fmt --all --check`, matching CI.
- A like-for-like batch-vs-streaming comparison in tests, not byte equality:
  page content legitimately differs in object ordering, so compare structure
  roles, MCID uniqueness/completeness, ParentTree entries and annotation
  ownership.
