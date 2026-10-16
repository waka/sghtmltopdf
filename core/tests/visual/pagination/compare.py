"""Build main separately, render a pagination matrix, and produce a local visual report."""

import argparse
import hashlib
import html
import itertools
import json
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

import pymupdf

FIXTURES = Path(__file__).resolve().parent
ROOT = FIXTURES.parents[3]


def run(args, **kwargs):
    print("+", " ".join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), check=True, **kwargs)


def cases():
    result = []
    for toc, cover, offset, decoration in itertools.product(
        [False, True], [None, "cover.html", "cover-two-pages.html"], [0, 10], ["simple", "html"]
    ):
        result.append(dict(
            name=f"toc-{int(toc)}_cover-{0 if cover is None else 2 if 'two' in cover else 1}_offset-{offset}_{decoration}",
            body="chapters.html", toc=toc, cover=cover, offset=offset, decoration=decoration,
        ))
    for name, body, cover, offset, decoration, extra in [
        ("empty-toc", "no-headings.html", None, 0, "simple", []),
        ("empty-toc-cover-offset", "no-headings.html", "cover-two-pages.html", 10, "html", []),
        ("multi-toc", "many-headings.html", None, 0, "simple", []),
        ("multi-toc-cover-offset", "many-headings.html", "cover-two-pages.html", 98, "html", []),
        ("three-digit-offset-css", "css-counters.html", "cover.html", 998, "css", []),
        ("css-selectors-cover-offset", "css-counters.html", "cover-two-pages.html", 10, "css", []),
        ("disabled-toc-links", "chapters.html", "cover.html", 10, "simple", ["--disable-toc-links"]),
        ("uncompressed", "chapters.html", "cover-two-pages.html", 10, "html", ["--no-pdf-compression"]),
    ]:
        result.append(dict(name=name, body=body, toc=True, cover=cover, offset=offset, decoration=decoration, extra=extra))
    return result


def cli_args(case):
    args = [FIXTURES / case["body"], "--font", ROOT / "core" / "tests" / "fonts" / "DejaVuSans.ttf",
            "--disable-system-fonts", "--page-size", "A5", "--margin-top", "22mm", "--margin-bottom", "22mm",
            "--margin-left", "15mm", "--margin-right", "15mm", "--page-offset", str(case["offset"])]
    if case["toc"]:
        args.append("--toc")
    if case["cover"]:
        args.extend(["--cover", FIXTURES / case["cover"]])
    if case["decoration"] == "html":
        args.extend(["--header-html", FIXTURES / "header.html", "--footer-html", FIXTURES / "footer.html"])
    elif case["decoration"] == "simple":
        args.extend(["--header-left", "VISUAL PAGINATION", "--footer-right", "Page [page] of [topage]"])
    args.extend(case.get("extra", []))
    return args


def build_binaries(output, baseline_ref):
    sha = subprocess.check_output(["git", "rev-parse", baseline_ref], cwd=ROOT, text=True).strip()
    source = output / "baseline-source"
    marker = source / ".visual-baseline-sha"
    if not marker.exists() or marker.read_text() != sha:
        if source.exists():
            shutil.rmtree(source)
        source.mkdir(parents=True)
        archive = output / "baseline.tar"
        try:
            run(["git", "archive", "--format=tar", "--output", archive, sha], cwd=ROOT)
            with tarfile.open(archive) as tar:
                tar.extractall(source, filter="data")
        finally:
            archive.unlink(missing_ok=True)
        marker.write_text(sha)
    baseline_target = output / "baseline-build"
    run(["cargo", "build", "--locked", "--manifest-path", source / "Cargo.toml",
         "--target-dir", baseline_target, "--bin", "sghtmltopdf"], cwd=ROOT)
    current_target = output / "current-build"
    run(["cargo", "build", "--locked", "--target-dir", current_target, "--bin", "sghtmltopdf"], cwd=ROOT)
    suffix = ".exe" if sys.platform == "win32" else ""
    return (baseline_target / "debug" / f"sghtmltopdf{suffix}",
            current_target / "debug" / f"sghtmltopdf{suffix}", sha)


def compare_page(left, right, directory, number, dpi, tolerance):
    images = []
    for label, page in [("main", left), ("streaming", right)]:
        if page is None:
            images.append(None)
            continue
        pix = page.get_pixmap(dpi=dpi, colorspace=pymupdf.csRGB, alpha=False)
        pix.save(directory / f"page-{number:03}-{label}.png")
        images.append(pix)
    a, b = images
    if a is None or b is None or (a.width, a.height) != (b.width, b.height):
        return dict(page=number, equal=False, reason="missing page or different raster dimensions")
    first, second = a.samples, b.samples
    diff = bytearray(len(first))
    changed = 0
    max_delta = 0
    for index in range(0, len(first), 3):
        delta = max(abs(first[index + c] - second[index + c]) for c in range(3))
        max_delta = max(max_delta, delta)
        if delta > tolerance:
            changed += 1
            diff[index:index + 3] = b"\xff\x00\x60"
        else:
            diff[index:index + 3] = b"\xff\xff\xff"
    pymupdf.Pixmap(pymupdf.csRGB, a.width, a.height, bytes(diff), False).save(directory / f"page-{number:03}-diff.png")
    return dict(page=number, equal=changed == 0, changed_pixels=changed,
                changed_percent=round(100 * changed / (a.width * a.height), 6), max_channel_delta=max_delta,
                text_equal=left.get_text() == right.get_text(),
                main_links=len(left.get_links()), streaming_links=len(right.get_links()))


def write_report(output, results, metadata):
    data = dict(metadata=metadata, cases=results)
    (output / "results.json").write_text(json.dumps(data, indent=2), encoding="utf-8")
    passed = sum(item["equal"] for item in results)
    chunks = [f'<!doctype html><meta charset="utf-8"><title>Pagination visual comparison</title><style>'
              'body{font:15px system-ui;margin:24px;background:#edf1f5;color:#182b42}'
              '.pages{display:flex;gap:8px}.pages figure{width:33%;margin:0}.pages img{width:100%;background:white}'
              'section{padding:16px;margin:16px 0;background:white}.pass{color:#187143}.fail{color:#be2645}'
              'a{color:#174d80}summary{cursor:pointer;padding:8px}pre{white-space:pre-wrap}</style>',
              f'<h1>main batch vs current streaming: {passed}/{len(results)} pixel matches</h1>',
              f'<pre>{html.escape(json.dumps(metadata, indent=2))}</pre>',
              '<p>Pink pixels mark differences above the configured per-channel tolerance. '
              'PDF bytes are not compared. Open page images at native size to inspect details.</p>']
    for case in results:
        name = html.escape(case["name"])
        css = "pass" if case["equal"] else "fail"
        chunks.append(f'<section><h2 class="{css}">{name}: {"MATCH" if case["equal"] else "DIFFERENT"}</h2>')
        if "error" in case:
            chunks.append(f'<pre>{html.escape(case["error"])}</pre></section>')
            continue
        chunks.append(f'<p><a href="{name}/main.pdf">main PDF</a> · <a href="{name}/streaming.pdf">streaming PDF</a> · '
                      f'pages: {case["main_pages"]} / {case["streaming_pages"]}</p>')
        for page in case["pages"]:
            n = page["page"]
            chunks.append(f'<details{" open" if not page["equal"] else ""}><summary>Page {n}: '
                          f'{html.escape(json.dumps(page))}</summary><div class="pages">')
            for label in ["main", "streaming", "diff"]:
                path = f'{name}/page-{n:03}-{label}.png'
                if (output / path).exists():
                    chunks.append(f'<figure><figcaption>{label}</figcaption><a href="{path}"><img loading="lazy" src="{path}"></a></figure>')
            chunks.append('</div></details>')
        chunks.append('</section>')
    (output / "index.html").write_text("\n".join(chunks), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-ref", default="main")
    parser.add_argument("--output", type=Path, default=ROOT / "target" / "visual-pagination")
    parser.add_argument("--dpi", type=int, default=96)
    parser.add_argument("--tolerance", type=int, default=0, help="Allowed per-channel pixel delta (default: exact)")
    parser.add_argument("--case", action="append", help="Run only this case name; repeatable")
    parser.add_argument("--skip-build", action="store_true", help="Reuse binaries in the output directory")
    args = parser.parse_args()
    if args.dpi <= 0 or not 0 <= args.tolerance <= 255:
        parser.error("dpi must be positive and tolerance must be between 0 and 255")
    selected = [case for case in cases() if not args.case or case["name"] in args.case]
    if not selected or (args.case and set(args.case) - {case["name"] for case in selected}):
        parser.error("unknown case; available: " + ", ".join(case["name"] for case in cases()))
    output = args.output.resolve()
    if not output.is_relative_to(ROOT / "target"):
        parser.error("output must be under the repository's ignored target directory")
    output.mkdir(parents=True, exist_ok=True)
    if args.skip_build:
        suffix = ".exe" if sys.platform == "win32" else ""
        baseline = output / "baseline-build" / "debug" / f"sghtmltopdf{suffix}"
        current = output / "current-build" / "debug" / f"sghtmltopdf{suffix}"
        sha = (output / "baseline-source" / ".visual-baseline-sha").read_text()
    else:
        baseline, current, sha = build_binaries(output, args.baseline_ref)
    metadata = dict(baseline_ref=args.baseline_ref, baseline_sha=sha,
                    current_sha=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                    current_worktree_status=subprocess.check_output(["git", "status", "--short"], cwd=ROOT, text=True),
                    pymupdf=pymupdf.VersionBind, dpi=args.dpi, tolerance=args.tolerance,
                    baseline_binary_sha256=hashlib.sha256(baseline.read_bytes()).hexdigest(),
                    current_binary_sha256=hashlib.sha256(current.read_bytes()).hexdigest())
    results = []
    for case in selected:
        directory = output / case["name"]
        directory.mkdir(exist_ok=True)
        for old in directory.glob("page-*.png"):
            old.unlink()
        result = dict(name=case["name"], options=case, equal=False)
        try:
            for binary, label, extra in [(baseline, "main", []), (current, "streaming", ["--streaming"])]:
                command = [binary, *cli_args(case), *extra, "-o", directory / f"{label}.pdf"]
                completed = run(command, cwd=ROOT, capture_output=True, text=True)
                (directory / f"{label}.log").write_text(completed.stdout + completed.stderr, encoding="utf-8")
            with pymupdf.open(directory / "main.pdf") as left, pymupdf.open(directory / "streaming.pdf") as right:
                pages = [compare_page(left[i] if i < len(left) else None, right[i] if i < len(right) else None,
                                      directory, i + 1, args.dpi, args.tolerance)
                         for i in range(max(len(left), len(right)))]
                result.update(main_pages=len(left), streaming_pages=len(right), pages=pages,
                              equal=len(left) == len(right) and all(page["equal"] for page in pages))
        except subprocess.CalledProcessError as error:
            result["error"] = f'{error}\n{error.stdout or ""}\n{error.stderr or ""}'
        except (OSError, RuntimeError, ValueError) as error:
            result["error"] = f'{type(error).__name__}: {error}'
        results.append(result)
        write_report(output, results, metadata)
        print(f'{case["name"]}: {"MATCH" if result["equal"] else "DIFFERENT"}', flush=True)
    print(f'Report: {output / "index.html"}', flush=True)
    return 0 if all(case["equal"] for case in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
