use example::{fib, sum_to};

#[test]
fn test_fib_base() {
    assert_eq!(fib(0), 0);
    assert_eq!(fib(1), 1);
}

#[test]
fn test_fib_10() {
    assert_eq!(fib(10), 55);
}

#[test]
fn test_sum_to_100() {
    assert_eq!(sum_to(100), 4950);
}

// Exercises real thread spawning + atomics. wasm32-wasip1 and
// wasm32-wasip1-threads share `target_feature`s, so we can't `cfg`-gate this.
// The test is opt-in via the `threads` feature flag; the runner integration
// test enables it when building for the threads target.
#[cfg(feature = "threads")]
#[test]
fn test_threads_can_compute() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;

    let counter = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let c = counter.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                c.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for h in handles {
        h.join().expect("thread should join");
    }
    assert_eq!(counter.load(Ordering::Relaxed), 4 * 1000);
}
