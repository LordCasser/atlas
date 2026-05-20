//! Taint analysis types: rules, findings, and path steps.
//!
//! # Design
//!
//! Taint analysis uses the P3 DataNode/DataFlowEdge infrastructure. Sources and
//! sinks are identified by matching rule patterns against symbol names. Taint
//! flows are traced through DataFlowEdges (intraprocedural) with call-graph
//! bridging (interprocedural).

use serde::{Deserialize, Serialize};
use crate::types::enums::{Confidence, Language};
use crate::types::ids::{DataFlowEdgeId, DataNodeId, FileId};
use crate::types::structs::TextRange;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

// ── TaintFindingId ─────────────────────────────────────────────────────────

/// Deterministic taint finding identifier.
///
/// blake3(rule_id + source_node + sink_node + file_id)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaintFindingId(pub [u8; 32]);

impl TaintFindingId {
    pub fn generate(
        rule_id: &str,
        source_node: &DataNodeId,
        sink_node: &DataNodeId,
        file_id: &FileId,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(rule_id.as_bytes());
        hasher.update(&[0]);
        hasher.update(source_node.as_bytes());
        hasher.update(&[0]);
        hasher.update(sink_node.as_bytes());
        hasher.update(&[0]);
        hasher.update(file_id.as_bytes());
        let hash = hasher.finalize();
        Self(hash.into())
    }

    pub fn as_bytes(&self) -> &[u8] { &self.0 }

    pub fn to_hex(&self) -> String { hex::encode(&self.0) }
}

impl ToSql for TaintFindingId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.as_slice()))
    }
}

impl FromSql for TaintFindingId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Blob(bytes) => {
                let arr: [u8; 32] = bytes.try_into().map_err(|_| {
                    FromSqlError::InvalidBlobSize {
                        expected_size: 32,
                        blob_size: bytes.len(),
                    }
                })?;
                Ok(Self(arr))
            }
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

// ── Severity ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Info => "info",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "critical" => Some(Severity::Critical),
            "high" => Some(Severity::High),
            "medium" => Some(Severity::Medium),
            "low" => Some(Severity::Low),
            "info" => Some(Severity::Info),
            _ => None,
        }
    }
}

// ── TaintRuleKind ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaintRuleKind {
    Source,
    Sink,
    Sanitizer,
    Propagator,
}

impl TaintRuleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaintRuleKind::Source => "source",
            TaintRuleKind::Sink => "sink",
            TaintRuleKind::Sanitizer => "sanitizer",
            TaintRuleKind::Propagator => "propagator",
        }
    }
}

// ── TaintRule ───────────────────────────────────────────────────────────────

/// A taint rule defining a source, sink, sanitizer, or propagator.
///
/// Rules are loaded from YAML (`.atlas/rules/*.yaml`) and used by the
/// TaintEngine to identify tainted data-flows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintRule {
    /// Unique rule identifier (e.g. "express.req.query")
    pub id: String,

    /// Target language filter (None = applies to all)
    pub language: Option<Language>,

    /// Rule kind: source, sink, sanitizer, or propagator
    #[serde(rename = "kind")]
    pub kind: TaintRuleKind,

    /// Callee name pattern to match (e.g. "child_process.exec", "os.system")
    #[serde(default)]
    pub callee: Option<String>,

    /// Symbol name pattern to match (e.g. "Request", "request.args")
    #[serde(default)]
    pub symbol_pattern: Option<String>,

    /// Access path pattern for field-level matching (e.g. "*.query.*")
    #[serde(default)]
    pub access_path_pattern: Option<String>,

    /// Argument index for sink/propagator (0-indexed)
    #[serde(default)]
    pub argument_index: Option<u32>,

    /// Whether the rule applies to return values
    #[serde(default)]
    pub applies_to_return: bool,

    /// Severity for this rule (default: Medium for sinks, Low for sources)
    #[serde(default = "default_severity")]
    pub severity: Severity,
}

fn default_severity() -> Severity { Severity::Medium }

// ── TaintFinding ────────────────────────────────────────────────────────────

/// A detected taint flow from source to sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintFinding {
    /// Deterministic finding ID
    pub id: TaintFindingId,

    /// Source DataNode (where taint originates)
    pub source_node: DataNodeId,

    /// Sink DataNode (where taint reaches a dangerous operation)
    pub sink_node: DataNodeId,

    /// The rule that triggered this finding
    pub rule_id: String,

    /// Severity (from rule or default)
    pub severity: Severity,

    /// Confidence in this finding (0.0–1.0)
    pub confidence: Confidence,

    /// File where the finding was discovered
    pub file_id: FileId,
}

// ── TaintPathStep ───────────────────────────────────────────────────────────

/// A single step in a taint path from source to sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintPathStep {
    /// Which finding this step belongs to
    pub finding_id: TaintFindingId,

    /// Step index (0 = source, N = sink)
    pub index: u32,

    /// The DataNode at this step
    pub data_node: DataNodeId,

    /// The DataFlowEdge leading to this step (None for source node)
    pub edge_id: Option<DataFlowEdgeId>,

    /// File where this step occurs
    pub file_id: FileId,

    /// Source range in the file
    pub range: TextRange,

    /// Human-readable description (e.g. "req.query → req.query.name → name → sink")
    pub message: String,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::{DataNodeId, FileId};
    use crate::types::enums::Language;

    fn make_source_node(file_id: &FileId) -> DataNodeId {
        DataNodeId::generate(file_id, None, "parameter", Some("query"), None, 0)
    }

    fn make_sink_node(file_id: &FileId) -> DataNodeId {
        DataNodeId::generate(file_id, None, "call_arg", Some("exec_arg"), None, 100)
    }

    #[test]
    fn test_taint_finding_id_deterministic() {
        let file1 = FileId::generate("app.ts");
        let source = make_source_node(&file1);
        let sink = make_sink_node(&file1);

        let id1 = TaintFindingId::generate("test.rule", &source, &sink, &file1);
        let id2 = TaintFindingId::generate("test.rule", &source, &sink, &file1);
        assert_eq!(id1, id2, "Same inputs must produce same ID");
    }

    #[test]
    fn test_taint_finding_id_different_rule() {
        let file1 = FileId::generate("app.ts");
        let source = make_source_node(&file1);
        let sink = make_sink_node(&file1);

        let id1 = TaintFindingId::generate("rule.a", &source, &sink, &file1);
        let id2 = TaintFindingId::generate("rule.b", &source, &sink, &file1);
        assert_ne!(id1, id2, "Different rules must produce different IDs");
    }

    #[test]
    fn test_severity_roundtrip() {
        for (s, expected) in &[
            (Severity::Critical, "critical"),
            (Severity::High, "high"),
            (Severity::Medium, "medium"),
            (Severity::Low, "low"),
            (Severity::Info, "info"),
        ] {
            assert_eq!(s.as_str(), *expected);
            assert_eq!(Severity::from_str(expected), Some(*s));
        }
    }

    #[test]
    fn test_taint_rule_serde() {
        let yaml = r#"
id: express.req.query
language: typescript
kind: source
symbol_pattern: Request
access_path_pattern: "*.query.*"
applies_to_return: false
"#;
        let rule: TaintRule = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(rule.id, "express.req.query");
        assert_eq!(rule.language, Some(Language::TypeScript));
        assert_eq!(rule.kind, TaintRuleKind::Source);
        assert_eq!(rule.symbol_pattern, Some("Request".to_string()));
        assert!(rule.access_path_pattern.is_some());
    }
}
