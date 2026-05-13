use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use example::{busy_sum_serial, fib, sum_to};

fn bench_fib(c: &mut Criterion) {
    c.bench_function("fib 10", |b| b.iter(|| fib(black_box(10))));
    c.bench_function("fib 15", |b| b.iter(|| fib(black_box(15))));
}

fn bench_fib_param(c: &mut Criterion) {
    let mut group = c.benchmark_group("fib_param");
    for n in [5u32, 10, 15] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| fib(black_box(n)))
        });
    }
    group.finish();
}

fn bench_sum(c: &mut Criterion) {
    c.bench_function("sum_to 1000", |b| b.iter(|| sum_to(black_box(1000))));
}

/// Serial vs parallel reduction. With `--features rayon` on a threaded
/// target, the rayon variant should win on machines with multiple cores.
fn bench_busy_sum(c: &mut Criterion) {
    let mut group = c.benchmark_group("busy_sum");
    let n: u32 = 10_000;
    group.bench_function("serial", |b| b.iter(|| busy_sum_serial(black_box(n))));
    #[cfg(feature = "rayon")]
    group.bench_function("rayon", |b| {
        b.iter(|| example::busy_sum_rayon(black_box(n)))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_fib,
    bench_fib_param,
    bench_sum,
    bench_busy_sum
);
criterion_main!(benches);
