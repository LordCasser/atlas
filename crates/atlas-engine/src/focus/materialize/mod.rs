//! Focus materialize — on-demand fact building under the Focus solution.
//!
//! **Product narrative:** query-time analysis is **Focus**; this module is the
//! internal structural + dataflow ensure stack. Index remains the separate
//! simple pre-materialization path.
//!
//! **Mechanism names:** `LazyDataflowService` / `LazyStructuralService` keep the
//! CS meaning of deferred evaluation (ensure when queried). They are not a
//! third product path; see `docs/architecture.md` §2.1.1 naming table.
//!
//! All production paths for a given project must use [`FocusMaterialize::open`]
//! so structural self-heal rebuilder and services share one configuration.
//! [`Clone`] is cheap and shares the same inner services via [`Arc`].

mod structural;

use std::path::PathBuf;
use std::sync::Arc;

use db::Store;

// Dataflow ensure implementation (crate formerly named `lazy`).
pub use focus_materialize::LazyDataflowService;

pub use structural::{
    CandidateProvider, DefaultCandidateProvider, EnsureStructuralResult, LazyStructuralService,
    rebuild_structural_for_file,
};

/// Shared inner stack (one configuration identity for all clones).
struct FocusMaterializeInner {
    structural: LazyStructuralService,
    dataflow: LazyDataflowService,
}

/// Single Focus-owned materialize stack for one store / project root.
///
/// Construct with [`FocusMaterialize::open`]. Cloning shares the same
/// structural and dataflow services (including rebuilder) via [`Arc`].
#[derive(Clone)]
pub struct FocusMaterialize {
    inner: Arc<FocusMaterializeInner>,
}

impl FocusMaterialize {
    /// Open the sole materialize configuration for `store` + optional root.
    ///
    /// Wires structural self-heal into dataflow exactly once.
    pub fn open(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        let structural = LazyStructuralService::new(store.clone(), project_root.clone());
        let store_for_rebuild = store.clone();
        let root_for_rebuild = project_root.clone();
        // `with_structural_rebuilder` is the only public dataflow constructor;
        // always requires a rebuilder (unconfigured services are unrepresentable).
        let dataflow = LazyDataflowService::with_structural_rebuilder(
            store,
            project_root,
            Arc::new(move |file_id| {
                rebuild_structural_for_file(
                    &store_for_rebuild,
                    root_for_rebuild.as_deref(),
                    &file_id,
                )
            }),
        );
        Self {
            inner: Arc::new(FocusMaterializeInner {
                structural,
                dataflow,
            }),
        }
    }

    /// Structural ensure service (Focus materialize).
    pub fn structural(&self) -> &LazyStructuralService {
        &self.inner.structural
    }

    /// Dataflow ensure service (Focus materialize; rebuilder already set).
    pub fn dataflow(&self) -> &LazyDataflowService {
        &self.inner.dataflow
    }

    /// Always `true` after [`open`]: rebuilder is wired at construction.
    /// Audit probe only — see [`LazyDataflowService::has_structural_rebuilder`].
    pub fn has_structural_rebuilder(&self) -> bool {
        self.inner.dataflow.has_structural_rebuilder()
    }

    /// Shared-stack pointer equality helper (tests / audits).
    pub fn same_stack_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

/// Build from pre-built services (unit tests with custom candidate providers).
#[cfg(test)]
pub fn from_parts_for_test(
    structural: LazyStructuralService,
    dataflow: LazyDataflowService,
) -> FocusMaterialize {
    FocusMaterialize {
        inner: Arc::new(FocusMaterializeInner {
            structural,
            dataflow,
        }),
    }
}

/// Dataflow with no-op rebuilder for unit tests that never self-heal.
#[cfg(test)]
pub fn dataflow_for_test(store: Arc<Store>, project_root: Option<PathBuf>) -> LazyDataflowService {
    LazyDataflowService::with_structural_rebuilder(store, project_root, Arc::new(|_id| Ok(())))
}

/// Path helper for tests.
#[cfg(test)]
pub fn open_in_memory() -> (Arc<Store>, FocusMaterialize) {
    let store = Store::open_in_memory().expect("in-memory store");
    store.init_schema().expect("schema");
    let store = Arc::new(store);
    let m = FocusMaterialize::open(store.clone(), None);
    (store, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_wires_rebuilder_and_clones_share_stack() {
        let (_store, m) = open_in_memory();
        let cloned = m.clone();
        // Real invariant: Clone shares one Arc stack (not a second materialize shell).
        assert!(
            m.same_stack_as(&cloned),
            "Clone must share Arc inner (same stack identity)"
        );
        assert!(std::ptr::eq(
            Arc::as_ptr(m.structural().store()),
            Arc::as_ptr(m.dataflow().store()),
        ));
        assert!(std::ptr::eq(
            m.structural() as *const _,
            cloned.structural() as *const _,
        ));
        assert!(std::ptr::eq(
            m.dataflow() as *const _,
            cloned.dataflow() as *const _,
        ));
    }

    #[test]
    fn open_with_root_sets_project_root_on_services() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let store = Arc::new(store);
        let root = PathBuf::from("/tmp/atlas-focus-mat-test");
        let m = FocusMaterialize::open(store, Some(root.clone()));
        assert_eq!(m.structural().project_root(), Some(root.as_path()));
        assert_eq!(m.dataflow().project_root(), Some(root.as_path()));
    }
}
