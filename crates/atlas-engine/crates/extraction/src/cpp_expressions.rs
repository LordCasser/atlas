//! C++ expression boundaries shared by structural extraction and lazy inspection.
//! These helpers do not select operator targets or prove evaluation/completeness.
use tree_sitter::Node;
use types::TextRange;

/// The callable whose scope contains a selected expression. An init-capture's
/// RHS belongs to the surrounding scope; its name and closure body do not.
/// This is lexical ownership, not a claim that the expression is evaluated.
pub fn expression_function(root: Node<'_>, at: TextRange) -> Option<Node<'_>> {
    let selected = root.descendant_for_byte_range(at.start_byte as usize, at.end_byte as usize)?;
    std::iter::successors(Some(selected), |node| node.parent()).find(|node| {
        node.kind() == "function_definition"
            || (node.kind() == "lambda_expression" && !in_capture_initializer(*node, at))
    })
}

// An init-capture's initializer is evaluated when the closure is created, in
// the surrounding scope. Its declared capture name and the closure body belong
// to the closure. Only a selection within the written RHS can skip that scope.
fn in_capture_initializer(lambda: Node<'_>, at: TextRange) -> bool {
    let Some(captures) = lambda.child_by_field_name("captures") else {
        return false;
    };
    if captures.start_byte() > at.start_byte as usize || captures.end_byte() < at.end_byte as usize
    {
        return false;
    }
    let selected = captures.descendant_for_byte_range(at.start_byte as usize, at.end_byte as usize);
    std::iter::successors(selected, |node| node.parent())
        .take_while(|node| node.id() != captures.id())
        .any(|node| {
            node.kind() == "lambda_capture_initializer"
                && node.child_by_field_name("right").is_some_and(|right| {
                    right.start_byte() <= at.start_byte as usize
                        && at.end_byte as usize <= right.end_byte()
                })
        })
}

/// Known unevaluated operands do not contribute invocation regions. A nested
/// function/closure body is investigated independently of its enclosing operand.
pub fn may_be_evaluated(node: Node<'_>, source: &str) -> bool {
    for parent in std::iter::successors(node.parent(), |node| node.parent()) {
        if matches!(parent.kind(), "function_definition" | "lambda_expression")
            && parent.child_by_field_name("body").is_some_and(|body| {
                body.start_byte() <= node.start_byte() && node.end_byte() <= body.end_byte()
            })
        {
            break;
        }
        if matches!(
            parent.kind(),
            "sizeof_expression" | "decltype" | "noexcept" | "requires_expression"
        ) {
            return false;
        }
        // The pinned grammar represents expression-form noexcept as a call.
        if parent.kind() == "call_expression"
            && parent
                .child_by_field_name("function")
                .and_then(|callee| callee.utf8_text(source.as_bytes()).ok())
                == Some("noexcept")
        {
            return false;
        }
    }
    true
}

/// Regions for which ordinary call-reference lookup does not analyze operator,
/// conversion or implicit invocation semantics. Built-in operations may occur;
/// this reports work not done, never a missing or selected function call.
pub fn unexamined_invocation_region(node: Node<'_>, source: &str) -> Option<TextRange> {
    let kind = node.kind();
    if !may_be_evaluated(node, source) {
        return None;
    }
    if kind == "for_range_loop" {
        // The body's written calls are independent of the implicit iteration
        // operations. Preserve exactly the header, including the closing ')'.
        let mut cursor = node.walk();
        let close = node
            .children(&mut cursor)
            .find(|child| child.kind() == ")")?;
        let mut range = crate::languages::node_range(node);
        let end = crate::languages::node_range(close);
        range.end_byte = end.end_byte;
        range.end_line = end.end_line;
        range.end_column = end.end_column;
        return Some(range);
    }
    if !kind.ends_with("_expression")
        || matches!(
            kind,
            // Calls/receivers, callable containment and allocation already have
            // their own facts. Parentheses add no operation of their own.
            "call_expression"
                | "field_expression"
                | "lambda_expression"
                | "parenthesized_expression"
                | "new_expression"
                | "sizeof_expression"
                | "requires_expression"
        )
    {
        return None;
    }
    Some(crate::languages::node_range(node))
}
