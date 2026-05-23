//! Capability tool: lists per-language analysis profiles with supported
//! features, limitations, and confidence floors.

use atlas_engine::LanguageCapabilityProfile;

use super::ToolRouter;

use serde_json::{Value, json};

impl ToolRouter {
    pub(crate) fn handle_language_capabilities(&self) -> (String, bool) {
        let profiles = LanguageCapabilityProfile::all_compiled();
        let caps: Vec<Value> = profiles
            .iter()
            .map(|p| {
                let mut cap = json!({
                    "language": p.language,
                    "capability_level": p.capability_level.as_str(),
                    "supported_features": p.supported_features,
                    "unsupported_features": p.unsupported_features,
                    "limitations": p.limitations,
                    "confidence_floor": p.confidence_floor,
                });
                // Include the fine-grained FeatureMatrix if available
                if let Some(ref features) = p.features {
                    cap["features"] = serde_json::to_value(features).unwrap_or(Value::Null);
                }
                cap
            })
            .collect();
        (
            serde_json::to_string(&json!({
                "language_count": caps.len(),
                "profiles": caps,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
