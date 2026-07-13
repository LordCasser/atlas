//! ExploreDossierBuilder — assembles the Symbol Dossier from repositories.
//!
//! Orchestrates repository calls to build a comprehensive Symbol Dossier:
//! subject identity, source excerpt, call evidence, relation groups,
//! file context, and recommended next queries.

use anyhow::Result;
use std::collections::{HashMap, HashSet};

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
        src_repo: &dyn SourceRepository,
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
    pub fn build_ambiguous(query: &str, candidates: Vec<SymbolCandidate>) -> AmbiguousResponse {
        let recommended_next_queries = candidates
            .iter()
            .map(|c| RecommendedQuery {
                tool: "atlas_explore".to_string(),
                args: serde_json::json!({"symbol": c.qualified_name}),
                reason: format!("Explore '{}' at {}:{}", c.qualified_name, c.file, c.line),
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
        InternalRelationKind::References => "references",
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
    src_repo: &dyn SourceRepository,
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
        truncate_utf8(&text, request.max_source_bytes).to_string()
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

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    let mut boundary = max_bytes.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &text[..boundary]
}

/// Build call evidence (incoming + outgoing) with call-site snippets.
fn build_call_evidence(
    symbol_id: &types::SymbolId,
    sym_repo: &dyn SymbolRepository,
    rel_repo: &dyn RelationRepository,
    src_repo: &dyn SourceRepository,
    evidence_limit: usize,
    incoming_counts: &HashMap<InternalRelationKind, usize>,
    outgoing_counts: &HashMap<InternalRelationKind, usize>,
    warnings: &mut Vec<String>,
) -> CallEvidence {
    let call_kinds: &[InternalRelationKind] = &[InternalRelationKind::Calls];

    let incoming_evidence =
        match rel_repo.incoming_evidence(symbol_id, Some(call_kinds), evidence_limit) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("Failed to query incoming call evidence: {e}"));
                Vec::new()
            }
        };
    let outgoing_evidence =
        match rel_repo.outgoing_evidence(symbol_id, Some(call_kinds), evidence_limit) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("Failed to query outgoing call evidence: {e}"));
                Vec::new()
            }
        };

    let incoming_total = incoming_counts
        .get(&InternalRelationKind::Calls)
        .copied()
        .unwrap_or(0);
    let outgoing_total = outgoing_counts
        .get(&InternalRelationKind::Calls)
        .copied()
        .unwrap_or(0);

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
    src_repo: &dyn SourceRepository,
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
                    warnings.push(format!(
                        "Failed to look up peer symbol {}: {e}",
                        peer_id.to_hex()
                    ));
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
    src_repo: &dyn SourceRepository,
    relation_limit: usize,
    incoming_counts: &HashMap<InternalRelationKind, usize>,
    outgoing_counts: &HashMap<InternalRelationKind, usize>,
    warnings: &mut Vec<String>,
) -> RelationGroups {
    let non_call_kinds: &[InternalRelationKind] = &[
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

    let incoming = match rel_repo.incoming_evidence(symbol_id, Some(non_call_kinds), relation_limit)
    {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("Failed to query incoming non-call relations: {e}"));
            Vec::new()
        }
    };
    let outgoing = match rel_repo.outgoing_evidence(symbol_id, Some(non_call_kinds), relation_limit)
    {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("Failed to query outgoing non-call relations: {e}"));
            Vec::new()
        }
    };
    let all_evidence: Vec<_> = incoming.into_iter().chain(outgoing).collect();

    // Group by InternalRelationKind
    let mut grouped: HashMap<InternalRelationKind, Vec<RelationEntry>> = HashMap::new();
    let mut seen_peers = HashSet::new();

    for ev in all_evidence {
        let peer_id = if ev.source_id == *symbol_id {
            ev.target_id
        } else {
            ev.source_id
        };
        if !seen_peers.insert((ev.relation_kind, peer_id)) {
            continue;
        }
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
            let total = incoming_counts.get(&kind).copied().unwrap_or(0)
                + outgoing_counts.get(&kind).copied().unwrap_or(0);
            RelationGroup {
                total: total.max(examples.len()),
                examples,
            }
        })
    };

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
                } else if imp.imported_name.is_empty() {
                    Vec::new()
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
    let has_incoming_calls = incoming_counts
        .get(&InternalRelationKind::Calls)
        .copied()
        .unwrap_or(0)
        > 0;
    // If there are callees → trace outgoing call graph
    let has_outgoing_calls = outgoing_counts
        .get(&InternalRelationKind::Calls)
        .copied()
        .unwrap_or(0)
        > 0;
    // If there are non-call relations → suggest context view
    let has_non_call = incoming_counts
        .iter()
        .any(|(k, &c)| c > 0 && *k != InternalRelationKind::Calls)
        || outgoing_counts
            .iter()
            .any(|(k, &c)| c > 0 && *k != InternalRelationKind::Calls);

    if has_incoming_calls && has_outgoing_calls {
        recs.push(RecommendedQuery {
            tool: "atlas_calls".to_string(),
            args: serde_json::json!({"symbol": qname, "direction": "both", "depth": 2}),
            reason: "Explore call graph 2 hops in both directions (has both callers and callees)"
                .to_string(),
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

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use types::{
        Confidence, FileId, ImportDef, ImportId, ImportKind, Language, SymbolDef, SymbolId,
        SymbolKind, TextRange,
    };

    use super::super::traits::{
        FileFactsRepository, RelationEvidence, RelationRepository, SourceRepository,
        SymbolRepository,
    };

    // ── helper constructors ──────────────────────────────────────────────

    fn fid(name: &str) -> FileId {
        FileId::generate(name)
    }

    fn sid(file: &FileId, path: &str, kind: &str) -> SymbolId {
        SymbolId::generate(file, "typescript", path, kind, None)
    }

    fn make_symbol(name: &str, qname: &str, sym_id: SymbolId, file_id: FileId) -> SymbolDef {
        SymbolDef {
            id: sym_id,
            kind: SymbolKind::Function,
            name: name.to_string(),
            qualified_name: qname.to_string(),
            symbol_path: qname.split('.').map(|s| s.to_string()).collect(),
            file_id,
            language: Language::TypeScript,
            range: TextRange {
                start_line: 0,
                end_line: 9,
                ..Default::default()
            },
            name_range: TextRange::default(),
            signature: Some(format!("fn {name}()")),
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

    fn tr(start_byte: u32, end_byte: u32, start_line: u32, end_line: u32) -> TextRange {
        TextRange {
            start_byte,
            end_byte,
            start_line,
            start_column: 0,
            end_line,
            end_column: 0,
        }
    }

    // ── Mock implementations ─────────────────────────────────────────────

    struct MockSymbolRepo {
        symbols: RefCell<HashMap<SymbolId, SymbolDef>>,
        files: RefCell<HashMap<FileId, String>>,
        resolve_results: RefCell<HashMap<String, Vec<SymbolDef>>>,
    }

    impl MockSymbolRepo {
        fn new() -> Self {
            Self {
                symbols: RefCell::new(HashMap::new()),
                files: RefCell::new(HashMap::new()),
                resolve_results: RefCell::new(HashMap::new()),
            }
        }
        fn add_symbol(&self, sym: SymbolDef) {
            self.symbols.borrow_mut().insert(sym.id, sym);
        }
        fn add_file(&self, file_id: FileId, path: &str) {
            self.files.borrow_mut().insert(file_id, path.to_string());
        }
    }

    impl SymbolRepository for MockSymbolRepo {
        fn resolve(&self, query: &str) -> anyhow::Result<Vec<SymbolDef>> {
            Ok(self
                .resolve_results
                .borrow()
                .get(query)
                .cloned()
                .unwrap_or_default())
        }
        fn get_signature(&self, symbol_id: &SymbolId) -> anyhow::Result<Option<String>> {
            Ok(self
                .symbols
                .borrow()
                .get(symbol_id)
                .and_then(|s| s.signature.clone()))
        }
        fn get_symbol_by_id(&self, id: &SymbolId) -> anyhow::Result<Option<SymbolDef>> {
            Ok(self.symbols.borrow().get(id).cloned())
        }
        fn get_file_path(&self, file_id: &FileId) -> anyhow::Result<Option<String>> {
            Ok(self.files.borrow().get(file_id).cloned())
        }
    }

    struct MockRelationRepo {
        incoming: RefCell<HashMap<SymbolId, Vec<RelationEvidence>>>,
        outgoing: RefCell<HashMap<SymbolId, Vec<RelationEvidence>>>,
        inc_counts: RefCell<HashMap<SymbolId, HashMap<InternalRelationKind, usize>>>,
        out_counts: RefCell<HashMap<SymbolId, HashMap<InternalRelationKind, usize>>>,
    }

    impl MockRelationRepo {
        fn new() -> Self {
            Self {
                incoming: RefCell::new(HashMap::new()),
                outgoing: RefCell::new(HashMap::new()),
                inc_counts: RefCell::new(HashMap::new()),
                out_counts: RefCell::new(HashMap::new()),
            }
        }
    }

    impl RelationRepository for MockRelationRepo {
        fn incoming_evidence(
            &self,
            symbol_id: &SymbolId,
            kinds: Option<&[InternalRelationKind]>,
            _limit: usize,
        ) -> anyhow::Result<Vec<RelationEvidence>> {
            let all = self
                .incoming
                .borrow()
                .get(symbol_id)
                .cloned()
                .unwrap_or_default();
            let filtered: Vec<_> = match kinds {
                Some(filter) => all
                    .into_iter()
                    .filter(|e| filter.contains(&e.relation_kind))
                    .collect(),
                None => all,
            };
            Ok(filtered)
        }
        fn outgoing_evidence(
            &self,
            symbol_id: &SymbolId,
            kinds: Option<&[InternalRelationKind]>,
            _limit: usize,
        ) -> anyhow::Result<Vec<RelationEvidence>> {
            let all = self
                .outgoing
                .borrow()
                .get(symbol_id)
                .cloned()
                .unwrap_or_default();
            let filtered: Vec<_> = match kinds {
                Some(filter) => all
                    .into_iter()
                    .filter(|e| filter.contains(&e.relation_kind))
                    .collect(),
                None => all,
            };
            Ok(filtered)
        }
        fn count_incoming_by_kind(
            &self,
            symbol_id: &SymbolId,
        ) -> anyhow::Result<HashMap<InternalRelationKind, usize>> {
            Ok(self
                .inc_counts
                .borrow()
                .get(symbol_id)
                .cloned()
                .unwrap_or_default())
        }
        fn count_outgoing_by_kind(
            &self,
            symbol_id: &SymbolId,
        ) -> anyhow::Result<HashMap<InternalRelationKind, usize>> {
            Ok(self
                .out_counts
                .borrow()
                .get(symbol_id)
                .cloned()
                .unwrap_or_default())
        }
    }

    struct MockFileFactsRepo {
        imports: RefCell<HashMap<FileId, Vec<ImportDef>>>,
        exports: RefCell<HashMap<FileId, Vec<ExportFact>>>,
        peers: RefCell<HashMap<FileId, Vec<SymbolDef>>>,
    }

    impl MockFileFactsRepo {
        fn new() -> Self {
            Self {
                imports: RefCell::new(HashMap::new()),
                exports: RefCell::new(HashMap::new()),
                peers: RefCell::new(HashMap::new()),
            }
        }
    }

    impl FileFactsRepository for MockFileFactsRepo {
        fn get_imports(&self, file_id: &FileId) -> anyhow::Result<Vec<ImportDef>> {
            Ok(self
                .imports
                .borrow()
                .get(file_id)
                .cloned()
                .unwrap_or_default())
        }
        fn get_exports(&self, file_id: &FileId) -> anyhow::Result<Vec<ExportFact>> {
            Ok(self
                .exports
                .borrow()
                .get(file_id)
                .cloned()
                .unwrap_or_default())
        }
        fn get_peers(
            &self,
            file_id: &FileId,
            exclude_id: &SymbolId,
            limit: usize,
        ) -> anyhow::Result<Vec<SymbolDef>> {
            let all = self
                .peers
                .borrow()
                .get(file_id)
                .cloned()
                .unwrap_or_default();
            let filtered: Vec<_> = all
                .into_iter()
                .filter(|s| &s.id != exclude_id)
                .take(limit)
                .collect();
            Ok(filtered)
        }
    }

    struct MockSourceRepo {
        files: RefCell<HashMap<FileId, String>>,
        /// Tracks how many times load_file was called (reset by clear_cache).
        read_count: RefCell<usize>,
    }

    impl MockSourceRepo {
        fn new() -> Self {
            Self {
                files: RefCell::new(HashMap::new()),
                read_count: RefCell::new(0),
            }
        }
        fn add_file(&self, file_id: FileId, content: &str) {
            self.files.borrow_mut().insert(file_id, content.to_string());
        }
        fn load_content(&self, file_id: &FileId) -> anyhow::Result<String> {
            *self.read_count.borrow_mut() += 1;
            self.files
                .borrow()
                .get(file_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("file not found"))
        }
    }

    impl SourceRepository for MockSourceRepo {
        fn read_range(&self, file_id: &FileId, range: &TextRange) -> anyhow::Result<String> {
            let content = self.load_content(file_id)?;
            let start = range.start_byte as usize;
            let end = range.end_byte as usize;
            Ok(content.get(start..end).unwrap_or("").to_string())
        }
        fn read_lines(
            &self,
            file_id: &FileId,
            start_line: u32,
            end_line: u32,
        ) -> anyhow::Result<String> {
            let content = self.load_content(file_id)?;
            let skip = start_line.saturating_sub(1) as usize;
            let take = end_line.saturating_sub(start_line).saturating_add(1) as usize;
            let joined = content
                .lines()
                .skip(skip)
                .take(take)
                .collect::<Vec<_>>()
                .join("\n");
            Ok(joined)
        }
        fn clear_cache(&self) {
            *self.read_count.borrow_mut() = 0;
        }
    }

    // ── builder tests ────────────────────────────────────────────────────

    fn default_request() -> ExploreRequest {
        ExploreRequest {
            symbol: "test_func".to_string(),
            source_mode: SourceMode::Excerpt,
            source_lines: 40,
            evidence_limit: 5,
            relation_limit: 20,
            peer_limit: 12,
            include_file_context: true,
            include_recommendations: true,
            max_source_bytes: 65536,
        }
    }

    #[test]
    fn build_assembles_valid_dossier() {
        let file = fid("src/main.ts");
        let sym = make_symbol(
            "main",
            "Main.main",
            sid(&file, "Main.main", "function"),
            file,
        );

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_file(file, "src/main.ts");

        let rel_repo = MockRelationRepo::new();
        // Prevent "relation count unavailable" warning by seeding a dummy count.
        let mut counts = HashMap::new();
        counts.insert(InternalRelationKind::Calls, 0);
        rel_repo
            .inc_counts
            .borrow_mut()
            .insert(sym.id, counts.clone());
        rel_repo.out_counts.borrow_mut().insert(sym.id, counts);

        let file_repo = MockFileFactsRepo::new();

        let src_repo = MockSourceRepo::new();
        src_repo.add_file(file, "fn main() {\n    println!(\"hi\");\n}\n");

        let req = default_request();

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/main.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        // Check subject info
        assert_eq!(dossier.subject.name, "main");
        assert_eq!(dossier.subject.qualified_name, "Main.main");
        assert_eq!(dossier.subject.file, "src/main.ts");
        assert_eq!(dossier.subject.kind, "function");
        assert_eq!(dossier.precision_tier, "exact");

        // Source excerpt present (Excerpt mode)
        assert!(dossier.source_excerpt.is_some());

        // Call evidence exists (empty groups)
        assert_eq!(dossier.call_evidence.incoming.total, 0);
        assert_eq!(dossier.call_evidence.outgoing.total, 0);

        // File context present (include_file_context=true)
        assert!(dossier.file_context.is_some());

        // Recommendations: with no relations, none are expected.
        assert!(
            dossier.recommended_next_queries.is_empty(),
            "expected no recommendations without relations"
        );

        // No warnings
        assert!(dossier.warnings.is_empty());
    }

    #[test]
    fn file_context_does_not_serialize_empty_import_symbol() {
        let file = fid("src/main.c");
        let sym = make_symbol("main", "main", sid(&file, "main", "function"), file);
        let sym_repo = MockSymbolRepo::new();
        let file_repo = MockFileFactsRepo::new();
        file_repo.imports.borrow_mut().insert(
            file,
            vec![ImportDef {
                id: ImportId::generate(&file, "include", "linux/kernel.h", None, 0),
                file_id: file,
                kind: ImportKind::Include,
                module: "linux/kernel.h".into(),
                imported_name: String::new(),
                local_name: None,
                is_wildcard: false,
                is_relative: false,
                range: tr(0, 0, 0, 0),
                alias: None,
            }],
        );

        let context = build_file_context(&sym, "src/main.c", &sym_repo, &file_repo, 12).unwrap();
        assert_eq!(context.imports.len(), 1);
        assert!(context.imports[0].symbols.is_empty());
    }

    #[test]
    fn build_ambiguous_returns_correct_candidates() {
        let candidates = vec![
            SymbolCandidate {
                qualified_name: "A.foo".to_string(),
                signature: Some("fn foo()".to_string()),
                file: "a.ts".to_string(),
                line: 10,
                kind: "function".to_string(),
                language: "typescript".to_string(),
            },
            SymbolCandidate {
                qualified_name: "B.foo".to_string(),
                signature: None,
                file: "b.ts".to_string(),
                line: 20,
                kind: "method".to_string(),
                language: "typescript".to_string(),
            },
        ];

        let response = ExploreDossierBuilder::build_ambiguous("foo", candidates);
        assert!(response.ambiguous);
        assert_eq!(response.query, "foo");
        assert_eq!(response.candidates.len(), 2);
        assert_eq!(response.recommended_next_queries.len(), 2);

        // Each recommendation uses atlas_explore with the qualified name.
        assert_eq!(response.recommended_next_queries[0].tool, "atlas_explore");
        assert_eq!(response.recommended_next_queries[0].args["symbol"], "A.foo");
    }

    #[test]
    fn source_mode_none_excludes_source_excerpt() {
        let file = fid("src/a.ts");
        let sym = make_symbol("a", "A.a", sid(&file, "A.a", "function"), file);

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_file(file, "src/a.ts");

        let rel_repo = MockRelationRepo::new();
        let file_repo = MockFileFactsRepo::new();
        let src_repo = MockSourceRepo::new();

        let mut req = default_request();
        req.source_mode = SourceMode::None_;

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/a.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        assert!(dossier.source_excerpt.is_none());
    }

    #[test]
    fn source_mode_full_reads_full_source() {
        let file = fid("src/full.ts");
        let sym = make_symbol("f", "F.f", sid(&file, "F.f", "function"), file);

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_file(file, "src/full.ts");

        let rel_repo = MockRelationRepo::new();
        let file_repo = MockFileFactsRepo::new();

        let src_repo = MockSourceRepo::new();
        src_repo.add_file(
            file,
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
        );

        let mut req = default_request();
        req.source_mode = SourceMode::Full;

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/full.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        let excerpt = dossier.source_excerpt.unwrap();
        assert_eq!(excerpt.mode, SourceMode::Full);
        assert!(!excerpt.truncated);
        // Full mode reads all lines in the symbol range (0-based: 0..10, 1-based: 1..10)
        assert_eq!(excerpt.start_line, 1);
        assert_eq!(excerpt.end_line, 10);
    }

    #[test]
    fn source_excerpt_does_not_cross_into_the_next_declaration() {
        let file = fid("src/functions.ts");
        let source = "function first() {\n  return 1;\n}\nfunction second() {\n  return 2;\n}\n";
        let first_end = source.find("function second").unwrap() as u32;
        let mut sym = make_symbol("first", "first", sid(&file, "first", "function"), file);
        sym.range = tr(0, first_end, 0, 2);
        let name_start = source.find("first").unwrap() as u32;
        sym.name_range = tr(name_start, name_start + 5, 0, 0);

        let src_repo = MockSourceRepo::new();
        src_repo.add_file(file, source);
        let mut req = default_request();
        req.source_mode = SourceMode::Full;
        let mut warnings = Vec::new();

        let excerpt = build_source_excerpt(&sym, &src_repo, &req, &mut warnings).unwrap();

        assert_eq!(excerpt.text, "function first() {\n  return 1;\n}");
        assert!(!excerpt.truncated);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn call_evidence_populated_from_relations() {
        let file = fid("src/call.ts");
        let sym_id = sid(&file, "Sub.f", "function");
        let sym = make_symbol("f", "Sub.f", sym_id, file);

        let caller_file = fid("src/caller.ts");
        let caller_id = sid(&caller_file, "Caller.g", "function");
        let caller_sym = make_symbol("g", "Caller.g", caller_id, caller_file);

        let callee_file = fid("src/callee.ts");
        let callee_id = sid(&callee_file, "Callee.h", "function");
        let callee_sym = make_symbol("h", "Callee.h", callee_id, callee_file);

        // ── Symbol repo ────────────────────────────────────────────────
        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_symbol(caller_sym);
        sym_repo.add_symbol(callee_sym);
        sym_repo.add_file(file, "src/call.ts");
        sym_repo.add_file(caller_file, "src/caller.ts");
        sym_repo.add_file(callee_file, "src/callee.ts");

        // ── Relation repo ──────────────────────────────────────────────
        let rel_repo = MockRelationRepo::new();

        // Incoming call: Caller.g → Sub.f
        rel_repo.incoming.borrow_mut().insert(
            sym_id,
            vec![RelationEvidence {
                source_id: caller_id,
                target_id: sym_id,
                relation_kind: InternalRelationKind::Calls,
                file_id: caller_file,
                range: tr(10, 14, 3, 3),
                confidence: Confidence::new(0.95),
            }],
        );

        // Outgoing call: Sub.f → Callee.h
        rel_repo.outgoing.borrow_mut().insert(
            sym_id,
            vec![RelationEvidence {
                source_id: sym_id,
                target_id: callee_id,
                relation_kind: InternalRelationKind::Calls,
                file_id: file,
                range: tr(20, 25, 5, 5),
                confidence: Confidence::new(0.80),
            }],
        );

        // Counts
        let mut inc_map = HashMap::new();
        inc_map.insert(InternalRelationKind::Calls, 1);
        rel_repo.inc_counts.borrow_mut().insert(sym_id, inc_map);

        let mut out_map = HashMap::new();
        out_map.insert(InternalRelationKind::Calls, 1);
        rel_repo.out_counts.borrow_mut().insert(sym_id, out_map);

        let file_repo = MockFileFactsRepo::new();
        let src_repo = MockSourceRepo::new();
        src_repo.add_file(caller_file, "fn g() { f(); }\n");
        src_repo.add_file(file, "fn f() { h(); }\n");

        let req = default_request();

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/call.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        assert_eq!(dossier.call_evidence.incoming.total, 1);
        assert_eq!(dossier.call_evidence.incoming.examples.len(), 1);
        assert_eq!(dossier.call_evidence.incoming.examples[0].symbol.name, "g");
        assert_eq!(
            dossier.call_evidence.incoming.examples[0].edge_kind,
            "calls"
        );
        assert_eq!(
            dossier.call_evidence.incoming.examples[0].confidence,
            "exact"
        );

        assert_eq!(dossier.call_evidence.outgoing.total, 1);
        assert_eq!(dossier.call_evidence.outgoing.examples.len(), 1);
        assert_eq!(dossier.call_evidence.outgoing.examples[0].symbol.name, "h");
    }

    #[test]
    fn semantic_relations_use_opposite_endpoint_and_deduplicate_examples() {
        let file = fid("src/subject.ts");
        let subject_id = sid(&file, "Subject.run", "function");
        let subject = make_symbol("run", "Subject.run", subject_id, file);
        let peer_file = fid("src/peer.ts");
        let peer_id = sid(&peer_file, "Peer", "class");
        let mut peer = make_symbol("Peer", "Peer", peer_id, peer_file);
        peer.kind = SymbolKind::Class;

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(subject.clone());
        sym_repo.add_symbol(peer);
        sym_repo.add_file(file, "src/subject.ts");
        sym_repo.add_file(peer_file, "src/peer.ts");

        let evidence = RelationEvidence {
            source_id: peer_id,
            target_id: subject_id,
            relation_kind: InternalRelationKind::Implements,
            file_id: peer_file,
            range: tr(0, 0, 0, 0),
            confidence: Confidence::new(0.9),
        };
        let rel_repo = MockRelationRepo::new();
        rel_repo
            .incoming
            .borrow_mut()
            .insert(subject_id, vec![evidence.clone(), evidence]);
        rel_repo.inc_counts.borrow_mut().insert(
            subject_id,
            HashMap::from([(InternalRelationKind::Implements, 2)]),
        );

        let dossier = ExploreDossierBuilder::build(
            &subject,
            "src/subject.ts",
            &sym_repo,
            &rel_repo,
            &MockFileFactsRepo::new(),
            &MockSourceRepo::new(),
            &default_request(),
            "exact".into(),
        )
        .unwrap();

        let implementations = dossier.relation_groups.implements.as_ref().unwrap();
        assert_eq!(implementations.total, 2);
        assert_eq!(implementations.examples.len(), 1);
        assert_eq!(implementations.examples[0].symbol.qualified_name, "Peer");
        let json = serde_json::to_value(dossier.relation_groups).unwrap();
        assert!(json.get("implements").is_some());
        assert!(json.get("references").is_none());
        assert!(json.get("referencesType").is_none());
    }

    #[test]
    fn warnings_added_when_relation_queries_fail() {
        let file = fid("src/err.ts");
        let sym = make_symbol("e", "E.e", sid(&file, "E.e", "function"), file);

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_file(file, "src/err.ts");

        // RelationRepo that returns Err — not possible with our mock as-is, but we can simulate
        // by having count_incoming_by_kind return empty maps — that adds a warning.
        let rel_repo = MockRelationRepo::new();

        let file_repo = MockFileFactsRepo::new();
        let src_repo = MockSourceRepo::new();

        let req = default_request();

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/err.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        // Empty relation counts → warning added
        assert!(
            dossier
                .warnings
                .iter()
                .any(|w| w.contains("Relation count data unavailable")),
            "expected relation count warning, got: {:?}",
            dossier.warnings
        );
    }

    #[test]
    fn file_context_excluded_when_flag_false() {
        let file = fid("src/noctx.ts");
        let sym = make_symbol("nc", "NC.nc", sid(&file, "NC.nc", "function"), file);

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_file(file, "src/noctx.ts");

        let rel_repo = MockRelationRepo::new();
        let file_repo = MockFileFactsRepo::new();
        let src_repo = MockSourceRepo::new();

        let mut req = default_request();
        req.include_file_context = false;

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/noctx.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        assert!(dossier.file_context.is_none());
    }

    #[test]
    fn recommendations_generated_based_on_relation_counts() {
        let file = fid("src/rec.ts");
        let sym_id = sid(&file, "R.r", "function");
        let sym = make_symbol("r", "R.r", sym_id, file);

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_file(file, "src/rec.ts");

        let rel_repo = MockRelationRepo::new();
        let mut inc = HashMap::new();
        inc.insert(InternalRelationKind::Calls, 2);
        inc.insert(InternalRelationKind::References, 1);
        rel_repo.inc_counts.borrow_mut().insert(sym_id, inc);

        let mut out = HashMap::new();
        out.insert(InternalRelationKind::Calls, 3);
        rel_repo.out_counts.borrow_mut().insert(sym_id, out);

        let file_repo = MockFileFactsRepo::new();
        let src_repo = MockSourceRepo::new();

        let req = default_request();

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/rec.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        // Has both incoming and outgoing calls → should generate "both" direction
        let has_both = dossier
            .recommended_next_queries
            .iter()
            .any(|r| r.tool == "atlas_calls" && r.args["direction"] == "both");
        assert!(has_both, "expected 'both' direction recommendation");
    }

    #[test]
    fn source_excerpt_truncated_by_max_source_bytes() {
        let file = fid("src/big.ts");
        let sym = make_symbol("big", "B.big", sid(&file, "B.big", "function"), file);

        let sym_repo = MockSymbolRepo::new();
        sym_repo.add_symbol(sym.clone());
        sym_repo.add_file(file, "src/big.ts");

        let rel_repo = MockRelationRepo::new();
        let file_repo = MockFileFactsRepo::new();

        // Create content larger than max_source_bytes
        let huge = "x".repeat(200);
        let src_repo = MockSourceRepo::new();
        src_repo.add_file(file, &huge);

        let mut req = default_request();
        req.max_source_bytes = 100;
        req.source_mode = SourceMode::Full;

        let dossier = ExploreDossierBuilder::build(
            &sym,
            "src/big.ts",
            &sym_repo,
            &rel_repo,
            &file_repo,
            &src_repo,
            &req,
            "exact".to_string(),
        )
        .unwrap();

        let excerpt = dossier.source_excerpt.unwrap();
        assert!(excerpt.truncated);
        assert!(excerpt.text.len() <= 100);
        assert!(
            dossier.warnings.iter().any(|w| w.contains("truncated")),
            "expected truncation warning"
        );
    }
}
