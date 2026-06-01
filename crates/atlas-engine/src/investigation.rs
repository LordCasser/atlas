//! Investigation context types shared between engine and MCP layers.
//!
//! Investigation represents an MCP-session-scoped analysis focus. The engine
//! uses it to prioritize extraction jobs (files/symbols related to the
//! investigation are processed first).

use types::ids::{FileId, SymbolId};
use types::structs::CapabilityMask;

/// An active investigation — connects the user's current analysis focus to the
/// lazy extraction scheduler for priority-based job ordering.
#[derive(Debug, Clone)]
pub struct Investigation {
    /// The focus of the investigation (what the user is querying).
    pub focus: InvestigationFocus,
    /// Related symbols discovered during the investigation.
    pub related_symbols: Vec<SymbolId>,
    /// Related files discovered during the investigation.
    pub related_files: Vec<FileId>,
    /// Desired extraction capabilities for full analysis.
    pub desired_capabilities: CapabilityMask,
}

/// What the user's current investigation is targeting.
#[derive(Debug, Clone)]
pub enum InvestigationFocus {
    /// A specific symbol (function, struct, class, etc.).
    Symbol(SymbolId),
    /// A specific struct/class field.
    Field {
        struct_sym: SymbolId,
        field_path: String,
    },
    /// A specific source position.
    Position {
        file_id: FileId,
        line: u32,
        col: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::FileId;
    use types::ids::SymbolId;
    use types::structs::CapabilityMask;

    #[test]
    fn investigation_focus_symbol_roundtrip() {
        let fid = FileId::generate("test.rs");
        let sid = SymbolId::generate(&fid, "typescript", "my_func", "function", None);
        let focus = InvestigationFocus::Symbol(sid);
        match focus {
            InvestigationFocus::Symbol(id) => assert_eq!(id, sid),
            _ => panic!("expected Symbol"),
        }
    }

    #[test]
    fn investigation_focus_position() {
        let fid = FileId::generate("test.rs");
        let focus = InvestigationFocus::Position {
            file_id: fid,
            line: 42,
            col: 10,
        };
        match focus {
            InvestigationFocus::Position { file_id, line, col } => {
                assert_eq!(file_id, fid);
                assert_eq!(line, 42);
                assert_eq!(col, 10);
            }
            _ => panic!("expected Position"),
        }
    }

    #[test]
    fn investigation_focus_field() {
        let fid = FileId::generate("test.rs");
        let sid = SymbolId::generate(&fid, "typescript", "MyStruct", "struct", None);
        let focus = InvestigationFocus::Field {
            struct_sym: sid,
            field_path: "data->state.aptr.cookiehost".into(),
        };
        match focus {
            InvestigationFocus::Field {
                struct_sym,
                field_path,
            } => {
                assert_eq!(struct_sym, sid);
                assert_eq!(field_path, "data->state.aptr.cookiehost");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn investigation_default() {
        let fid = FileId::generate("test.rs");
        let sid = SymbolId::generate(&fid, "typescript", "main", "function", None);
        let investigation = Investigation {
            focus: InvestigationFocus::Symbol(sid),
            related_symbols: vec![],
            related_files: vec![],
            desired_capabilities: CapabilityMask::default(),
        };
        assert!(investigation.related_symbols.is_empty());
        assert!(investigation.related_files.is_empty());
    }
}
