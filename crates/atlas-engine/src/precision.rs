use types::structs::precision::PrecisionTier;

/// Compute the precision tier for structural lazy extraction.
pub fn structural_precision(built: usize, cached: usize, budget_exceeded: bool) -> PrecisionTier {
    if built == 0 && cached == 0 {
        if budget_exceeded {
            // budget exceeded before any file was built
            PrecisionTier::ManifestOnly
        } else {
            PrecisionTier::Unavailable
        }
    } else if budget_exceeded {
        if built == 0 {
            // budget exceeded before any file was built (but some were cached)
            PrecisionTier::ManifestOnly
        } else {
            // some files built, but budget exceeded mid-build
            PrecisionTier::DegradedStructural
        }
    } else {
        PrecisionTier::Exact
    }
}

/// Compute precision tier for lazy dataflow extraction.
#[allow(dead_code)] // public API, consumed by MCP/CLI consumers
pub fn dataflow_precision(built: usize, planned: usize, budget_exceeded: bool) -> PrecisionTier {
    if planned == 0 {
        PrecisionTier::Unavailable
    } else if built == 0 {
        if budget_exceeded {
            PrecisionTier::ManifestOnly // planned but nothing built due to budget
        } else {
            PrecisionTier::Unavailable
        }
    } else if budget_exceeded && built < planned {
        PrecisionTier::PartialExact // some units built but not all
    } else {
        PrecisionTier::Exact // all units built
    }
}

/// Suggested next action based on structural precision tier.
#[allow(dead_code)] // public API, consumed in future phases
pub fn next_action_structural(tier: PrecisionTier) -> Option<&'static str> {
    match tier {
        PrecisionTier::Unavailable => Some("no structural data — run 'atlas index --analysis structural'"),
        PrecisionTier::DegradedStructural => Some("budget exceeded — increase LAZY_STRUCTURAL_BUDGET_MS or reduce scope"),
        _ => None,
    }
}
