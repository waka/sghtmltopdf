//! Scale series: synthetic documents of 1k to 60k elements rendered through
//! the engine in batch and streaming mode. Throughput is reported per
//! element, so anything worse than linear shows up as falling throughput.
//!
//! Run: `cargo bench --bench scale`
//! One size: `cargo bench --bench scale -- '/20000$'`

mod support;

use std::time::Duration;

use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use sghtmltopdf_core::engine::Mode;

use support::{render, SCALE_KINDS};

fn scale(c: &mut Criterion) {
    for kind in SCALE_KINDS {
        let mut group = c.benchmark_group(format!("scale/{}", kind.name));
        group.sampling_mode(SamplingMode::Flat);
        group.sample_size(10);
        group.warm_up_time(Duration::from_millis(500));
        group.measurement_time(Duration::from_secs(3));

        for &count in kind.sizes {
            let html = (kind.generate)(count);
            group.throughput(Throughput::Elements(count as u64));
            for (mode_name, mode) in [("batch", Mode::Batch), ("streaming", Mode::Streaming)] {
                group.bench_with_input(BenchmarkId::new(mode_name, count), &html, |b, html| {
                    b.iter(|| render(html.as_bytes(), mode))
                });
            }
        }
        group.finish();
    }
}

criterion_group!(benches, scale);
criterion_main!(benches);
