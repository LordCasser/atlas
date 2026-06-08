//! ExploreDossierBuilder — assembles the Symbol Dossier from repositories.
//!
//! Orchestrates repository calls to build a comprehensive Symbol Dossier:
//! subject identity, source excerpt, call evidence, relation groups,
//! file context, and recommended next queries.

use anyhow::Result;
use std::collections::HashMap;

use super::traits::{FileFactsRepository, RelationRepository, SourceRepository, SymbolRepository};
use super::types::*;

/// Builder that orchestrates dossier construction.
pub struct ExploreDossierBuilder;

impl ExploreDossierBuilder {
    /// Build a full dossier for a resolved symbol.
    pub fn build(
        sym: &types::SymbolDef,
        file_path: &str,
        sym_repo: &dyn SymbolRepository,
        rel_repo: &dyn RelationRepository,
        file_repo: &dyn FileFactsRepository,
        src_repo: &mut dyn SourceRepository,
        request: &ExploreRequest,
        precision_tier: String,
    ) -> Result<ExploreDossier> {
        let mut warnings: Vec<String> = Vec::new();

        // ── Subject info ──────────────────────────────────────────────
        let signature = sym_repo
            .get_signature(&sym.id)
            .unwrap_or(None)
            .or_else(|| sym.signature.clone());
        let subject = SubjectInfo {
            id: sym.id.to_hex(),
            kind: sym.kind.as_str().to_string(),
            name: sym.name.clone(),
            qualified_name: sym.qualified_name.clone(),
            signature,
            language: sym.language.as_str().to_string(),
            file: file_path.to_string(),
            range: SubjectRange {
                start_line: sym.range.start_line + 1,
                end_line: sym.range.end_line + 1,
            },
        };

        // ── Source excerpt ────────────────────────────────────────────
        let source_excerpt = if request.source_mode != SourceMode::None_ {
            build_source_excerpt(sym, src_repo, request, &mut warnings)
        } else {
            None
        };

        // ── Relation counts (centralized, used by multiple sub‑builders) ──────
        let incoming_counts = rel_repo.count_incoming_by_kind(&sym.id).unwrap_or_default();
        let outgoing_counts = rel_repo.count_outgoing_by_kind(&sym.id).unwrap_or_default();
        if incoming_counts.is_empty() && outgoing_counts.is_empty() {
            warnings.push("Relation count data unavailable; totals may be inaccurate".to_string());
        }

        // ── Call evidence ─────────────────────────────────────────────
        let call_evidence = build_call_evidence(
            &sym.id,
            sym_repo,
            rel_repo,
            src_repo,
            request.evidence_limit,
            &incoming_counts,
            &outgoing_counts,
            &mut warnings,
        );

        // ── Relation groups ───────────────────────────────────────────
        let relation_groups = build_relation_groups(
            &sym.id,
            sym_repo,
            rel_repo,
            src_repo,
            request.relation_limit,
            &incoming_counts,
            &outgoing_counts,
            &mut warnings,
        );

        // ── File context ───────────────────────────────────────────────
        let file_context = if request.include_file_context {
            build_file_context(sym, file_path, sym_repo, file_repo, request.peer_limit)
        } else {
            None
        };

        // ── Recommendations ───────────────────────────────────────────
        let recommended_next_queries = if request.include_recommendations {
            generate_recommendations(sym, &incoming_counts, &outgoing_counts)
        } else {
            Vec::new()
        };

        Ok(ExploreDossier {
            subject,
            source_excerpt,
            call_evidence,
            relation_groups,
            file_context,
            recommended_next_queries,
            precision_tier,
            warnings,
        })
    }

    /// Build an `AmbiguousResponse` when a query matches multiple candidates.
    pub fn build_ambiguous(
        query: &str,
        candidates: Vec<SymbolCandidate>,
    ) -> AmbiguousResponse {
        let recommended_next_queries = candidates
            .iter()
            .map(|c| RecommendedQuery {
                tool: "atlas_explore".to_string(),
                args: serde_json::json!({"symbol": c.qualified_name}),
                reason: format!(
                    "Explore '{}' at {}:{}",
                    c.qualified_name, c.file, c.line
                ),
            })
            .collect();

        AmbiguousResponse {
            ambiguous: true,
            query: query.to_string(),
            candidates,
            recommended_next_queries,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Internal helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a `Confidence` score to a human-readable label.
fn confidence_label(c: types::enums::Confidence) -> &'static str {
    let v = c.as_f32();
    if v >= 0.9 {
        "exact"
    } else if v >= 0.6 {
        "high"
    } else if v >= 0.3 {
        "medium"
    } else {
        "inferred"
    }
}

/// Map `InternalRelationKind` to lowercase string (matching EdgeKind::as_str).
fn relation_kind_str(kind: InternalRelationKind) -> &'static str {
    match kind {
        InternalRelationKind::Calls => "calls",
        InternalRelationKind::ReferencesType => "references",
        InternalRelationKind::Implements => "implements",
        InternalRelationKind::Extends => "extends",
        InternalRelationKind::Instantiates => "instantiates",
        InternalRelationKind::Reads => "reads",
        InternalRelationKind::Writes => "writes",
        InternalRelationKind::FieldRead => "field_read",
        InternalRelationKind::FieldWrite => "field_write",
        InternalRelationKind::Decorates => "decorates",
        InternalRelationKind::RegistersCallback => "registers_callback",
    }
}

/// Build a source excerpt based on the requested mode.
///
/// - `None_` → not called by the builder (early return).
/// - `Full` → reads all lines from the symbol's range; hard-truncates by
///   `max_source_bytes`.
/// - `Excerpt` → same as Full but capped at `source_lines` lines of the
///   symbol body (not a context window around it).
fn build_source_excerpt(
    sym: &types::SymbolDef,
    src_repo: &mut dyn SourceRepository,
    request: &ExploreRequest,
    warnings: &mut Vec<String>,
) -> Option<SourceExcerpt> {
    // TextRange lines are 0-based; read_lines takes 1-based inclusive.
    let start_1 = sym.range.start_line + 1;
    let raw_end_1 = sym.range.end_line + 1;

    let (mode, end_1) = match request.source_mode {
        SourceMode::Excerpt => {
            let capped = (start_1 + request.source_lines.saturating_sub(1)).min(raw_end_1);
            (SourceMode::Excerpt, capped)
        }
        SourceMode::Full => (SourceMode::Full, raw_end_1),
        SourceMode::None_ => return None,
    };

    let text = match src_repo.read_lines(&sym.file_id, start_1, end_1) {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("Failed to read source: {e}"));
            return None;
        }
    };

    let truncated = request.max_source_bytes > 0 && text.len() > request.max_source_bytes;
    let text = if truncated {
        warnings.push(format!(
            "source excerpt truncated to {} bytes (original {} bytes)",
            request.max_source_bytes,
            text.len(),
        ));
        // Truncate at a UTF-8 character boundary.
        let mut boundary = request.max_source_bytes;
        while boundary > 0 && !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text[..boundary].to_string()
    } else {
        text
    };

    Some(SourceExcerpt {
        mode,
        start_line: start_1,
        end_line: end_1,
        truncated,
        text,
    })
}

/// Build call evidence (incoming + outgoing) with call-site snippets.
fn build_call_evidence(
    symbol_id: &types::SymbolId,
    sym_repo: &dyn SymbolRepository,
    rel_repo: &dyn RelationRepository,
    src_repo: &mut dyn SourceRepository,
    evidence_limit: usize,
    incoming_counts: &HashMap<InternalRelationKind, usize>,
    outgoing_counts: &HashMap<InternalRelationKind, usize>,
    warnings: &mut Vec<String>,
) -> CallEvidence {
    let call_kinds: &[InternalRelationKind] = &[InternalRelationKind::Calls];

    let incoming_evidence = match rel_repo.incoming_evidence(symbol_id, Some(call_kinds), evidence_limit) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("Failed to query incoming call evidence: {e}"));
            Vec::new()
        }
    };
    let outgoing_evidence = match rel_repo.outgoing_evidence(symbol_id, Some(call_kinds), evidence_limit) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("Failed to query outgoing call evidence: {e}"));
            Vec::new()
        }
    };

    let incoming_total = incoming_counts.get(&InternalRelationKind::Calls).copied().unwrap_or(0);
    let outgoing_total = outgoing_counts.get(&InternalRelationKind::Calls).copied().unwrap_or(0);

    let incoming = CallEvidenceGroup {
        total: incoming_total,
        examples: evidence_to_call_entries(
            incoming_evidence,
            sym_repo,
            src_repo,
            /* is_incoming */ true,
            warnings,
        ),
    };
    let outgoing = CallEvidenceGroup {
        total: outgoing_total,
        examples: evidence_to_call_entries(
            outgoing_evidence,
            sym_repo,
            src_repo,
            /* is_incoming */ false,
            warnings,
        ),
    };

    CallEvidence { incoming, outgoing }
}

/// Convert relation evidence to call evidence entries.
///
/// `is_incoming` determines which side is the "peer" symbol:
/// - incoming (someone calls the subject): peer = source_id (the caller)
/// - outgoing (subject calls someone): peer = target_id (the callee)
fn evidence_to_call_entries(
    evidence: Vec<super::traits::RelationEvidence>,
    sym_repo: &dyn SymbolRepository,
    src_repo: &mut dyn SourceRepository,
    is_incoming: bool,
    warnings: &mut Vec<String>,
) -> Vec<CallEvidenceEntry> {
    evidence
        .into_iter()
        .filter_map(|ev| {
            let peer_id = if is_incoming {
                ev.source_id
            } else {
                ev.target_id
            };

            let peer_sym = match sym_repo.get_symbol_by_id(&peer_id) {
                Ok(Some(s)) => s,
                Ok(None) => return None,
                Err(e) => {
                    warnings.push(format!("Failed to look up peer symbol {}: {e}", peer_id.to_hex()));
                    return None;
                }
            };
            let peer_file = match sym_repo.get_file_path(&peer_sym.file_id) {
                Ok(Some(p)) => p,
                Ok(None) => String::new(),
                Err(e) => {
                    warnings.push(format!("Failed to resolve file path: {e}"));
                    String::new()
                }
            };

            let snippet = src_repo
                .read_range(&ev.file_id, &ev.range)
                .unwrap_or_default();

            let callsite_file = match sym_repo.get_file_path(&ev.file_id) {
                Ok(Some(p)) => p,
                Ok(None) => String::new(),
                Err(e) => {
                    warnings.push(format!("Failed to resolve callsite file: {e}"));
                    String::new()
                }
            };

            Some(CallEvidenceEntry {
                symbol: PeerSymbol {
                    name: peer_sym.name,
                    qualified_name: peer_sym.qualified_name,
                    kind: peer_sym.kind.as_str().to_string(),
                    file: peer_file,
                    line: peer_sym.range.start_line + 1,
                    signature: peer_sym.signature,
                },
                callsite: CallSite {
                    file: callsite_file,
                    line: ev.range.start_line + 1,
                    column: ev.range.start_column + 1,
                    snippet,
                },
                edge_kind: relation_kind_str(ev.relation_kind).to_string(),
                confidence: confidence_label(ev.confidence).to_string(),
            })
        })
        .collect()
}

/// Build non-call relation groups.
///
/// Queries all non-call kinds together, groups results in memory,
/// then distributes into the `RelationGroups` structure.
/// FieldRead + FieldWrite are merged into `field_access`.
fn build_relation_groups(
    symbol_id: &types::SymbolId,
    sym_repo: &dyn SymbolRepository,
    rel_repo: &dyn RelationRepository,
    src_repo: &mut dyn SourceRepository,
    relation_limit: usize,
    incoming_counts: &HashMap<InternalRelationKind, usize>,
    outgoing_counts: &HashMap<InternalRelationKind, usize>,
    warnings: &mut Vec<String>,
) -> RelationGroups {
    let non_call_kinds: &[InternalRelationKind] = &[
        InternalRelationKind::ReferencesType,
        InternalRelationKind::Implements,
        InternalRelationKind::Extends,
        InternalRelationKind::Instantiates,
        InternalRelationKind::Reads,
        InternalRelationKind::Writes,
        InternalRelationKind::FieldRead,
        InternalRelationKind::FieldWrite,
        InternalRelationKind::Decorates,
        InternalRelationKind::RegistersCallback,
    ];

    let incoming = match rel_repo.incoming_evidence(symbol_id, Some(non_call_kinds), relation_limit) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("Failed to query incoming non-call relations: {e}"));
            Vec::new()
        }
    };
    let outgoing = match rel_repo.outgoing_evidence(symbol_id, Some(non_call_kinds), relation_limit) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("Failed to query outgoing non-call relations: {e}"));
            Vec::new()
        }
    };
    let all_evidence: Vec<_> = incoming.into_iter().chain(outgoing).collect();

    // Group by InternalRelationKind
    let mut grouped: HashMap<InternalRelationKind, Vec<RelationEntry>> = HashMap::new();

    for ev in all_evidence {
        // For relation evidence, the "other" symbol is the one that is NOT
        // the subject. Since we don't have the subject ID here, we use
        // target_id as the peer (works for most relation types where the
        // subject is the source of the relation).
        let peer_id = ev.target_id;
        let peer_sym = match sym_repo.get_symbol_by_id(&peer_id).ok().flatten() {
            Some(s) => s,
            None => continue,
        };
        let peer_file = sym_repo
            .get_file_path(&peer_sym.file_id)
            .ok()
            .flatten()
            .unwrap_or_default();

        let snippet = src_repo
            .read_range(&ev.file_id, &ev.range)
            .ok()
            .filter(|s| !s.is_empty());

        let entry = RelationEntry {
            symbol: PeerSymbol {
                name: peer_sym.name,
                qualified_name: peer_sym.qualified_name,
                kind: peer_sym.kind.as_str().to_string(),
                file: peer_file,
                line: peer_sym.range.start_line + 1,
                signature: peer_sym.signature,
            },
            snippet,
            confidence: confidence_label(ev.confidence).to_string(),
        };

        grouped.entry(ev.relation_kind).or_default().push(entry);
    }

    // Build FieldAccessGroup from FieldRead + FieldWrite
    let mut field_access_examples: Vec<FieldAccessEntry> = Vec::new();
    let mut field_access_total: usize = 0;
    if let Some(entries) = grouped.remove(&InternalRelationKind::FieldRead) {
        field_access_total += incoming_counts
            .get(&InternalRelationKind::FieldRead)
            .copied()
            .unwrap_or(0)
            + outgoing_counts
                .get(&InternalRelationKind::FieldRead)
                .copied()
                .unwrap_or(0);
        for e in entries {
            field_access_examples.push(FieldAccessEntry {
                access: "read".to_string(),
                symbol: e.symbol,
                snippet: e.snippet,
                confidence: e.confidence,
            });
        }
    }
    if let Some(entries) = grouped.remove(&InternalRelationKind::FieldWrite) {
        field_access_total += incoming_counts
            .get(&InternalRelationKind::FieldWrite)
            .copied()
            .unwrap_or(0)
            + outgoing_counts
                .get(&InternalRelationKind::FieldWrite)
                .copied()
                .unwrap_or(0);
        for e in entries {
            field_access_examples.push(FieldAccessEntry {
                access: "write".to_string(),
                symbol: e.symbol,
                snippet: e.snippet,
                confidence: e.confidence,
            });
        }
    }

    let field_access = if !field_access_examples.is_empty() {
        Some(FieldAccessGroup {
            total: field_access_total,
            examples: field_access_examples,
        })
    } else {
        None
    };

    // Helper: extract a RelationGroup from the grouped map.
    let mut take_group = |kind: InternalRelationKind| -> Option<RelationGroup> {
        grouped.remove(&kind).map(|examples| {
            let total = incoming_counts
                .get(&kind)
                .copied()
                .unwrap_or(0)
                + outgoing_counts.get(&kind).copied().unwrap_or(0);
            RelationGroup {
                total: total.max(examples.len()),
                examples,
            }
        })
    };

    let references_type = take_group(InternalRelationKind::ReferencesType);
    let implements = take_group(InternalRelationKind::Implements);
    let extends = take_group(InternalRelationKind::Extends);
    let instantiates = take_group(InternalRelationKind::Instantiates);
    let reads = take_group(InternalRelationKind::Reads);
    let writes = take_group(InternalRelationKind::Writes);
    let decorates = take_group(InternalRelationKind::Decorates);
    let registers_callback = take_group(InternalRelationKind::RegistersCallback);

    // Warn about any unhandled leftover groups (shouldn't happen in practice).
    for (kind, entries) in &grouped {
        if !entries.is_empty() {
            warnings.push(format!(
                "Unhandled relation kind {:?} with {} entries",
                kind,
                entries.len()
            ));
        }
    }

    RelationGroups {
        references_type,
        implements,
        extends,
        instantiates,
        field_access,
        reads,
        writes,
        decorates,
        registers_callback,
    }
}

/// Build file-level context: imports, exports, peers.
fn build_file_context(
    sym: &types::SymbolDef,
    file_path: &str,
    sym_repo: &dyn SymbolRepository,
    file_repo: &dyn FileFactsRepository,
    peer_limit: usize,
) -> Option<FileContext> {
    let imports: Vec<ImportFact> = file_repo
        .get_imports(&sym.file_id)
        .unwrap_or_default()
        .into_iter()
        .map(|imp| ImportFact {
            module: imp.module,
            symbols: {
                if imp.is_wildcard {
                    vec!["*".to_string()]
                } else if let Some(alias) = &imp.alias {
                    vec![alias.clone()]
                } else {
                    vec![imp.imported_name.clone()]
                }
            },
            line: imp.range.start_line + 1,
        })
        .collect();

    let exports = file_repo.get_exports(&sym.file_id).unwrap_or_default();

    let peer_symbols = file_repo
        .get_peers(&sym.file_id, &sym.id, peer_limit)
        .unwrap_or_default();

    let peers: Vec<PeerSymbol> = peer_symbols
        .into_iter()
        .map(|s| {
            let sig = sym_repo.get_signature(&s.id).ok().flatten();
            PeerSymbol {
                name: s.name,
                qualified_name: s.qualified_name,
                kind: s.kind.as_str().to_string(),
                file: file_path.to_string(),
                line: s.range.start_line + 1,
                signature: sig,
            }
        })
        .collect();

    Some(FileContext {
        file: file_path.to_string(),
        imports,
        exports,
        peers,
    })
}

/// Generate recommended next queries.
fn generate_recommendations(
    sym: &types::SymbolDef,
    incoming_counts: &HashMap<InternalRelationKind, usize>,
    outgoing_counts: &HashMap<InternalRelationKind, usize>,
) -> Vec<RecommendedQuery> {
    let mut recs = Vec::new();
    let qname = &sym.qualified_name;

    // If there are callers → trace incoming call graph
    let has_incoming_calls = incoming_counts.get(&InternalRelationKind::Calls).copied().unwrap_or(0) > 0;
    // If there are callees → trace outgoing call graph
    let has_outgoing_calls = outgoing_counts.get(&InternalRelationKind::Calls).copied().unwrap_or(0) > 0;
    // If there are non-call relations → suggest context view
    let has_non_call = incoming_counts.iter().any(|(k, &c)| c > 0 && *k != InternalRelationKind::Calls)
        || outgoing_counts.iter().any(|(k, &c)| c > 0 && *k != InternalRelationKind::Calls);

    if has_incoming_calls && has_outgoing_calls {
        recs.push(RecommendedQuery {
            tool: "atlas_calls".to_string(),
            args: serde_json::json!({"symbol": qname, "direction": "both", "depth": 2}),
            reason: "Explore call graph 2 hops in both directions (has both callers and callees)".to_string(),
        });
    } else if has_incoming_calls {
        recs.push(RecommendedQuery {
            tool: "atlas_calls".to_string(),
            args: serde_json::json!({"symbol": qname, "direction": "incoming", "depth": 2}),
            reason: "Explore incoming callers up to 2 hops".to_string(),
        });
    } else if has_outgoing_calls {
        recs.push(RecommendedQuery {
            tool: "atlas_calls".to_string(),
            args: serde_json::json!({"symbol": qname, "direction": "outgoing", "depth": 2}),
            reason: "Explore outgoing callees up to 2 hops".to_string(),
        });
    }

    if has_non_call || has_incoming_calls || has_outgoing_calls {
        recs.push(RecommendedQuery {
            tool: "atlas_symbol".to_string(),
            args: serde_json::json!({"symbol": qname, "view": "context"}),
            reason: "Get rich structured context for this symbol".to_string(),
        });
    }

    recs
}
