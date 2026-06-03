//! Core types for the language-agnostic domain rules system.

use serde::{Deserialize, Serialize};

/// A domain rule as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainRule {
    pub id: String,
    pub language: String,
    pub rule_kind: String,
    pub pattern: String,
    pub pattern_kind: String,
    pub meta: Option<String>,
    pub meta_version: i32,
    pub source: String,
    pub status: String,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

impl From<db::DomainRuleRow> for DomainRule {
    fn from(row: db::DomainRuleRow) -> Self {
        Self {
            id: row.id,
            language: row.language,
            rule_kind: row.rule_kind,
            pattern: row.pattern,
            pattern_kind: row.pattern_kind,
            meta: row.meta,
            meta_version: row.meta_version,
            source: row.source,
            status: row.status,
            confidence: row.confidence,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Pattern matching strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatternKind {
    Exact,
    Prefix,
    Suffix,
    Glob,
    Regex,
}

impl PatternKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Suffix => "suffix",
            Self::Glob => "glob",
            Self::Regex => "regex",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(Self::Exact),
            "prefix" => Some(Self::Prefix),
            "suffix" => Some(Self::Suffix),
            "glob" => Some(Self::Glob),
            "regex" => Some(Self::Regex),
            _ => None,
        }
    }
}

/// Where a domain rule came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSource {
    Builtin,
    Learned,
    User,
}

impl RuleSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Learned => "learned",
            Self::User => "user",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "builtin" => Some(Self::Builtin),
            "learned" => Some(Self::Learned),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// Rule lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleStatus {
    Candidate,
    Enabled,
    Disabled,
    Rejected,
    Deprecated,
}

impl RuleStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Rejected => "rejected",
            Self::Deprecated => "deprecated",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "candidate" => Some(Self::Candidate),
            "enabled" => Some(Self::Enabled),
            "disabled" => Some(Self::Disabled),
            "rejected" => Some(Self::Rejected),
            "deprecated" => Some(Self::Deprecated),
            _ => None,
        }
    }
}

/// Two-tier recognition result for a function name.
#[derive(Debug, Clone)]
pub enum RuleMatch {
    /// Strong: user-defined or approved learned rule.
    Known {
        rule_id: String,
        kind: String,
        confidence: f64,
        meta: Option<serde_json::Value>,
    },
    /// Weak: builtin heuristic or unapproved learned.
    Heuristic {
        rule_id: String,
        kind: String,
        confidence: f64,
        meta: Option<serde_json::Value>,
    },
}
