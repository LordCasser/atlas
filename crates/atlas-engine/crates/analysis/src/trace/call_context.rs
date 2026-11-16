//! Callsite restrictions for one backward value trace. These are semantic
//! scope constraints on recorded calls, not proof of runtime execution.

use std::{collections::HashMap, ops::Deref};

use db::Store;
use types::{CallsiteId, DataFlowEdge, DataFlowKind, DataNode, SymbolId};

pub(super) struct Candidate {
    pub edge: DataFlowEdge,
    pub callsite_id: Option<CallsiteId>,
    pub virtual_edge: bool,
    pub context: Option<Vec<CallsiteId>>,
    pub excluded: bool,
}

impl Deref for Candidate {
    type Target = DataFlowEdge;
    fn deref(&self) -> &Self::Target {
        &self.edge
    }
}

pub(super) enum Transition {
    Follow(Vec<CallsiteId>),
    Excluded,
    Unavailable,
}

pub(super) struct CallContexts<'a> {
    store: &'a Store,
    calls: HashMap<CallsiteId, Vec<(SymbolId, SymbolId)>>,
}

impl<'a> CallContexts<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            calls: HashMap::new(),
        }
    }

    fn links(&mut self, id: &CallsiteId) -> anyhow::Result<&[(SymbolId, SymbolId)]> {
        if !self.calls.contains_key(id) {
            let mut links: Vec<_> = self
                .store
                .find_resolved_callsites_by_id(id)?
                .into_iter()
                .map(|r| (r.callsite.caller, r.callee))
                .collect();
            links.sort();
            links.dedup();
            self.calls.insert(*id, links);
        }
        Ok(&self.calls[id])
    }

    pub fn valid_for(&mut self, node: &DataNode, context: &[CallsiteId]) -> anyhow::Result<bool> {
        if context.is_empty() {
            return Ok(true);
        }
        let Some(owner) = node.function_id else {
            return Ok(false);
        };
        let mut callees = Vec::new();
        for (depth, id) in context.iter().enumerate() {
            let next: Vec<_> = self
                .links(id)?
                .iter()
                .filter(|(caller, _)| depth == 0 || callees.contains(caller))
                .map(|(_, callee)| *callee)
                .collect();
            if next.is_empty() {
                return Ok(false);
            }
            callees = next;
        }
        Ok(callees.contains(&owner))
    }

    pub fn transition(
        &mut self,
        edge: &Candidate,
        target: &DataNode,
        source: &DataNode,
        context: &[CallsiteId],
    ) -> anyhow::Result<Transition> {
        let mut next = context.to_vec();
        match edge.kind {
            DataFlowKind::ArgToParam
            | DataFlowKind::ReturnToCall
            | DataFlowKind::WritebackToCall => {
                // Stored cross-function edges can use their call node's
                // explicit identity. A virtual provider supplies its own
                // boundary; never infer it from an unrelated nested call.
                let id = edge.callsite_id.or_else(|| {
                    if edge.virtual_edge {
                        None
                    } else {
                        match edge.kind {
                            DataFlowKind::ArgToParam
                                if source.kind == types::DataNodeKind::CallArg =>
                            {
                                source.callsite_id
                            }
                            DataFlowKind::ReturnToCall | DataFlowKind::WritebackToCall => {
                                target.callsite_id
                            }
                            _ => None,
                        }
                    }
                });
                let Some(id) = id else {
                    return Ok(Transition::Unavailable);
                };
                let (Some(from), Some(to)) = (source.function_id, target.function_id) else {
                    return Ok(Transition::Unavailable);
                };
                let returning = matches!(
                    edge.kind,
                    DataFlowKind::ReturnToCall | DataFlowKind::WritebackToCall
                );
                let relation = if returning { (to, from) } else { (from, to) };
                if !self.links(&id)?.contains(&relation) || !self.valid_for(target, context)? {
                    return Ok(Transition::Unavailable);
                }
                if returning {
                    next.push(id);
                } else if let Some(entered) = next.last() {
                    if entered != &id {
                        return Ok(Transition::Excluded);
                    }
                    next.pop();
                }
            }
            // A shared-state writer need not run in the reader's invocation.
            // Its explicit state boundary diagnostic remains with the trace.
            DataFlowKind::StateFlow => next.clear(),
            _ => {
                if !self.valid_for(source, context)? {
                    return Ok(Transition::Unavailable);
                }
            }
        }
        Ok(Transition::Follow(next))
    }
}
