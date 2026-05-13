//! Code under test for the example bench.

pub fn fib(n: u32) -> u64 {
    match n {
        0 => 0,
        1 => 1,
        _ => fib(n - 1) + fib(n - 2),
    }
}

pub fn sum_to(n: u32) -> u64 {
    let mut s: u64 = 0;
    for i in 0..n {
        s = s.wrapping_add(i as u64);
    }
    s
}

/// Compute-heavy per-element transform. The inner loop is just an LCG-style
/// stew so the optimizer can't fold it away, and the function has no shared
/// state — perfect for parallel reduction.
#[inline]
pub fn busy(x: u32) -> u64 {
    let mut acc: u64 = (x as u64) ^ 0xdead_beef_cafe_d00d;
    for _ in 0..200 {
        acc = acc
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(x as u64 | 1);
    }
    acc
}

/// Sequential map-reduce: `(0..n).map(busy).sum()`.
pub fn busy_sum_serial(n: u32) -> u64 {
    (0..n).map(busy).fold(0u64, u64::wrapping_add)
}

/// Same reduction, but split across the rayon thread pool. Only available
/// when the crate is built with `--features rayon` (and only meaningful on
/// a threaded target like `wasm32-wasip1-threads`).
#[cfg(feature = "rayon")]
pub fn busy_sum_rayon(n: u32) -> u64 {
    use rayon::prelude::*;
    (0..n)
        .into_par_iter()
        .map(busy)
        .reduce(|| 0u64, u64::wrapping_add)
}
