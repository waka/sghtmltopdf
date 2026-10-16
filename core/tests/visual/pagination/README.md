# Pagination visual comparison

Compare **main in batch mode** with **the current checkout in streaming mode**, using the same bundled DejaVu Sans font, A5 page size, and margins. The script exports main with `git archive` and builds it separately; it never switches or modifies the current checkout. `main` means the local branch, not an automatically fetched remote branch.

## Run on Windows

Requires Cargo and Python 3.12+ (tested with Python 3.13). Install the pinned renderer in a virtual environment:

```powershell
python -m venv target\visual-pagination-venv
.\target\visual-pagination-venv\Scripts\python.exe -m pip install -r core\tests\visual\pagination\requirements.txt
.\target\visual-pagination-venv\Scripts\python.exe core\tests\visual\pagination\compare.py
```

If `python` is a Windows Store alias, use the full path to an installed Python executable for the first command.

Open [the generated report](../../../../target/visual-pagination/index.html) in a browser. Each case contains both PDFs and page-by-page main, streaming, and pink difference images. Generated sources, isolated build directories, PDFs, images, logs, and JSON results stay under the already ignored `target` directory.

The report records the baseline SHA, current SHA/worktree status, binary hashes, PyMuPDF version, DPI, page counts, changed-pixel percentages, extracted-text equality, and link counts. Text/link observations are diagnostic only: the pass/fail criterion is equal page counts/dimensions and pixel equality within the configured tolerance. A nonzero exit code means at least one mismatch or conversion failure. A match against main is a parity check, not proof that main's output is correct.

## Coverage

The 24-case base matrix covers every combination of:

- TOC off/on;
- no cover / one-page cover / two-page cover;
- page offset 0 / 10;
- simple CLI headers/footers / HTML headers/footers with current and total page numbers.

Eight additional cases cover empty heading lists, multi-page TOCs, offsets near digit boundaries (98 and 998), CSS page counters and `:first`/`:left`/`:right` margin-box selectors, disabled TOC links, and uncompressed PDF output. Body headings include explicit and automatic destinations, nested levels, and forward/backward document links.

## Focused runs

```powershell
# Reuse built binaries and render just one case.
.\target\visual-pagination-venv\Scripts\python.exe core\tests\visual\pagination\compare.py --skip-build --case multi-toc-cover-offset

# Higher resolution; allow a small per-channel raster difference.
.\target\visual-pagination-venv\Scripts\python.exe core\tests\visual\pagination\compare.py --dpi 144 --tolerance 2
```

The default is an exact comparison at 96 DPI (`--tolerance 0`). `--case` is repeatable. Omit `--skip-build` after code changes; otherwise stale binaries are intentionally reused. `--baseline-ref` can select another local ref or commit. `--output` must remain under this repository's `target` directory. Each run replaces the report with the selected cases; previous unselected case folders may remain on disk.

These are manual/opt-in visual tests; no new dependencies are added to the Rust test suite or normal builds. System-font discovery is disabled and fixture heading/table font weights are normal, so different batch/streaming fallback-font discovery does not obscure pagination comparisons.

## Results after the streaming spacing fix

Against local main `369efa3`, **all 32 cases and all 256 page pairs match pixel-for-pixel** with PyMuPDF 1.26.7 at 96 DPI and zero tolerance.

The fix carries adjacent top-level sibling margins across streamed elements and preserves their overlapping layout coordinates during pagination, rather than stacking margin-box heights. Positive, unequal, mixed-sign, and negative margins have regression coverage, including hidden intervening elements and forced page breaks. The spacing defect also reproduces in main's streaming mode without a TOC, so it predates the TOC implementation.

### Initial results before the fix

At local main `369efa3` versus streaming `4be7d2f`, PyMuPDF 1.26.7 at 96 DPI with exact pixels:

- **28 of 32 cases match exactly**, across 256 compared pages overall.
- The entire 24-case TOC/cover/offset/header-footer matrix matches.
- CSS counters/selectors, three-digit offsets, disabled TOC links, and uncompressed output match.
- `empty-toc` and `empty-toc-cover-offset` differ on the first body page: the second top-level paragraph is 7.5 PDF points lower in streaming.
- `multi-toc` and `multi-toc-cover-offset` differ on body pages: vertical spacing accumulates between top-level blocks (7.5 PDF points per boundary in the inspected first body page). The TOC pages match.
- All four differing cases have equal page counts and extracted text. These fixtures expose body-layout spacing differences, rather than differences in the inspected page-number text or TOC placement. They were retained as failing visual comparisons until the spacing fix, rather than hidden by a relaxed threshold.

The initial run first exposed differing heading fonts under system discovery; disabling discovery and using normal weights isolated the spacing differences above. No renderer changes were made as part of creating these fixtures.

Comparator self-tests:

```powershell
$env:PYTHONDONTWRITEBYTECODE = '1'
.\target\visual-pagination-venv\Scripts\python.exe -m unittest discover -s core\tests\visual\pagination -p test_compare.py
```

