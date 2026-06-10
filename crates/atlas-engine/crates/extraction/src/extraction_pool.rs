//! Extraction-specific rayon thread pool with enlarged stack size.
//!
//! Tree-sitter recursive-descent parsing combined with recursive CFG builder
//! (`walk_block`) and DataFlow builder traversals can consume stack depths
//! that exceed rayon's default 2 MiB worker stack. This module provides a
//! pre-configured thread pool with 8 MiB stacks for extraction workloads.

use std::sync::LazyLock;

/// Stack size for extraction worker threads, in bytes (8 MiB).
///
/// The default rayon thread stack (2 MiB) is insufficient for
/// `--analysis full` mode parsing of deeply nested source files.
/// 8 MiB matches the main-thread default on macOS/Linux and provides
/// a 4× safety margin over observed peak usage.
pub const EXTRACTION_STACK_SIZE: usize = 8 * 1024 * 1024;

/// Default number of threads for the extraction pool.
/// Matches rayon's default (logical CPU count).
pub fn extraction_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Lazily-initialized global extraction pool.
///
/// Using `LazyLock` avoids `OnceCell` pitfalls and ensures the pool is
/// built exactly once, on first use, with the configured stack size.
/// All extraction, resolution, and graph-building phases that use rayon
/// `par_iter()` should install this pool via `pool.install(|| { ... })`.
pub static EXTRACTION_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    rayon::ThreadPoolBuilder::new()
        .stack_size(EXTRACTION_STACK_SIZE)
        .num_threads(extraction_worker_count())
        .thread_name(|idx| format!("atlas-extract-{idx}"))
        .build()
        .expect("Failed to build extraction thread pool")
});

/// Returns a reference to the lazily-initialized global extraction pool.
#[inline]
pub fn extraction_pool() -> &'static rayon::ThreadPool {
    &EXTRACTION_POOL
}

#[cfg(test)]
mod tests {
    use rayon::prelude::*;

    use super::*;

    #[test]
    fn test_extraction_stack_size_is_8_mib() {
        assert_eq!(EXTRACTION_STACK_SIZE, 8 * 1024 * 1024);
    }

    #[test]
    fn test_pool_is_built() {
        // Accessing EXTRACTION_POOL triggers lazy initialization
        let pool = &EXTRACTION_POOL;
        // Verify we can install and run work
        pool.install(|| {
            let result: Vec<usize> = (0..10).into_par_iter().map(|x| x * 2).collect();
            assert_eq!(result, vec![0, 2, 4, 6, 8, 10, 12, 14, 16, 18]);
        });
    }

    #[test]
    fn test_pool_threads_are_named() {
        let pool = &EXTRACTION_POOL;
        let names = std::sync::Mutex::new(Vec::new());
        pool.install(|| {
            (0..pool.current_num_threads())
                .into_par_iter()
                .for_each(|_| {
                    if let Some(name) = std::thread::current().name() {
                        names.lock().unwrap().push(name.to_string());
                    }
                });
        });
        let names = names.lock().unwrap();
        for name in names.iter() {
            assert!(
                name.starts_with("atlas-extract-"),
                "unexpected thread name: {name}"
            );
        }
    }
}
