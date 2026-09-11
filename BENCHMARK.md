# Benchmarking sghtmltopdf

The benchmark suite lives in `core/benches`. It answers two questions:
*did this change make anything slower*, and *did it change what comes out*.
Every case uses the fonts bundled in `core/tests/fonts` and runs with
`--disable-system-fonts`, so results do not depend on what is installed on
your machine: page counts and PDF sizes come out identical on macOS and on a
Linux runner.

## Quick check before opening a PR

```sh
cargo bench -p sghtmltopdf-core --bench metrics
```

This takes about 30 seconds. It renders every fixture and prints one row per
case with time, peak memory, allocations, page count and PDF size, compared
against `core/benches/baseline.json`. It exits with status 1 when something
regressed, and lists the reasons at the end:

```
FAIL engine/batch/table_pagination: allocs +7.3%
WARN cli/report_toc: time +18.2%
```

Page count and PDF size must not change at all. Allocations may grow by 5%,
peak RSS by 10%. Time warns at 15% and fails at 30%, using the best of
three runs.

## When a change is intentional

A layout fix that moves a page break, or a smaller font subset, will fail the
gate on purpose. Re-record the baseline and commit it with your change so the
diff documents what moved:

```sh
cargo bench -p sghtmltopdf-core --bench metrics -- --save-baseline
```

## In CI

The `bench` job runs the same gate against the committed `baseline.json` on
every push and pull request. Time is measured but not judged there
(`--no-time`): a shared runner swings by hundreds of percent, while the
deterministic numbers are the same as on the machine that recorded the
baseline. Peak RSS is skipped automatically because the baseline comes from a
different OS and architecture.

So a CI failure means the same thing as a local one, and the fix is the same:
re-record the baseline if the change was intentional, and commit it.

The three criterion targets run once each (`--test`) in the same job. That
only checks they still work; their timings are ignored.

## Finding out where the time went

The three criterion targets measure wall-clock time statistically and print
the change against the previous run:

```sh
cargo bench -p sghtmltopdf-core --bench phases      # each pipeline stage, per fixture
cargo bench -p sghtmltopdf-core --bench end_to_end  # Engine API, CLI binary, HTTP server
cargo bench -p sghtmltopdf-core --bench scale       # 1k to 60k elements, batch and streaming
```

A full run of all three takes around ten minutes. Narrow it down with a
regex on the benchmark name:

```sh
cargo bench -p sghtmltopdf-core --bench phases -- 'phase/table_pagination/'
cargo bench -p sghtmltopdf-core --bench phases -- '/layout$'
cargo bench -p sghtmltopdf-core --bench end_to_end -- '^cli/'
```

To compare a branch against `main` rather than against your last run:

```sh
git switch main
cargo bench -p sghtmltopdf-core -- --save-baseline main
git switch my-branch
cargo bench -p sghtmltopdf-core -- --baseline main
```

HTML reports with plots are written to `target/criterion/report/index.html`.

## Adding a fixture

Put an HTML file in `core/benches/fixtures/` and add one line to `FIXTURES`
in `core/benches/support/mod.rs`. Mark it `streaming: false` if it uses
`counter(pages)`, `<body>` backgrounds or borders, or a `<style>` after
`<body>`, and `local_assets: true` if it reads images, CSS or fonts from
disk. Then run `--save-baseline` so the new case has a reference.

## More

`core/benches/README.md` lists every fixture and what it covers, all
options of the metrics gate, and the profiling examples under
`core/examples` for digging into a regression once the benches have pointed
at a stage.
