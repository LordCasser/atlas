//! Domain rules management tools: annotate, list, learn.

use super::{ToolRouter, get_str};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_atlas_annotate(&self, args: &serde_json::Value) -> (String, bool) {
        let language = get_str(args, "language");
        let rule_kind = get_str(args, "rule_kind");
        let pattern = get_str(args, "pattern");
        let confidence = args
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0);

        if rule_kind.is_empty() || pattern.is_empty() {
            return ("Missing rule_kind or pattern".to_string(), true);
        }

        let language = if language.is_empty() { "c" } else { language };

        let valid_kinds = ["free_fn", "alloc_fn", "owned_pattern", "cleanup_fn"];
        if !valid_kinds.contains(&rule_kind) {
            return (
                format!(
                    "Invalid rule_kind '{}'. Must be one of: {}",
                    rule_kind,
                    valid_kinds.join(", ")
                ),
                true,
            );
        }

        match self.project().overlay_runtime.upsert_domain_rule(
            language, rule_kind, pattern, "exact", "user", "enabled", confidence, None,
        ) {
            Ok(id) => {
                let resp = json!({
                    "ok": true,
                    "rule_id": id,
                    "language": language,
                    "rule_kind": rule_kind,
                    "pattern": pattern,
                    "source": "user",
                    "confidence": confidence,
                });
                (
                    serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            Err(e) => (format!("Failed to save rule: {e}"), true),
        }
    }

    pub(crate) fn handle_atlas_domain_rules(&self, args: &serde_json::Value) -> (String, bool) {
        let action = get_str(args, "action");
        let rule_id = get_str(args, "rule_id");
        let source = get_str(args, "source");
        let language = get_str(args, "language");
        let status = get_str(args, "status");

        match action {
            "delete" => {
                if rule_id.is_empty() {
                    return ("Missing rule_id for delete action".to_string(), true);
                }
                match self.project().overlay_runtime.delete_domain_rule(rule_id) {
                    Ok(true) => {
                        let resp = json!({"ok": true, "deleted": rule_id});
                        (
                            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                            false,
                        )
                    }
                    Ok(false) => (format!("Rule not found: {rule_id}"), true),
                    Err(e) => (format!("Failed to delete rule: {e}"), true),
                }
            }
            _ => {
                // Default: list with optional filters
                let lang_filter = if language.is_empty() {
                    None
                } else {
                    Some(language)
                };
                let status_filter = if status.is_empty() {
                    None
                } else {
                    Some(status)
                };
                match self
                    .project()
                    .store
                    .list_domain_rules(lang_filter, status_filter)
                {
                    Ok(rules) => {
                        let items: Vec<_> = rules
                            .iter()
                            .filter(|r| source.is_empty() || r.source == source)
                            .map(|r| {
                                json!({
                                    "id": r.id,
                                    "language": r.language,
                                    "source": r.source,
                                    "rule_kind": r.rule_kind,
                                    "pattern": r.pattern,
                                    "pattern_kind": r.pattern_kind,
                                    "status": r.status,
                                    "confidence": r.confidence,
                                    "created_at": r.created_at,
                                    "updated_at": r.updated_at,
                                })
                            })
                            .collect();
                        let resp = json!({
                            "ok": true,
                            "total": items.len(),
                            "rules": items,
                        });
                        (
                            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                            false,
                        )
                    }
                    Err(e) => (format!("Failed to list rules: {e}"), true),
                }
            }
        }
    }

    pub(crate) fn handle_atlas_rule_learn(&self, args: &serde_json::Value) -> (String, bool) {
        let min_confidence = args
            .get("min_confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);

        // Delegate to the C learning strategy from domain_rules crate
        use atlas_engine::rule_engine::kinds::c::CLearningStrategy;
        use atlas_engine::rule_engine::learning::RuleLearningStrategy;

        let learner = CLearningStrategy;
        match learner.discover_candidates(&self.project().store) {
            Ok(candidates) => {
                let filtered: Vec<_> = candidates
                    .iter()
                    .filter(|c| c.confidence >= min_confidence)
                    .map(|c| {
                        json!({
                            "function_name": c.pattern,
                            "rule_kind": c.rule_kind,
                            "usage_count": c.usage_count,
                            "confidence": c.confidence,
                        })
                    })
                    .collect();
                let resp = json!({
                    "ok": true,
                    "candidates": filtered,
                    "message": "Review candidates and use domain_rules(action='add') to approve them; learned rules are not applied automatically.",
                });
                (
                    serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            Err(e) => (format!("Rule learning failed: {e}"), true),
        }
    }
}
