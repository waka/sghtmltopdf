//! Per-phase benchmarks: each stage of the pipeline (parse, stylesheet
//! extraction, cascade, box tree, layout, pagination, PDF encoding) timed in
//! isolation for every fixture in the corpus, with the stage's inputs built
//! once outside the timed loop.
//!
//! Run everything: `cargo bench --bench phases`
//! One fixture:    `cargo bench --bench phases -- 'phase/flexbox/'`
//! One stage:      `cargo bench --bench phases -- '/layout$'`

mod support;

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, SamplingMode, Throughput};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{build_box_tree, layout_document, paginate, PageSettings};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, user_agent_stylesheet};

use support::{author_stylesheet, font_collection, FIXTURES};

/// Costs paid once per conversion regardless of the document.
fn startup(c: &mut Criterion) {
    let mut group = c.benchmark_group("startup");
    group.sampling_mode(SamplingMode::Flat);
    group.bench_function("ua_stylesheet", |b| b.iter(user_agent_stylesheet));
    let empty = sghtmltopdf_core::style::parse_stylesheet("");
    group.bench_function("load_fonts", |b| {
        b.iter(|| font_collection(black_box(&empty)))
    });
    group.finish();
}

fn phases(c: &mut Criterion) {
    let ua = user_agent_stylesheet();
    let settings = PageSettings::default();

    for fixture in FIXTURES {
        let html = fixture.html();
        let mut group = c.benchmark_group(format!("phase/{}", fixture.name));
        group.sampling_mode(SamplingMode::Flat);
        group.throughput(Throughput::Bytes(html.len() as u64));

        group.bench_function("parse", |b| b.iter(|| html::parse(black_box(&html))));
        let dom = html::parse(&html);

        group.bench_function("stylesheet", |b| {
            b.iter(|| author_stylesheet(black_box(&dom)))
        });
        let author = author_stylesheet(&dom);

        // Only fixtures with `@font-face` pay for anything beyond the bundled
        // set, so the font phase is skipped elsewhere to keep the run short.
        if !author.font_faces.is_empty() {
            group.bench_function("font_faces", |b| {
                b.iter(|| font_collection(black_box(&author)))
            });
        }
        let fonts = font_collection(&author);

        group.bench_function("cascade", |b| {
            b.iter(|| compute_styles(black_box(&dom), &ua, &author))
        });
        let styles = compute_styles(&dom, &ua, &author);

        group.bench_function("box_tree", |b| {
            b.iter(|| build_box_tree(black_box(&dom), &styles))
        });
        let tree = build_box_tree(&dom, &styles);

        group.bench_function("layout", |b| {
            b.iter(|| layout_document(black_box(&tree), &styles, &fonts, settings.content_width()))
        });
        let laid_out = layout_document(&tree, &styles, &fonts, settings.content_width());

        // Pagination consumes the laid-out tree, so each iteration gets a copy.
        group.bench_function("paginate", |b| {
            b.iter_batched(
                || laid_out.clone(),
                |mut laid_out| paginate(&mut laid_out, settings.content_height()),
                BatchSize::LargeInput,
            )
        });
        let pages = paginate(&mut laid_out.clone(), settings.content_height());

        group.bench_function("encode_pdf", |b| {
            b.iter(|| {
                encode_pdf(
                    black_box(&pages),
                    &styles,
                    &HashMap::new(),
                    &fonts,
                    &settings,
                )
            })
        });

        group.finish();
    }
}

fn config() -> Criterion {
    Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_millis(1500))
}

criterion_group! {
    name = benches;
    config = config();
    targets = startup, phases
}
criterion_main!(benches);
