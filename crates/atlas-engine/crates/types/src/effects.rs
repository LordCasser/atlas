//! Semantic effect types — language-agnostic resource-effect abstractions.
//!
//! These types describe what a statement does to resources (allocate, free,
//! store, assign, etc.) without depending on any specific language semantics.
//! Language-specific rules are injected via the `OwnershipContract` trait.
//!
//! # Architecture
//! - `SemanticEffect` lives on `CfgNode.semantic_effects` as a multi-effect vector.
//! - `OwnershipContract` is implemented per-language and injected into analysis.
//! - All types are serializable and deterministically identifiable.

use serde::{Deserialize, Serialize};

use super::ids::{CfgNodeId, EffectId};

// ==================== SemanticEffect ====================

/// A single semantic effect produced by a statement in the control-flow graph.
///
/// Unlike the legacy single `effect_kind` per node, a single statement can
/// produce multiple effects (e.g., `p = alloc(); field = p` produces both
/// an Alloc and a Store).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEffect {
    pub id: EffectId,
    /// The CFG node that produced this effect.
    pub cfg_node_id: CfgNodeId,
    /// Ordering within the node (for statements with multiple effects).
    pub order: u32,
    pub kind: SemanticEffectKind,
    /// Confidence score [0.0, 1.0] — how certain we are about this effect.
    pub confidence: f64,
}

// Semantic identity is determined by id alone; confidence is an annotation
// that does not affect equality.  The derived PartialEq would fail because
// f64 does not implement Eq; we implement PartialEq manually to exclude
// confidence from structural comparison.
impl PartialEq for SemanticEffect {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.cfg_node_id == other.cfg_node_id
            && self.order == other.order
            && self.kind == other.kind
    }
}

impl Eq for SemanticEffect {}

/// The kind of resource effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SemanticEffectKind {
    /// Resource allocation (return value becomes owned resource).
    /// e.g., `p = malloc(N)`, `conn = createConnection()`
    Alloc { target: PlaceRef, callee: String },
    /// Resource release / free.
    /// e.g., `free(p)`, `conn.close()`
    Free { place: PlaceRef, callee: String },
    /// Write a value into a field.
    /// e.g., `obj->field = value`
    Store { dst: PlaceRef, src: ValueSource },
    /// Assign a value to a local variable.
    /// e.g., `local = expr`
    Assign { dst: PlaceRef, src: ValueSource },
    /// General function call (no tracked resource effect identified).
    Call { callee: String },
    /// Nullify / zero out a place.
    Nullify { place: PlaceRef },
    /// Return a value from the function.
    Return { value: ValueSource },
    /// A value escapes the local scope (e.g., stored in a global, passed
    /// by pointer to an external function, or moved to another thread).
    Escape { value: ValueSource, to: EscapeTarget },
}

// ==================== PlaceRef ====================

/// A location (place) that can hold a resource — a field, a local variable,
/// or an indeterminate location.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaceRef {
    /// A named field access path, canonicalized.
    /// e.g., `"data->state.aptr.cookiehost"`, `"self.connection"`
    Field { path: String },
    /// A local variable.
    Local { name: String },
    /// Cannot be resolved to a concrete place.
    Indeterminate,
}

// ==================== ValueSource ====================

/// Where a value comes from — the source of data flowing into a Store or Assign.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ValueSource {
    /// Return value of a function call.
    CallReturn { callee: String },
    /// A function parameter.
    Param { name: String },
    /// A local variable.
    Local { name: String },
    /// A literal null / zero value.
    LiteralNull,
    /// Source cannot be determined.
    Unknown,
}

// ==================== EscapeTarget ====================

/// Destination when a value escapes the local scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EscapeTarget {
    Global,
    Argument,
    ReturnValue,
    Thread,
    Unknown,
}

// ==================== OwnershipContract ====================

/// Language-agnostic resource ownership contract.
///
/// Each language provides its own implementation that tells the analysis
/// engine:
///  1. Whether a call returns an owned resource (producer).
///  2. Whether a call consumes/releases a resource (consumer).
///
/// This trait is defined in `types` so that both `analysis` and `extraction`
/// can reference it without crate-level dependency cycles.
pub trait OwnershipContract: Send + Sync {
    /// Check whether a call returns a resource and with what ownership semantics.
    fn classify_return(&self, callee: &str) -> Option<ReturnContract>;

    /// Check whether a call consumes/releases a resource, with location
    /// and consumption style.
    fn classify_consumption(&self, callee: &str) -> Option<ConsumptionContract>;
}

// ==================== ReturnContract ====================

/// Ownership semantics of a function's return value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnContract {
    /// Caller owns the return value and must release it.
    /// e.g., `malloc`, `fopen`, `Box::new`, `os.Open`
    NewOwned,
    /// Caller borrows the return value and must NOT release it.
    /// e.g., `getenv`, `&self.field`
    Borrowed,
    /// May or may not be owned depending on context.
    /// e.g., `realloc(NULL, N)` produces owned, `realloc(ptr, N)` may reuse.
    MaybeOwned,
    /// Returns NULL (no resource) or a non-null owned resource.
    /// e.g., many C allocation wrappers.
    NullOrOwned,
    /// Ownership cannot be determined from callee name alone.
    Unknown,
}

// ==================== ConsumptionContract ====================

/// How a resource is consumed/released in a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsumptionContract {
    /// Where the resource appears in the call syntax.
    pub resource: ResourceLocator,
    /// The syntactic style of consumption (free-function, method, implicit, etc.).
    /// NOTE: this is per-contract, not per-language — a single language
    /// can mix multiple styles (e.g., C has both `free(ptr)` and implicit scope exit).
    pub style: ConsumptionStyle,
    /// Match confidence [0.0, 1.0].
    pub confidence: f64,
}

// ==================== ResourceLocator ====================

/// Where a resource appears in a call's argument list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceLocator {
    /// The return value is the resource.
    ReturnValue,
    /// An argument at a specific index is the resource.
    /// e.g., `free(ptr)` → Argument { index: 0 }
    Argument { index: usize },
    /// The receiver (this/self) is the resource.
    /// e.g., `conn.close()` → Receiver
    Receiver,
    /// An out-param (pointer argument) receives the resource.
    /// e.g., `fopen_s(&fp, ...)` → OutParam { index: 0 }
    OutParam { index: usize },
    /// The resource is released implicitly at scope exit.
    /// e.g., Rust Drop, C++ destructor.
    ImplicitScopeExit,
}

// ==================== ConsumptionStyle ====================

/// The syntactic pattern through which a resource is consumed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConsumptionStyle {
    /// Free-function style: `free(ptr)`, `fclose(fp)`
    ExplicitCall,
    /// Method-call style: `obj.close()`, `conn.dispose()`
    MethodCall,
    /// Compiler-generated: Rust Drop, C++ destructor
    Implicit,
    /// Go-style defer: `defer f.Close()`
    Deferred,
    /// Python-style context manager: `with open() as f:`
    ContextManaged,
}
