//! Unified symbol resolution with fault-tolerant scoring.
//!
//! Provides a [`SymbolSelector`] system that allows callers (MCP, TUI, CLI) to
//! locate symbols by qualified name with optional disambiguation hints.  Wrong
//! hints never block correct matches — they only affect ranking.
//!
//! # Policies
//!
//! | Policy                 | Multiple candidates behaviour               | Used by                   |
//! |------------------------|---------------------------------------------|---------------------------|
//! | `UniqueOrCandidates`   | Return `Single` if gap >= 400; else `Ambiguous` | detail, explore, context |
//! | `Aggregate`            | Return all as `Ambiguous` (roots for graph)  | calls, impact, path      |
//! | `BestEffortSingle`     | Pick best regardless; mark as `BestEffort`  | trace, usages            |

use serde::{Deserialize, Serialize};

use crate::Store;
use crate::{SymbolDef, SymbolId};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum candidates to return in `Aggregate` mode.
pub const MAX_AGGREGATION_CANDIDATES: usize = 50;

/// Minimum score gap between 1st and 2nd to confidently pick a single symbol.
/// Equal to `SCORE_LINE_EXACT - SCORE_LINE_STRONG` = 400 — exact line is the
/// weakest acceptable unique disambiguator.
const MIN_SCORE_GAP_FOR_UNIQUE: u64 = 400;

// Qualified name: base score for all matching candidates
const SCORE_QNAME_EXACT: u64 = 10_000;

// File path: strong signal
const SCORE_PATH_EXACT: u64 = 3_000;
const SCORE_PATH_SUFFIX: u64 = 2_000;
const SCORE_PATH_SAME_BASENAME: u64 = 1_200;
const SCORE_PATH_SEGMENT_PER: u64 = 200; // per overlapping segment, max 1000 total
const SCORE_PATH_SEGMENT_MAX: u64 = 1_000;

// Line: moderate signal
const SCORE_LINE_EXACT: u64 = 1_200; // delta = 0
const SCORE_LINE_STRONG: u64 = 800; // delta <= 2
const SCORE_LINE_NEAR: u64 = 500; // delta <= 10
const SCORE_LINE_WEAK: u64 = 200; // delta <= 50
const SCORE_LINE_FAR_PER: u64 = 100; // max(0, 100 - delta), only if delta <= 100

// Kind and language: weak tiebreakers
const SCORE_KIND_EXACT: u64 = 200;
const SCORE_LANGUAGE_EXACT: u64 = 100;

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// Unified input: string name or structured selector.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SymbolInput {
    /// Simple qualified name string (e.g. "atlas_engine::Engine").
    Name(String),
    /// Structured selector with optional disambiguation hints.
    Selector(SymbolSelector),
}

/// Structured symbol selector.
///
/// `qualified_name` is required; all other fields are optional hints used for
/// fault-tolerant ranking only — wrong values cannot block correct matches.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SymbolSelector {
    /// Fully qualified symbol name (required).
    pub qualified_name: String,
    /// Project-relative file path hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// 1-based line number hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Symbol kind hint (e.g. "function", "class", "method").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Language hint (e.g. "rust", "typescript").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

// ---------------------------------------------------------------------------
// Policy & result types
// ---------------------------------------------------------------------------

/// Resolution policy chosen by the calling tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolResolutionPolicy {
    /// Require unique or return candidates.  Used by: symbol detail, explore, context.
    UniqueOrCandidates,
    /// Auto-aggregate multiple candidates as roots.  Used by: calls, impact, path.
    Aggregate,
    /// Pick best even with low confidence.  Used by: trace, usages.
    BestEffortSingle,
}

/// Unified resolution result.
#[derive(Debug, Clone)]
pub enum SymbolResolution {
    /// Exactly one symbol resolved.
    Single {
        symbol_id: SymbolId,
        resolved: ResolvedSymbol,
    },
    /// Multiple candidates — policy decides how to handle.
    Ambiguous {
        candidates: Vec<ScoredCandidate>,
        /// Score gap between 1st and 2nd (0 for Aggregate policy).
        score_gap: u64,
    },
    /// No symbol found.
    NotFound {
        qname: String,
        /// Suggested alternative qualified names (up to 5).
        suggestions: Vec<String>,
    },
}

/// Actual resolved symbol info.
///
/// ALWAYS returns database truth — never echoes user input.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedSymbol {
    pub qualified_name: String,
    pub file_path: String,
    pub line: u32,
    pub kind: String,
    pub language: String,
    #[serde(flatten)]
    pub match_info: MatchInfo,
}

/// Metadata about how this symbol was matched.
#[derive(Debug, Clone, Serialize)]
pub struct MatchInfo {
    pub mode: MatchMode,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub ignored_mismatches: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_match: Option<PathMatchQuality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_delta: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// Ambiguity-free: exactly one `find_symbols_by_qname` result.
    UniqueQname,
    /// Ambiguity resolved via scoring: gap >= 400 between 1st and 2nd.
    Scored,
    /// Multiple candidates aggregated intentionally (Aggregate policy).
    Aggregated,
    /// Best pick despite low confidence (BestEffortSingle policy).
    BestEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMatchQuality {
    Exact,
    Suffix,
    SameBasename,
    Fuzzy,
    #[serde(rename = "none")]
    None_,
}

/// Scored candidate for output.  `symbol_ref` is reusable in subsequent queries.
#[derive(Debug, Clone, Serialize)]
pub struct ScoredCandidate {
    pub qualified_name: String,
    pub file_path: String,
    pub line: u32,
    pub kind: String,
    pub language: String,
    pub score: u64,
    pub reasons: Vec<String>,
    pub symbol_ref: SymbolSelector,
    /// Internal SymbolId — not serialized to JSON (MCP consumers use symbol_ref).
    /// Enables direct ID lookup without re-querying by qualified_name.
    #[serde(skip)]
    pub symbol_id: SymbolId,
}

/// Internal scoring struct (not serialized directly).
/// Holds the raw SymbolDef + resolved file_path.
#[derive(Debug, Clone)]
pub(crate) struct CandidateScore {
    pub symbol_def: SymbolDef,
    pub file_path: String,
    pub score: u64,
    pub reasons: Vec<String>,
    pub line_delta: Option<i64>,
    pub path_match: PathMatchQuality,
}

// =========================================================================
// Path utilities
// =========================================================================

/// Normalize and validate a file_path input.
///
/// - Strips "./" prefix
/// - Normalizes backslashes to forward slashes
/// - Rejects ".." path escapes (returns error)
/// - Rejects absolute paths (starts with "/" or contains "://")
/// - Rejects empty paths after normalization
pub fn normalize_and_validate_path(raw: &str) -> Result<String, String> {
    let normalized = raw
        .trim_start_matches("./")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();

    if normalized.contains("..") {
        return Err(format!("file_path contains path escape '..': {raw}"));
    }
    if normalized.starts_with('/') || normalized.contains("://") {
        return Err(format!(
            "file_path must be project-relative, not absolute: {raw}"
        ));
    }
    if normalized.is_empty() {
        return Err("file_path is empty after normalization".into());
    }

    Ok(normalized)
}

/// Analyze how well the input path matches a candidate path.
///
/// Returns `(quality, overlap_segment_count)`.
fn analyze_path_match(input_raw: &str, candidate_raw: &str) -> (PathMatchQuality, usize) {
    let input = normalize_path_for_match(input_raw);
    let candidate = normalize_path_for_match(candidate_raw);

    // 1. Exact match
    if input == candidate {
        return (PathMatchQuality::Exact, 100);
    }

    // 2. Suffix match (candidate ends with input)
    if candidate.ends_with(&input) {
        return (PathMatchQuality::Suffix, 50);
    }
    if input.ends_with(&candidate) {
        return (PathMatchQuality::Suffix, 40);
    }

    // 3. Reverse segment overlap
    let input_segs: Vec<&str> = input.split('/').filter(|s| !s.is_empty()).collect();
    let cand_segs: Vec<&str> = candidate.split('/').filter(|s| !s.is_empty()).collect();
    let overlap = input_segs
        .iter()
        .rev()
        .zip(cand_segs.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    if overlap > 0 {
        // When overlap covers the entire shorter path, it's a Suffix match.
        let min_len = cand_segs.len().min(input_segs.len());
        if overlap >= min_len {
            return (PathMatchQuality::Suffix, overlap);
        }

        // When only the basename (last segment) matches and neither path
        // is single-segment, treat as SameBasename, not Fuzzy.
        if overlap == 1
            && cand_segs.len() > 1
            && input_segs.len() > 1
            && input_segs.last() == cand_segs.last()
        {
            return (PathMatchQuality::SameBasename, 1);
        }

        return (PathMatchQuality::Fuzzy, overlap);
    }

    // 4. Same basename only (no segment overlap from the end because
    //    the preceding segments differ; last-segment match was not
    //    caught by the reverse-overlap logic above).
    if input_segs.last() == cand_segs.last() {
        return (PathMatchQuality::SameBasename, 1);
    }

    (PathMatchQuality::None_, 0)
}

/// Internal path normalization for matching (no validation, but consistent).
fn normalize_path_for_match(raw: &str) -> String {
    raw.trim_start_matches("./")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Compute score from path match analysis.
fn score_path_match(quality: PathMatchQuality, overlap: usize) -> u64 {
    match quality {
        PathMatchQuality::Exact => SCORE_PATH_EXACT,
        PathMatchQuality::Suffix => SCORE_PATH_SUFFIX,
        PathMatchQuality::SameBasename => SCORE_PATH_SAME_BASENAME,
        PathMatchQuality::Fuzzy => {
            let segment_score = (overlap as u64) * SCORE_PATH_SEGMENT_PER;
            segment_score.min(SCORE_PATH_SEGMENT_MAX)
        }
        PathMatchQuality::None_ => 0,
    }
}

// =========================================================================
// Score computation
// =========================================================================

/// Score a single candidate against the selector input.
fn score_single_candidate(
    sel: &SymbolSelector,
    symbol_def: &SymbolDef,
    file_path: &str,
    path_match_quality: PathMatchQuality,
    path_overlap: usize,
) -> CandidateScore {
    let mut score = SCORE_QNAME_EXACT;
    let mut reasons: Vec<String> = vec!["qualified_name_exact".into()];

    // File path scoring
    let path_score = score_path_match(path_match_quality, path_overlap);
    if path_score > 0 {
        score += path_score;
        reasons.push(format!("file_path_{}", serde_rename(path_match_quality)));
    }

    // Line scoring (0-based -> 1-based conversion for comparison)
    let candidate_line = symbol_def.range.start_line.saturating_add(1);
    let line_delta = sel
        .line
        .map(|input_line| (candidate_line as i64 - input_line as i64).abs());
    if let Some(delta) = line_delta {
        let line_score = score_line_delta(delta);
        if line_score > 0 {
            score += line_score;
            let reason = match delta {
                0 => "line_exact",
                d if d <= 2 => "line_strong",
                d if d <= 10 => "line_near",
                d if d <= 50 => "line_weak",
                _ => "line_far",
            };
            reasons.push(reason.into());
        }
    }

    // Kind scoring
    if let Some(ref sel_kind) = sel.kind {
        if sel_kind.eq_ignore_ascii_case(symbol_def.kind.as_str()) {
            score += SCORE_KIND_EXACT;
            reasons.push("kind_exact".into());
        }
    }

    // Language scoring
    if let Some(ref sel_lang) = sel.language {
        if sel_lang.eq_ignore_ascii_case(symbol_def.language.as_str()) {
            score += SCORE_LANGUAGE_EXACT;
            reasons.push("language_exact".into());
        }
    }

    CandidateScore {
        symbol_def: symbol_def.clone(),
        file_path: file_path.to_string(),
        score,
        reasons,
        line_delta,
        path_match: path_match_quality,
    }
}

fn score_line_delta(delta: i64) -> u64 {
    match delta {
        0 => SCORE_LINE_EXACT,
        1 | 2 => SCORE_LINE_STRONG,
        d if d <= 10 => SCORE_LINE_NEAR,
        d if d <= 50 => SCORE_LINE_WEAK,
        d if d <= 100 => {
            let far = 100i64.saturating_sub(d).max(0) as u64;
            far.min(SCORE_LINE_FAR_PER)
        }
        _ => 0,
    }
}

fn serde_rename(q: PathMatchQuality) -> &'static str {
    match q {
        PathMatchQuality::Exact => "exact",
        PathMatchQuality::Suffix => "suffix",
        PathMatchQuality::SameBasename => "same_basename",
        PathMatchQuality::Fuzzy => "fuzzy",
        PathMatchQuality::None_ => "none",
    }
}

/// Compute which optional selector fields did NOT match the chosen candidate.
///
/// Used for transparency in responses: tells the caller which hints were
/// "ignored" in favor of the qualified-name match.
pub fn compute_ignored_mismatches(
    sel: &SymbolSelector,
    sym: &SymbolDef,
    resolved_path: &str,
    resolved_line: u32,
) -> Vec<String> {
    let mut mismatches = Vec::new();

    if let Some(ref sel_fp) = sel.file_path {
        let input_norm = normalize_path_for_match(sel_fp);
        let cand_norm = normalize_path_for_match(resolved_path);
        if input_norm != cand_norm
            && !cand_norm.ends_with(&input_norm)
            && !input_norm.ends_with(&cand_norm)
        {
            mismatches.push("file_path".into());
        }
    }

    if let Some(sel_line) = sel.line {
        if resolved_line != sel_line {
            mismatches.push("line".into());
        }
    }

    if let Some(ref sel_kind) = sel.kind {
        if !sel_kind.eq_ignore_ascii_case(sym.kind.as_str()) {
            mismatches.push("kind".into());
        }
    }

    if let Some(ref sel_lang) = sel.language {
        if !sel_lang.eq_ignore_ascii_case(sym.language.as_str()) {
            mismatches.push("language".into());
        }
    }

    mismatches
}

// =========================================================================
// Store-dependent resolution functions
// =========================================================================

/// Look up candidates by qualified name, with optional lazy structural fallback.
///
/// Returns `Vec` of `(SymbolDef, resolved_file_path)` tuples.
pub fn lookup_candidates(
    store: &Store,
    lazy_orchestrator: Option<&crate::LazyOrchestrator>,
    qname: &str,
) -> Result<Vec<(SymbolDef, String)>, String> {
    let symbols = store
        .find_symbols_by_qname(qname)
        .map_err(|e| format!("Lookup error: {e}"))?;

    if !symbols.is_empty() {
        return Ok(symbols
            .into_iter()
            .map(|s| {
                let path = store
                    .get_file(&s.file_id)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
                    .unwrap_or_default();
                (s, path)
            })
            .collect());
    }

    // Lazy structural fallback: extract simple name
    if let Some(orchestrator) = lazy_orchestrator {
        let simple = qname.rsplit(&['.', ':']).next().unwrap_or(qname);
        if simple != qname {
            // Trigger lazy structural for this name (timeout ~2s)
            // For now, skip — the MCP layer handles lazy extraction separately
            let _ = orchestrator;
        }
    }

    Ok(vec![])
}

/// Score all candidates for a selector, sorted descending.
pub(crate) fn score_candidates(
    store: &Store,
    sel: &SymbolSelector,
) -> Result<Vec<CandidateScore>, String> {
    let candidates = lookup_candidates(store, None, &sel.qualified_name)?;

    let mut scored: Vec<CandidateScore> = candidates
        .iter()
        .map(|(sym, path)| {
            let (quality, overlap) = if let Some(ref input_fp) = sel.file_path {
                analyze_path_match(input_fp, path)
            } else {
                (PathMatchQuality::None_, 0)
            };
            score_single_candidate(sel, sym, path, quality, overlap)
        })
        .collect();

    scored.sort_by(|a, b| b.score.cmp(&a.score));
    Ok(scored)
}

/// Unified symbol resolution entry point.
pub fn resolve_symbol_input(
    store: &Store,
    input: &SymbolInput,
    policy: SymbolResolutionPolicy,
) -> Result<SymbolResolution, String> {
    match input {
        SymbolInput::Name(qname) => resolve_by_name(store, qname, policy),
        SymbolInput::Selector(sel) => resolve_by_selector(store, sel, policy),
    }
}

/// Resolve by plain qualified name — no scoring, no selector hints.
pub fn resolve_by_name(
    store: &Store,
    qname: &str,
    _policy: SymbolResolutionPolicy,
) -> Result<SymbolResolution, String> {
    let candidates = lookup_candidates(store, None, qname)?;

    match candidates.len() {
        0 => Ok(SymbolResolution::NotFound {
            qname: qname.into(),
            suggestions: find_similar_names(store, qname, 5),
        }),
        1 => {
            let (sym, path) = &candidates[0];
            let line = sym.range.start_line.saturating_add(1);
            Ok(SymbolResolution::Single {
                symbol_id: sym.id,
                resolved: ResolvedSymbol {
                    qualified_name: sym.qualified_name.clone(),
                    file_path: path.clone(),
                    line,
                    kind: sym.kind.as_str().to_string(),
                    language: sym.language.as_str().to_string(),
                    match_info: MatchInfo {
                        mode: MatchMode::UniqueQname,
                        ignored_mismatches: vec![],
                        path_match: None,
                        line_delta: None,
                    },
                },
            })
        }
        _ => {
            let scored: Vec<ScoredCandidate> = candidates
                .iter()
                .take(MAX_AGGREGATION_CANDIDATES)
                .map(|(sym, path)| {
                    let line = sym.range.start_line.saturating_add(1);
                    ScoredCandidate {
                        qualified_name: sym.qualified_name.clone(),
                        file_path: path.clone(),
                        line,
                        kind: sym.kind.as_str().to_string(),
                        language: sym.language.as_str().to_string(),
                        score: 0,
                        reasons: vec!["qualified_name_exact".into()],
                        symbol_ref: SymbolSelector {
                            qualified_name: sym.qualified_name.clone(),
                            file_path: Some(path.clone()),
                            line: Some(line),
                            kind: Some(sym.kind.as_str().to_string()),
                            language: Some(sym.language.as_str().to_string()),
                        },
                        symbol_id: sym.id,
                    }
                })
                .collect();
            Ok(SymbolResolution::Ambiguous {
                candidates: scored,
                score_gap: 0,
            })
        }
    }
}

/// Resolve by structured selector — scores all candidates and applies policy.
pub fn resolve_by_selector(
    store: &Store,
    sel: &SymbolSelector,
    policy: SymbolResolutionPolicy,
) -> Result<SymbolResolution, String> {
    // Validate file_path if provided
    if let Some(ref fp) = sel.file_path {
        normalize_and_validate_path(fp)?;
    }

    let mut scored = score_candidates(store, sel)?;

    if scored.is_empty() {
        return Ok(SymbolResolution::NotFound {
            qname: sel.qualified_name.clone(),
            suggestions: find_similar_names(store, &sel.qualified_name, 5),
        });
    }

    if scored.len() == 1 {
        let best = &scored[0];
        let line = best.symbol_def.range.start_line.saturating_add(1);
        let mismatches =
            compute_ignored_mismatches(sel, &best.symbol_def, &best.file_path, line);
        return Ok(SymbolResolution::Single {
            symbol_id: best.symbol_def.id,
            resolved: ResolvedSymbol {
                qualified_name: best.symbol_def.qualified_name.clone(),
                file_path: best.file_path.clone(),
                line,
                kind: best.symbol_def.kind.as_str().to_string(),
                language: best.symbol_def.language.as_str().to_string(),
                match_info: MatchInfo {
                    mode: MatchMode::UniqueQname,
                    ignored_mismatches: mismatches,
                    path_match: Some(best.path_match),
                    line_delta: best.line_delta,
                },
            },
        });
    }

    match policy {
        SymbolResolutionPolicy::Aggregate => {
            let candidates: Vec<ScoredCandidate> = scored
                .into_iter()
                .take(MAX_AGGREGATION_CANDIDATES)
                .map(|cs| {
                    let line = cs.symbol_def.range.start_line.saturating_add(1);
                    ScoredCandidate {
                        qualified_name: cs.symbol_def.qualified_name.clone(),
                        file_path: cs.file_path.clone(),
                        line,
                        kind: cs.symbol_def.kind.as_str().to_string(),
                        language: cs.symbol_def.language.as_str().to_string(),
                        score: cs.score,
                        reasons: cs.reasons,
                        symbol_ref: SymbolSelector {
                            qualified_name: cs.symbol_def.qualified_name.clone(),
                            file_path: Some(cs.file_path.clone()),
                            line: Some(line),
                            kind: Some(cs.symbol_def.kind.as_str().to_string()),
                            language: Some(cs.symbol_def.language.as_str().to_string()),
                        },
                        symbol_id: cs.symbol_def.id,
                    }
                })
                .collect();
            Ok(SymbolResolution::Ambiguous {
                candidates,
                score_gap: 0,
            })
        }
        SymbolResolutionPolicy::UniqueOrCandidates => {
            let gap = scored[0].score.saturating_sub(scored[1].score);
            if gap >= MIN_SCORE_GAP_FOR_UNIQUE {
                let best = scored.remove(0);
                let line = best.symbol_def.range.start_line.saturating_add(1);
                let mismatches =
                    compute_ignored_mismatches(sel, &best.symbol_def, &best.file_path, line);
                Ok(SymbolResolution::Single {
                    symbol_id: best.symbol_def.id,
                    resolved: ResolvedSymbol {
                        qualified_name: best.symbol_def.qualified_name.clone(),
                        file_path: best.file_path.clone(),
                        line,
                        kind: best.symbol_def.kind.as_str().to_string(),
                        language: best.symbol_def.language.as_str().to_string(),
                        match_info: MatchInfo {
                            mode: MatchMode::Scored,
                            ignored_mismatches: mismatches,
                            path_match: Some(best.path_match),
                            line_delta: best.line_delta,
                        },
                    },
                })
            } else {
                let candidates: Vec<ScoredCandidate> = scored
                    .into_iter()
                    .map(|cs| {
                        let line = cs.symbol_def.range.start_line.saturating_add(1);
                        ScoredCandidate {
                            qualified_name: cs.symbol_def.qualified_name.clone(),
                            file_path: cs.file_path.clone(),
                            line,
                            kind: cs.symbol_def.kind.as_str().to_string(),
                            language: cs.symbol_def.language.as_str().to_string(),
                            score: cs.score,
                            reasons: cs.reasons,
                            symbol_ref: SymbolSelector {
                                qualified_name: cs.symbol_def.qualified_name.clone(),
                                file_path: Some(cs.file_path.clone()),
                                line: Some(line),
                                kind: Some(cs.symbol_def.kind.as_str().to_string()),
                                language: Some(cs.symbol_def.language.as_str().to_string()),
                            },
                            symbol_id: cs.symbol_def.id,
                        }
                    })
                    .collect();
                Ok(SymbolResolution::Ambiguous {
                    candidates,
                    score_gap: gap,
                })
            }
        }
        SymbolResolutionPolicy::BestEffortSingle => {
            let best = scored.remove(0);
            let mode = if scored.is_empty()
                || best.score.saturating_sub(scored[0].score) >= MIN_SCORE_GAP_FOR_UNIQUE
            {
                MatchMode::Scored
            } else {
                MatchMode::BestEffort
            };
            let line = best.symbol_def.range.start_line.saturating_add(1);
            let mismatches =
                compute_ignored_mismatches(sel, &best.symbol_def, &best.file_path, line);
            Ok(SymbolResolution::Single {
                symbol_id: best.symbol_def.id,
                resolved: ResolvedSymbol {
                    qualified_name: best.symbol_def.qualified_name.clone(),
                    file_path: best.file_path,
                    line,
                    kind: best.symbol_def.kind.as_str().to_string(),
                    language: best.symbol_def.language.as_str().to_string(),
                    match_info: MatchInfo {
                        mode,
                        ignored_mismatches: mismatches,
                        path_match: Some(best.path_match),
                        line_delta: best.line_delta,
                    },
                },
            })
        }
    }
}

/// Find up to `limit` qualified names similar to `qname`, by matching the
/// simple (unqualified) name portion.
pub fn find_similar_names(store: &Store, qname: &str, limit: usize) -> Vec<String> {
    let simple = qname.rsplit(&['.', ':']).next().unwrap_or(qname);
    match store.find_symbols_by_name(simple) {
        Ok(symbols) => symbols
            .iter()
            .take(limit)
            .map(|s| s.qualified_name.clone())
            .collect(),
        Err(_) => vec![],
    }
}

// =========================================================================
// Unit tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileId, FileInfo, Language, ParseStatus, Store, SymbolKind, TextRange};
    use crate::LazyOrchestrator;
    use std::sync::Arc;

    /// Helper: build a minimal SymbolDef for testing.
    fn make_symbol(
        kind: SymbolKind,
        lang: Language,
        qname: &str,
        line: u32, // 1-based
    ) -> SymbolDef {
        let simple = qname.rsplit(&['.', ':']).next().unwrap_or(qname);
        let line0 = line.saturating_sub(1); // 0-based
        SymbolDef {
            id: SymbolId::from_bytes([0u8; 32]),
            kind,
            name: simple.to_string(),
            qualified_name: qname.to_string(),
            symbol_path: qname.split(&['.', ':']).map(String::from).collect(),
            file_id: FileId::from_bytes([0u8; 32]),
            language: lang,
            range: TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: line0,
                start_column: 0,
                end_line: line0,
                end_column: 0,
            },
            name_range: TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: line0,
                start_column: 0,
                end_line: line0,
                end_column: 0,
            },
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        }
    }

    // ── normalize_and_validate_path ─────────────────────────────────────

    #[test]
    fn test_normalize_and_validate_path_normal() {
        assert_eq!(
            normalize_and_validate_path("./src/main.rs").unwrap(),
            "src/main.rs"
        );
        assert_eq!(
            normalize_and_validate_path("src\\lib\\foo.rs").unwrap(),
            "src/lib/foo.rs"
        );
        assert_eq!(normalize_and_validate_path("src/").unwrap(), "src");
        assert_eq!(
            normalize_and_validate_path("src/main.rs").unwrap(),
            "src/main.rs"
        );
    }

    #[test]
    fn test_normalize_and_validate_path_escape_rejected() {
        assert!(normalize_and_validate_path("../escape.rs").is_err());
        assert!(normalize_and_validate_path("src/../../etc/passwd").is_err());
    }

    #[test]
    fn test_normalize_and_validate_path_absolute_rejected() {
        assert!(normalize_and_validate_path("/etc/passwd").is_err());
    }

    #[test]
    fn test_normalize_and_validate_path_url_rejected() {
        assert!(normalize_and_validate_path("file:///etc/passwd").is_err());
    }

    #[test]
    fn test_normalize_and_validate_path_empty_rejected() {
        assert!(normalize_and_validate_path("./").is_err());
        assert!(normalize_and_validate_path("").is_err());
    }

    // ── analyze_path_match ──────────────────────────────────────────────

    #[test]
    fn test_analyze_path_match_exact() {
        let (q, o) = analyze_path_match("src/main.rs", "src/main.rs");
        assert_eq!(q, PathMatchQuality::Exact);
        assert!(o > 0);
    }

    #[test]
    fn test_analyze_path_match_exact_case_insensitive() {
        let (q, _) = analyze_path_match("SRC/Main.RS", "src/main.rs");
        assert_eq!(q, PathMatchQuality::Exact);
    }

    #[test]
    fn test_analyze_path_match_suffix() {
        // candidate ends with input
        let (q, _) = analyze_path_match("src/main.rs", "project/src/main.rs");
        assert_eq!(q, PathMatchQuality::Suffix);
    }

    #[test]
    fn test_analyze_path_match_basename() {
        // different directories, same filename -> SameBasename
        let (q, _) = analyze_path_match("other/main.rs", "src/main.rs");
        assert_eq!(q, PathMatchQuality::SameBasename);
    }

    #[test]
    fn test_analyze_path_match_fuzzy() {
        // Partial segment overlap from the end:
        // "path/to/file.rs" vs "other/to/file.rs" -> overlap 2 (to/file.rs)
        let (q, overlap) = analyze_path_match("path/to/file.rs", "other/to/file.rs");
        assert_eq!(q, PathMatchQuality::Fuzzy);
        assert_eq!(overlap, 2);
    }

    #[test]
    fn test_analyze_path_match_fuzzy_single() {
        // Single segment overlap from the end where the basename matches AND
        // at least one directory segment also matches:
        // "a/b/same/foo.rs" vs "x/y/same/foo.rs" -> overlap 2 ("same/foo.rs")
        let (q, overlap) = analyze_path_match("a/b/same/foo.rs", "x/y/same/foo.rs");
        assert_eq!(q, PathMatchQuality::Fuzzy);
        assert_eq!(overlap, 2);
    }

    #[test]
    fn test_analyze_path_match_none() {
        let (q, o) = analyze_path_match("src/foo.rs", "lib/bar.rs");
        assert_eq!(q, PathMatchQuality::None_);
        assert_eq!(o, 0);
    }

    // ── score_line_delta ────────────────────────────────────────────────

    #[test]
    fn test_score_line_delta_exact() {
        assert_eq!(score_line_delta(0), 1200);
    }

    #[test]
    fn test_score_line_delta_strong() {
        assert_eq!(score_line_delta(1), 800);
        assert_eq!(score_line_delta(2), 800);
    }

    #[test]
    fn test_score_line_delta_near() {
        assert_eq!(score_line_delta(5), 500);
        assert_eq!(score_line_delta(10), 500);
    }

    #[test]
    fn test_score_line_delta_weak() {
        assert_eq!(score_line_delta(20), 200);
        assert_eq!(score_line_delta(50), 200);
    }

    #[test]
    fn test_score_line_delta_far() {
        // delta=80 -> max(0, 100-80)=20, min(20, 100)=20
        assert_eq!(score_line_delta(80), 20);
        // delta=100 -> max(0, 100-100)=0
        assert_eq!(score_line_delta(100), 0);
        // delta=200 -> 0 (beyond far range)
        assert_eq!(score_line_delta(200), 0);
    }

    // ── score_path_match ────────────────────────────────────────────────

    #[test]
    fn test_score_path_match_exact() {
        assert_eq!(score_path_match(PathMatchQuality::Exact, 100), 3000);
    }

    #[test]
    fn test_score_path_match_suffix() {
        assert_eq!(score_path_match(PathMatchQuality::Suffix, 50), 2000);
    }

    #[test]
    fn test_score_path_match_basename() {
        assert_eq!(score_path_match(PathMatchQuality::SameBasename, 1), 1200);
    }

    #[test]
    fn test_score_path_match_fuzzy_with_overlap() {
        // overlap=3 -> 3*200=600, cap=1000
        assert_eq!(score_path_match(PathMatchQuality::Fuzzy, 3), 600);
    }

    #[test]
    fn test_score_path_match_fuzzy_capped() {
        // overlap=10 -> 10*200=2000, capped at 1000
        assert_eq!(score_path_match(PathMatchQuality::Fuzzy, 10), 1000);
    }

    #[test]
    fn test_score_path_match_none() {
        assert_eq!(score_path_match(PathMatchQuality::None_, 0), 0);
    }

    // ── SymbolSelector serde ────────────────────────────────────────────

    #[test]
    fn test_symbol_selector_deserialize_minimal() {
        let json = r#"{"qualified_name": "my.crate.func"}"#;
        let sel: SymbolSelector = serde_json::from_str(json).unwrap();
        assert_eq!(sel.qualified_name, "my.crate.func");
        assert!(sel.file_path.is_none());
        assert!(sel.line.is_none());
        assert!(sel.kind.is_none());
        assert!(sel.language.is_none());
    }

    #[test]
    fn test_symbol_selector_deserialize_full() {
        let json = r#"{
            "qualified_name": "my.crate.func",
            "file_path": "src/lib.rs",
            "line": 42,
            "kind": "function",
            "language": "rust"
        }"#;
        let sel: SymbolSelector = serde_json::from_str(json).unwrap();
        assert_eq!(sel.qualified_name, "my.crate.func");
        assert_eq!(sel.file_path.unwrap(), "src/lib.rs");
        assert_eq!(sel.line.unwrap(), 42);
        assert_eq!(sel.kind.unwrap(), "function");
        assert_eq!(sel.language.unwrap(), "rust");
    }

    #[test]
    fn test_symbol_selector_serde_roundtrip() {
        let sel = SymbolSelector {
            qualified_name: "foo.bar".into(),
            file_path: Some("src/main.rs".into()),
            line: Some(10),
            kind: Some("function".into()),
            language: Some("rust".into()),
        };
        let json = serde_json::to_string(&sel).unwrap();
        let sel2: SymbolSelector = serde_json::from_str(&json).unwrap();
        assert_eq!(sel.qualified_name, sel2.qualified_name);
        assert_eq!(sel.file_path, sel2.file_path);
        assert_eq!(sel.line, sel2.line);
        assert_eq!(sel.kind, sel2.kind);
        assert_eq!(sel.language, sel2.language);
    }

    #[test]
    fn test_symbol_input_name() {
        let json = r#""my.crate.func""#;
        let input: SymbolInput = serde_json::from_str(json).unwrap();
        match input {
            SymbolInput::Name(name) => assert_eq!(name, "my.crate.func"),
            _ => panic!("expected Name variant"),
        }
    }

    #[test]
    fn test_symbol_input_selector() {
        let json = r#"{"qualified_name": "my.crate.func", "kind": "function"}"#;
        let input: SymbolInput = serde_json::from_str(json).unwrap();
        match input {
            SymbolInput::Selector(sel) => {
                assert_eq!(sel.qualified_name, "my.crate.func");
                assert_eq!(sel.kind.unwrap(), "function");
            }
            _ => panic!("expected Selector variant"),
        }
    }

    // ── compute_ignored_mismatches ──────────────────────────────────────

    #[test]
    fn test_ignored_mismatches_all_match() {
        let sel = SymbolSelector {
            qualified_name: "foo".into(),
            file_path: Some("src/main.rs".into()),
            line: Some(42),
            kind: Some("function".into()),
            language: Some("rust".into()),
        };
        let sym = make_symbol(SymbolKind::Function, Language::Rust, "foo", 42);
        let ignored = compute_ignored_mismatches(&sel, &sym, "src/main.rs", 42);
        assert!(
            ignored.is_empty(),
            "expected no mismatches, got {ignored:?}"
        );
    }

    #[test]
    fn test_ignored_mismatches_file_path_differs() {
        let sel = SymbolSelector {
            qualified_name: "foo".into(),
            file_path: Some("src/main.rs".into()),
            line: None,
            kind: None,
            language: None,
        };
        let sym = make_symbol(SymbolKind::Function, Language::Rust, "foo", 10);
        let ignored = compute_ignored_mismatches(&sel, &sym, "src/lib.rs", 10);
        assert!(ignored.contains(&"file_path".to_string()));
    }

    #[test]
    fn test_ignored_mismatches_line_differs() {
        let sel = SymbolSelector {
            qualified_name: "foo".into(),
            file_path: None,
            line: Some(42),
            kind: None,
            language: None,
        };
        let sym = make_symbol(SymbolKind::Function, Language::Rust, "foo", 99);
        let ignored = compute_ignored_mismatches(&sel, &sym, "src/main.rs", 99);
        assert!(ignored.contains(&"line".to_string()));
    }

    #[test]
    fn test_ignored_mismatches_kind_differs() {
        let sel = SymbolSelector {
            qualified_name: "foo".into(),
            file_path: None,
            line: None,
            kind: Some("function".into()),
            language: None,
        };
        let sym = make_symbol(SymbolKind::Method, Language::Rust, "foo", 10);
        let ignored = compute_ignored_mismatches(&sel, &sym, "src/main.rs", 10);
        assert!(ignored.contains(&"kind".to_string()));
    }

    #[test]
    fn test_ignored_mismatches_language_differs() {
        let sel = SymbolSelector {
            qualified_name: "foo".into(),
            file_path: None,
            line: None,
            kind: None,
            language: Some("typescript".into()),
        };
        let sym = make_symbol(
            SymbolKind::Function,
            Language::JavaScript,
            "foo",
            10,
        );
        let ignored = compute_ignored_mismatches(&sel, &sym, "src/main.ts", 10);
        assert!(ignored.contains(&"language".to_string()));
    }

    // ── Scoring invariants ──────────────────────────────────────────────

    #[test]
    fn test_kind_cannot_force_unique() {
        // SCORE_KIND_EXACT (200) < MIN_SCORE_GAP_FOR_UNIQUE (400).
        assert!(SCORE_KIND_EXACT < MIN_SCORE_GAP_FOR_UNIQUE);
    }

    #[test]
    fn test_language_cannot_force_unique() {
        // SCORE_LANGUAGE_EXACT (100) < MIN_SCORE_GAP_FOR_UNIQUE (400).
        assert!(SCORE_LANGUAGE_EXACT < MIN_SCORE_GAP_FOR_UNIQUE);
    }

    #[test]
    fn test_line_exact_can_force_unique() {
        // SCORE_LINE_EXACT - SCORE_LINE_STRONG = 1200 - 800 = 400
        // which equals MIN_SCORE_GAP_FOR_UNIQUE.
        assert_eq!(
            SCORE_LINE_EXACT - SCORE_LINE_STRONG,
            MIN_SCORE_GAP_FOR_UNIQUE
        );
    }

    // ── lookup_candidates ──────────────────────────────────────────────

    #[test]
    fn test_lookup_candidates_not_found_no_lazy() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        let result =
            lookup_candidates(&store, None, "nonexistent::symbol").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_lookup_candidates_not_found_with_lazy_placeholder() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let store_arc = Arc::new(store);
        let orchestrator =
            LazyOrchestrator::new(store_arc.clone(), None, vec![]);

        // "missing::fn" has simple="fn" != qname, so the lazy fallback
        // branch is entered. The placeholder `let _ = orchestrator`
        // should not panic.
        let result = lookup_candidates(
            store_arc.as_ref(),
            Some(&orchestrator),
            "missing::fn",
        )
        .unwrap();
        assert!(
            result.is_empty(),
            "placeholder fallback should not panic, returns empty"
        );
    }

    #[test]
    fn test_lookup_candidates_found_no_lazy() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        let file_id = FileId::generate("src/engine.rs");
        let file = FileInfo {
            file_id,
            path: "src/engine.rs".into(),
            language: Language::Rust,
            content_hash: "deadbeef".into(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file).unwrap();

        let mut sym =
            make_symbol(SymbolKind::Function, Language::Rust, "Engine.run", 42);
        sym.file_id = file_id;
        sym.id = SymbolId::generate(&file_id, "rust", "Engine.run", "function", None);
        store.insert_symbols(&[sym]).unwrap();

        let result = lookup_candidates(&store, None, "Engine.run").unwrap();
        assert_eq!(result.len(), 1, "should find exactly one candidate");
        assert_eq!(result[0].0.qualified_name, "Engine.run");
        assert!(
            !result[0].1.is_empty(),
            "file_path should not be empty"
        );
    }

    #[test]
    fn scored_candidate_carries_symbol_id() {
        // Verify ScoredCandidate.symbol_id is populated and serialization excludes it.
        let sym = make_symbol(SymbolKind::Function, Language::Rust, "my_crate::foo", 10);
        let sc = ScoredCandidate {
            qualified_name: sym.qualified_name.clone(),
            file_path: "src/lib.rs".into(),
            line: 10,
            kind: sym.kind.as_str().into(),
            language: sym.language.as_str().into(),
            score: 100,
            reasons: vec!["exact".into()],
            symbol_ref: SymbolSelector {
                qualified_name: sym.qualified_name.clone(),
                file_path: Some("src/lib.rs".into()),
                line: Some(10),
                kind: Some(sym.kind.as_str().into()),
                language: Some(sym.language.as_str().into()),
            },
            symbol_id: sym.id,
        };
        assert_eq!(sc.symbol_id, sym.id);
        // Verify JSON output does NOT contain symbol_id
        let json_str = serde_json::to_string(&sc).unwrap();
        assert!(
            !json_str.contains("symbol_id"),
            "symbol_id must be #[serde(skip)]"
        );
    }
}
