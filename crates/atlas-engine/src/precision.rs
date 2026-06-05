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

/// Suggested next action based on structural precision tier.
#[allow(dead_code)] // public API, consumed in future phases
pub fn next_action_structural(tier: PrecisionTier) -> Option<&'static str> {
    match tier {
        PrecisionTier::Unavailable => {
            Some("no structural data — run 'atlas index --analysis structural'")
        }
        PrecisionTier::ManifestOnly => {
            Some("manifest-only — run 'atlas index' for full structural data")
        }
        PrecisionTier::LocalDataflowOnly => Some(
            "partial — cross-file references not resolved; run 'atlas index' for full structural data",
        ),
        PrecisionTier::DegradedStructural => {
            Some("budget exceeded — increase LAZY_STRUCTURAL_BUDGET_MS or reduce scope")
        }
        PrecisionTier::PartialExact => {
            Some("structural complete, dataflow truncated — increase budget or narrow scope")
        }
        PrecisionTier::Exact => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_action_covers_all_tiers() {
        // Exact → None (no action needed)
        assert_eq!(next_action_structural(PrecisionTier::Exact), None);
        // All other tiers → Some action
        assert!(next_action_structural(PrecisionTier::Unavailable).is_some());
        assert!(next_action_structural(PrecisionTier::ManifestOnly).is_some());
        assert!(next_action_structural(PrecisionTier::LocalDataflowOnly).is_some());
        assert!(next_action_structural(PrecisionTier::DegradedStructural).is_some());
        assert!(next_action_structural(PrecisionTier::PartialExact).is_some());
    }

    #[test]
    fn next_action_unavailable() {
        let hint = next_action_structural(PrecisionTier::Unavailable).unwrap();
        assert!(hint.contains("no structural data"));
    }

    #[test]
    fn next_action_manifest_only() {
        let hint = next_action_structural(PrecisionTier::ManifestOnly).unwrap();
        assert!(hint.contains("manifest-only"));
    }

    #[test]
    fn next_action_local_dataflow_only() {
        let hint = next_action_structural(PrecisionTier::LocalDataflowOnly).unwrap();
        assert!(hint.contains("cross-file references not resolved"));
    }

    #[test]
    fn next_action_degraded_structural() {
        let hint = next_action_structural(PrecisionTier::DegradedStructural).unwrap();
        assert!(hint.contains("budget exceeded"));
    }

    #[test]
    fn next_action_partial_exact() {
        let hint = next_action_structural(PrecisionTier::PartialExact).unwrap();
        assert!(hint.contains("dataflow truncated"));
    }
}
