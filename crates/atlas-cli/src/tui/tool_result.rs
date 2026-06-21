//! Human-oriented projection of shared MCP tool responses.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy)]
enum FactKind {
    Heading,
    Fact,
    Code,
    Diagnostic,
}

#[derive(Debug, Clone)]
struct FactLine {
    text: String,
    kind: FactKind,
}

#[derive(Debug, Clone, Default)]
struct ResultHud {
    status: String,
    context: Vec<String>,
    summary: Option<String>,
    signals: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ToolResultView {
    raw: String,
    hud: ResultHud,
    facts: Vec<FactLine>,
    query_id: Option<String>,
}

impl ToolResultView {
    pub fn from_text(text: String, is_error: bool) -> Self {
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Self {
                raw: text.clone(),
                hud: ResultHud {
                    status: if is_error { "Error" } else { "Text" }.into(),
                    ..ResultHud::default()
                },
                facts: vec![FactLine {
                    text,
                    kind: if is_error {
                        FactKind::Diagnostic
                    } else {
                        FactKind::Fact
                    },
                }],
                query_id: None,
            };
        };

        let query_id = value
            .get("query_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let hud = build_hud(&value, is_error, query_id.as_deref());
        let mut facts = Vec::new();
        match &value {
            Value::Object(object) => format_object_fields(object, 0, true, None, &mut facts),
            other => facts.push(FactLine {
                text: scalar_text(other),
                kind: FactKind::Fact,
            }),
        }
        append_diagnostics(&value, &mut facts);
        if facts.is_empty() {
            facts.push(FactLine {
                text: "No code facts returned.".into(),
                kind: FactKind::Fact,
            });
        }

        Self {
            raw: serde_json::to_string_pretty(&value).unwrap_or(text),
            hud,
            facts,
            query_id,
        }
    }

    pub fn query_id(&self) -> Option<&str> {
        self.query_id.as_deref()
    }

    #[cfg(test)]
    fn hud_text(&self) -> String {
        std::iter::once(self.hud.status.as_str())
            .chain(self.hud.context.iter().map(String::as_str))
            .chain(self.hud.summary.iter().map(String::as_str))
            .chain(self.hud.signals.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[cfg(test)]
    fn fact_text(&self) -> String {
        self.facts
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn render(
    frame: &mut ratatui::Frame,
    area: Rect,
    tool_name: &str,
    view: &ToolResultView,
    raw: bool,
    scroll: u16,
) {
    if raw {
        let title = format!(" {} raw | r facts | x close ", humanize_key(tool_name));
        frame.render_widget(
            Paragraph::new(view.raw.as_str())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(title)
                        .border_style(Style::default().fg(Color::DarkGray)),
                )
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0)),
            area,
        );
        return;
    }

    let hud_height = (hud_line_count(&view.hud) as u16 + 2).min(5);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(hud_height), Constraint::Min(3)])
        .split(area);
    render_hud(frame, rows[0], tool_name, &view.hud);
    render_facts(frame, rows[1], &view.facts, scroll);
}

fn render_hud(frame: &mut ratatui::Frame, area: Rect, tool_name: &str, hud: &ResultHud) {
    let status_color = match hud.status.as_str() {
        "Error" => Color::Red,
        "Refining" | "Partial" | "Limited" => Color::Yellow,
        _ => Color::Green,
    };
    let context_items = if area.width < 72 {
        &hud.context[..hud.context.len().min(2)]
    } else {
        hud.context.as_slice()
    };
    let context = context_items.join("  |  ");
    let signals = hud.signals.join("  |  ");
    let line_width = area.width.saturating_sub(2) as usize;
    let mut lines = Vec::with_capacity(3);
    if !context.is_empty() {
        lines.push(Line::from(fit_text(&context, line_width)));
    }
    if let Some(summary) = &hud.summary {
        lines.push(Line::from(Span::styled(
            fit_text(summary, line_width),
            Style::default().fg(Color::White),
        )));
    }
    if !signals.is_empty() {
        lines.push(Line::from(Span::styled(
            fit_text(&signals, line_width),
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(Line::from(
            vec![
                    Span::styled(
                        format!(" {} ", humanize_key(tool_name)),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {} ", hud.status),
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ],
        ))),
        area,
    );
}

fn hud_line_count(hud: &ResultHud) -> usize {
    usize::from(!hud.context.is_empty())
        + usize::from(hud.summary.is_some())
        + usize::from(!hud.signals.is_empty())
}

fn render_facts(frame: &mut ratatui::Frame, area: Rect, facts: &[FactLine], scroll: u16) {
    let lines = facts
        .iter()
        .map(|line| {
            let style = match line.kind {
                FactKind::Heading => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                FactKind::Fact => Style::default().fg(Color::White),
                FactKind::Code => Style::default().fg(Color::Green),
                FactKind::Diagnostic => Style::default().fg(Color::Yellow),
            };
            Line::styled(line.text.clone(), style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Code facts | j/k scroll | r raw | x close "),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn build_hud(value: &Value, is_error: bool, query_id: Option<&str>) -> ResultHud {
    let object = value.as_object();
    let analysis = object
        .and_then(|root| root.get("analysis"))
        .and_then(Value::as_object);
    let partial = object
        .and_then(|root| root.get("partial_result"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_gaps = object
        .and_then(|root| root.get("gaps"))
        .and_then(Value::as_array)
        .is_some_and(|gaps| !gaps.is_empty());
    let refining = analysis
        .and_then(|item| item.get("retry_after_ms"))
        .is_some_and(|retry| !retry.is_null());
    let status = if is_error
        || object
            .and_then(|root| root.get("ok"))
            .and_then(Value::as_bool)
            == Some(false)
    {
        "Error"
    } else if refining {
        "Refining"
    } else if partial {
        "Partial"
    } else if has_gaps {
        "Limited"
    } else {
        "Complete"
    };

    let mut context = Vec::new();
    if let Some(scope) = analysis
        .and_then(|item| item.get("scope"))
        .and_then(Value::as_str)
    {
        context.push(format!("Scope {scope}"));
    }
    if let Some(basis) = analysis
        .and_then(|item| item.get("basis"))
        .and_then(Value::as_array)
    {
        let basis = basis
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" + ");
        if !basis.is_empty() {
            context.push(format!("Basis {basis}"));
        }
    }
    for key in ["kind", "direction"] {
        if let Some(text) = object
            .and_then(|root| root.get(key))
            .and_then(Value::as_str)
        {
            context.push(format!("{} {}", humanize_key(key), humanize_value(text)));
        }
    }

    let mut signals = Vec::new();
    if let Some(root) = object {
        append_capability_signals(root.get("capability"), &mut signals);
    }
    if let Some(coverage) = object
        .and_then(|root| root.get("coverage_counts"))
        .and_then(Value::as_object)
    {
        for (key, value) in coverage {
            signals.push(format!("{} {}", humanize_key(key), scalar_text(value)));
        }
    }
    if let Some(root) = object {
        let shown = root.get("shown").and_then(Value::as_u64);
        let total = root
            .get("total_reached")
            .or_else(|| root.get("total_nodes_visited"))
            .or_else(|| root.get("total"))
            .and_then(Value::as_u64);
        if let (Some(shown), Some(total)) = (shown, total) {
            signals.push(format!("Result {shown} / {total}"));
        } else if let Some(total) = total {
            signals.push(format!("Results {total}"));
        }
        if root.get("truncated").and_then(Value::as_bool) == Some(true) {
            signals.push("Truncated".into());
        }
        let warning_count = ["warnings", "diagnostics", "gaps"]
            .iter()
            .filter_map(|key| root.get(*key).and_then(Value::as_array))
            .map(Vec::len)
            .sum::<usize>();
        if warning_count > 0 {
            signals.push(format!("Diagnostics {warning_count}"));
        }
    }
    if query_id.is_some() {
        signals.push("Resume ready".into());
    }

    ResultHud {
        status: status.into(),
        context,
        summary: analysis
            .and_then(|item| item.get("summary"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        signals,
    }
}

fn append_capability_signals(capability: Option<&Value>, signals: &mut Vec<String>) {
    let Some(capability) = capability.and_then(Value::as_object) else {
        return;
    };
    for key in ["language", "level"] {
        if let Some(value) = capability.get(key).and_then(Value::as_str) {
            signals.push(humanize_value(value));
        }
    }
    if let Some(confidence) = capability.get("confidence").and_then(Value::as_f64) {
        let percent = if confidence <= 1.0 {
            confidence * 100.0
        } else {
            confidence
        };
        signals.push(format!("Confidence {percent:.0}%"));
    }
}

fn format_object_fields(
    object: &Map<String, Value>,
    depth: usize,
    root: bool,
    inherited_file: Option<&str>,
    lines: &mut Vec<FactLine>,
) {
    let current_file = object
        .get("file")
        .or_else(|| object.get("file_path"))
        .and_then(Value::as_str)
        .or(inherited_file);
    if let Some(identity) = compact_identity(object, depth, current_file, !root) {
        lines.push(identity);
    }
    let mut fields = object.iter().collect::<Vec<_>>();
    if root {
        fields.sort_by_key(|(key, _)| root_fact_priority(key));
    }
    for (key, value) in fields {
        if (root && is_root_metadata(key)) || is_identity_key(key, object, !root) {
            continue;
        }
        format_named_value(key, value, depth, current_file, lines);
    }
}

fn format_named_value(
    key: &str,
    value: &Value,
    depth: usize,
    inherited_file: Option<&str>,
    lines: &mut Vec<FactLine>,
) {
    let indent = "  ".repeat(depth);
    let label = humanize_key(key);
    if matches!(key, "sourceExcerpt" | "source_excerpt") {
        if let Some(object) = value.as_object() {
            format_source_excerpt(object, depth, lines);
            return;
        }
    }
    match value {
        Value::Null => {}
        Value::Bool(_) | Value::Number(_) => lines.push(FactLine {
            text: format!("{indent}{label}: {}", scalar_text(value)),
            kind: FactKind::Fact,
        }),
        Value::String(text) if text.contains('\n') || key.eq_ignore_ascii_case("source") => {
            lines.push(FactLine {
                text: format!("{indent}{label}"),
                kind: FactKind::Heading,
            });
            for source_line in text.lines() {
                lines.push(FactLine {
                    text: format!("{indent}  {source_line}"),
                    kind: FactKind::Code,
                });
            }
        }
        Value::String(text) => lines.push(FactLine {
            text: format!("{indent}{label}: {text}"),
            kind: FactKind::Fact,
        }),
        Value::Array(items) if items.is_empty() => lines.push(FactLine {
            text: format!("{indent}{label}: none"),
            kind: FactKind::Fact,
        }),
        Value::Array(items) if items.iter().all(is_scalar) => lines.push(FactLine {
            text: format!(
                "{indent}{label}: {}",
                items.iter().map(scalar_text).collect::<Vec<_>>().join(", ")
            ),
            kind: FactKind::Fact,
        }),
        Value::Array(items) => {
            lines.push(FactLine {
                text: format!("{indent}{label} ({})", items.len()),
                kind: FactKind::Heading,
            });
            for item in items {
                match item {
                    Value::Object(object) => {
                        if compact_identity(object, depth + 1, inherited_file, true).is_none() {
                            lines.push(FactLine {
                                text: format!("{}-", "  ".repeat(depth + 1)),
                                kind: FactKind::Fact,
                            });
                        }
                        format_object_fields(object, depth + 1, false, inherited_file, lines)
                    }
                    other => lines.push(FactLine {
                        text: format!("{}- {}", "  ".repeat(depth + 1), scalar_text(other)),
                        kind: FactKind::Fact,
                    }),
                }
            }
        }
        Value::Object(object) => {
            lines.push(FactLine {
                text: format!("{indent}{label}"),
                kind: FactKind::Heading,
            });
            format_object_fields(object, depth + 1, false, inherited_file, lines);
        }
    }
}

fn compact_identity(
    object: &Map<String, Value>,
    depth: usize,
    inherited_file: Option<&str>,
    nested: bool,
) -> Option<FactLine> {
    if let (Some(pattern), Some(rule_kind)) = (
        object.get("pattern").and_then(Value::as_str),
        object.get("rule_kind").and_then(Value::as_str),
    ) {
        let mut text = format!(
            "{}- {pattern}  [{}]",
            "  ".repeat(depth),
            humanize_value(rule_kind)
        );
        if let Some(language) = object.get("language").and_then(Value::as_str) {
            text.push_str(&format!("  {language}"));
        }
        append_confidence(object, &mut text);
        return Some(FactLine {
            text,
            kind: FactKind::Fact,
        });
    }
    if let Some(field) = object.get("field_qname").and_then(Value::as_str) {
        let mut text = format!("{}- {field}", "  ".repeat(depth));
        if let Some(target) = object.get("target_qname").and_then(Value::as_str) {
            text.push_str(&format!(" -> {target}"));
        }
        append_confidence(object, &mut text);
        return Some(FactLine {
            text,
            kind: FactKind::Fact,
        });
    }
    if let Some(task_id) = nested.then(|| {
        object
            .get("task_id")
            .or_else(|| object.get("query_id"))
            .and_then(Value::as_str)
    })? {
        let status = object
            .get("status")
            .and_then(Value::as_str)
            .map(humanize_value)
            .unwrap_or_else(|| "task".into());
        return Some(FactLine {
            text: format!("{}- {task_id}  [{status}]", "  ".repeat(depth)),
            kind: FactKind::Fact,
        });
    }
    if let (Some(imported_name), Some(module)) = (
        object.get("imported_name").and_then(Value::as_str),
        object.get("module").and_then(Value::as_str),
    ) {
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .map(humanize_value)
            .unwrap_or_else(|| "import".into());
        return Some(FactLine {
            text: format!(
                "{}- {imported_name}  [{kind}]  from {module}",
                "  ".repeat(depth)
            ),
            kind: FactKind::Fact,
        });
    }
    let name = object
        .get("qualified_name")
        .or_else(|| object.get("qualifiedName"))
        .or_else(|| object.get("name"))
        .and_then(Value::as_str);
    let kind = object.get("kind").and_then(Value::as_str);
    let file = object
        .get("file")
        .or_else(|| object.get("file_path"))
        .and_then(Value::as_str)
        .or(inherited_file);
    let line = object
        .get("line")
        .and_then(Value::as_u64)
        .or_else(|| nested_line(object.get("range")));
    let Some(name) = name else {
        if nested {
            if let Some(file) = object
                .get("file")
                .or_else(|| object.get("file_path"))
                .and_then(Value::as_str)
            {
                return Some(FactLine {
                    text: format!("{}- {file}", "  ".repeat(depth)),
                    kind: FactKind::Fact,
                });
            }
            if let Some(depth_value) = object.get("depth").and_then(Value::as_u64) {
                return Some(FactLine {
                    text: format!("{}- Depth {depth_value}", "  ".repeat(depth)),
                    kind: FactKind::Fact,
                });
            }
        }
        return None;
    };
    let mut text = format!("{}- {name}", "  ".repeat(depth));
    if let Some(kind) = kind {
        text.push_str(&format!("  [{}]", humanize_value(kind)));
    }
    if let Some(file) = file {
        text.push_str(&format!("  {file}"));
        if let Some(line) = line {
            text.push_str(&format!(":{line}"));
        }
    }
    Some(FactLine {
        text,
        kind: FactKind::Fact,
    })
}

fn nested_line(range: Option<&Value>) -> Option<u64> {
    let range = range?.as_object()?;
    range
        .get("line")
        .or_else(|| range.get("start_line"))
        .or_else(|| range.get("startLine"))
        .and_then(Value::as_u64)
}

fn is_identity_key(key: &str, object: &Map<String, Value>, nested: bool) -> bool {
    let rule = object.get("pattern").and_then(Value::as_str).is_some()
        && object.get("rule_kind").and_then(Value::as_str).is_some();
    if rule && matches!(key, "pattern" | "rule_kind" | "language" | "confidence") {
        return true;
    }
    let dispatch = object.get("field_qname").and_then(Value::as_str).is_some();
    if dispatch && matches!(key, "field_qname" | "target_qname" | "confidence") {
        return true;
    }
    let task = object
        .get("task_id")
        .or_else(|| object.get("query_id"))
        .and_then(Value::as_str)
        .is_some();
    if task && matches!(key, "task_id" | "query_id" | "status") {
        return true;
    }
    let dependency = object
        .get("imported_name")
        .and_then(Value::as_str)
        .is_some()
        && object.get("module").and_then(Value::as_str).is_some();
    if dependency && matches!(key, "imported_name" | "module" | "kind" | "line") {
        return true;
    }
    if nested
        && object
            .get("qualified_name")
            .or_else(|| object.get("qualifiedName"))
            .or_else(|| object.get("name"))
            .is_none()
    {
        if matches!(key, "file" | "file_path")
            && object
                .get("file")
                .or_else(|| object.get("file_path"))
                .and_then(Value::as_str)
                .is_some()
        {
            return true;
        }
        if key == "depth" && object.get("depth").and_then(Value::as_u64).is_some() {
            return true;
        }
    }
    compact_identity(object, 0, None, nested).is_some()
        && matches!(
            key,
            "qualified_name"
                | "qualifiedName"
                | "name"
                | "kind"
                | "file"
                | "file_path"
                | "line"
                | "range"
                | "id"
                | "symbol_ref"
        )
}

fn append_confidence(object: &Map<String, Value>, text: &mut String) {
    let Some(confidence) = object.get("confidence").and_then(Value::as_f64) else {
        return;
    };
    let percent = if confidence <= 1.0 {
        confidence * 100.0
    } else {
        confidence
    };
    text.push_str(&format!("  confidence {percent:.0}%"));
}

fn root_fact_priority(key: &str) -> u8 {
    match key {
        "subject" => 0,
        "source" | "sourceExcerpt" | "source_excerpt" => 1,
        "result" | "path" | "steps" | "hops" | "file_groups" => 2,
        "semantic_impact" | "invariants_affected" | "lifecycle_paths_affected" => 3,
        "callEvidence" | "call_evidence" | "relationGroups" | "relation_groups" | "callers"
        | "callees" | "incoming" | "outgoing" => 4,
        "fileContext" | "file_context" | "dependencies" | "dependents" => 6,
        "recommendedNextQueries" | "recommended_next_queries" => 7,
        _ => 5,
    }
}

fn format_source_excerpt(object: &Map<String, Value>, depth: usize, lines: &mut Vec<FactLine>) {
    let start = object
        .get("startLine")
        .or_else(|| object.get("start_line"))
        .and_then(Value::as_u64);
    let end = object
        .get("endLine")
        .or_else(|| object.get("end_line"))
        .and_then(Value::as_u64);
    let label = match (start, end) {
        (Some(start), Some(end)) => format!("Source (lines {start}-{end})"),
        (Some(start), None) => format!("Source (line {start})"),
        _ => "Source".into(),
    };
    let indent = "  ".repeat(depth);
    lines.push(FactLine {
        text: format!("{indent}{label}"),
        kind: FactKind::Heading,
    });
    if let Some(text) = object.get("text").and_then(Value::as_str) {
        for source_line in text.lines() {
            lines.push(FactLine {
                text: format!("{indent}  {source_line}"),
                kind: FactKind::Code,
            });
        }
    }
    if object.get("truncated").and_then(Value::as_bool) == Some(true) {
        lines.push(FactLine {
            text: format!("{indent}  [source truncated]"),
            kind: FactKind::Diagnostic,
        });
    }
}

fn is_root_metadata(key: &str) -> bool {
    matches!(
        key,
        "analysis"
            | "coverage_counts"
            | "capability"
            | "partial_result"
            | "diagnostics"
            | "warnings"
            | "gaps"
            | "query_id"
            | "ok"
            | "truncated"
            | "shown"
            | "total_reached"
            | "total_nodes_visited"
            | "total"
            | "kind"
            | "direction"
            | "max_depth"
            | "bfs_limit"
            | "capability_note"
            | "noise_note"
            | "note"
            | "resolution"
            | "edge_kinds_used"
            | "include_children"
    )
}

fn append_diagnostics(value: &Value, lines: &mut Vec<FactLine>) {
    let Some(root) = value.as_object() else {
        return;
    };
    let mut messages = Vec::new();
    for key in ["warnings", "diagnostics", "gaps"] {
        if let Some(value) = root.get(key) {
            collect_messages(value, &mut messages);
        }
    }
    for key in ["note", "capability_note", "noise_note"] {
        if let Some(message) = root.get(key).and_then(Value::as_str) {
            messages.push(message.to_owned());
        }
    }
    if messages.is_empty() {
        return;
    }
    lines.push(FactLine {
        text: "Diagnostics".into(),
        kind: FactKind::Heading,
    });
    messages.sort();
    messages.dedup();
    lines.extend(messages.into_iter().map(|message| FactLine {
        text: format!("  ! {message}"),
        kind: FactKind::Diagnostic,
    }));
}

fn collect_messages(value: &Value, messages: &mut Vec<String>) {
    match value {
        Value::String(text) => messages.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                collect_messages(item, messages);
            }
        }
        Value::Object(object) => {
            if let Some(message) = object
                .get("message")
                .or_else(|| object.get("detail"))
                .or_else(|| object.get("reason"))
                .and_then(Value::as_str)
            {
                messages.push(message.to_owned());
            }
        }
        _ => {}
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "none".into(),
        Value::Bool(value) => if *value { "yes" } else { "no" }.into(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn humanize_value(value: &str) -> String {
    value.replace(['_', '-'], " ")
}

fn humanize_key(key: &str) -> String {
    let mut words = String::with_capacity(key.len() + 4);
    for (index, ch) in key.chars().enumerate() {
        if ch == '_' || ch == '-' {
            words.push(' ');
        } else if ch.is_ascii_uppercase() && index > 0 {
            words.push(' ');
            words.push(ch.to_ascii_lowercase());
        } else {
            words.push(ch.to_ascii_lowercase());
        }
    }
    if let Some(first) = words.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    words
}

fn fit_text(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.into();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    text.chars().take(width - 3).collect::<String>() + "..."
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn rendered(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn impact_projects_metadata_to_hud_and_code_facts_to_body() {
        let view = ToolResultView::from_text(
            serde_json::json!({
                "analysis": {
                    "scope": "local",
                    "basis": ["manifest", "structural"],
                    "retry_after_ms": 5000,
                    "summary": "Focus analysis is expanding."
                },
                "coverage_counts": {"seed_file": 1},
                "query_id": "q_123",
                "shown": 1,
                "total_reached": 3,
                "file_groups": [{
                    "file": "src/lib.rs",
                    "symbols": [{
                        "kind": "function",
                        "line": 42,
                        "name": "run",
                        "qualified_name": "crate::run"
                    }]
                }]
            })
            .to_string(),
            false,
        );

        assert_eq!(view.query_id(), Some("q_123"));
        assert!(view.hud_text().contains("Refining"));
        assert!(view.hud_text().contains("manifest + structural"));
        assert!(view.hud_text().contains("Seed file 1"));
        assert!(view.hud_text().contains("1 / 3"));
        assert!(view.fact_text().contains("crate::run"));
        assert!(view.fact_text().contains("src/lib.rs:42"));
        assert!(!view.fact_text().contains("retry_after_ms"));
        assert!(!view.fact_text().contains("query_id"));
        assert!(!view.fact_text().contains("q_123"));
        assert!(view.fact_text().contains("- src/lib.rs"));
        assert!(!view.fact_text().lines().any(|line| line.trim() == "-"));
    }

    #[test]
    fn trace_capability_and_confidence_are_hud_signals() {
        let view = ToolResultView::from_text(
            serde_json::json!({
                "ok": true,
                "kind": "variable",
                "capability": {
                    "language": "rust",
                    "level": "dataflow_full",
                    "confidence": 0.92
                },
                "partial_result": false,
                "result": {
                    "steps": [{"file": "src/lib.rs", "line": 7, "name": "value"}]
                }
            })
            .to_string(),
            false,
        );

        assert!(view.hud_text().contains("rust"));
        assert!(view.hud_text().contains("dataflow full"));
        assert!(view.hud_text().contains("92%"));
        assert!(view.fact_text().contains("src/lib.rs:7"));
        assert!(!view.fact_text().contains("capability"));
    }

    #[test]
    fn unknown_non_metadata_fields_remain_visible() {
        let view =
            ToolResultView::from_text(r#"{"future_fact":{"meaning":"preserved"}}"#.into(), false);

        assert!(view.fact_text().contains("Future fact"));
        assert!(view.fact_text().contains("Meaning: preserved"));
    }

    #[test]
    fn explore_prioritizes_subject_and_source_over_file_inventory() {
        let view = ToolResultView::from_text(
            serde_json::json!({
                "callEvidence": {"incoming": {"total": 0}},
                "fileContext": {"imports": [{"module": "std::sync::Arc", "imported_name": "Arc", "kind": "use"}]},
                "sourceExcerpt": {"startLine": 10, "endLine": 12, "text": "struct Item {\n    value: u8,\n}"},
                "subject": {"qualifiedName": "crate::Item", "kind": "struct", "file": "src/lib.rs", "line": 10}
            })
            .to_string(),
            false,
        );
        let facts = view.fact_text();

        assert!(facts.find("crate::Item").unwrap() < facts.find("Source (lines 10-12)").unwrap());
        assert!(facts.find("Source (lines 10-12)").unwrap() < facts.find("Call evidence").unwrap());
        assert!(facts.contains("- Arc  [use]  from std::sync::Arc"));
    }

    #[test]
    fn plain_text_error_stays_readable() {
        let view = ToolResultView::from_text("Symbol is required".into(), true);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), "trace", &view, false, 0))
            .unwrap();
        let output = rendered(&terminal);

        assert!(view.hud_text().contains("Error"));
        assert!(view.fact_text().contains("Symbol is required"));
        assert!(output.contains("Error"));
        assert!(output.contains("Code facts"));
        assert!(!output.contains("Trace raw"));
    }

    #[test]
    fn simple_sync_result_does_not_invent_empty_hud_messages() {
        let view = ToolResultView::from_text(r#"{"rules":[],"total":0}"#.into(), false);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), "domain_rules", &view, false, 0))
            .unwrap();
        let output = rendered(&terminal);

        assert!(!output.contains("No query metadata"));
        assert!(!output.contains("No analysis note"));
        assert!(output.contains("Results 0"));
        assert!(output.contains("Rules: none"));
    }

    #[test]
    fn management_records_have_human_identity_lines() {
        let rules = ToolResultView::from_text(
            r#"{"rules":[{"rule_kind":"free_fn","pattern":"release_*","language":"c","confidence":0.9}]}"#.into(),
            false,
        );
        let dispatches = ToolResultView::from_text(
            r#"{"annotations":[{"field_qname":"ops.read","target_qname":"driver_read","confidence":0.8}]}"#.into(),
            false,
        );

        assert!(
            rules
                .fact_text()
                .contains("- release_*  [free fn]  c  confidence 90%")
        );
        assert!(
            dispatches
                .fact_text()
                .contains("- ops.read -> driver_read  confidence 80%")
        );
    }

    #[test]
    fn narrow_render_shows_hud_facts_and_view_toggle() {
        let view = ToolResultView::from_text(
            r#"{"analysis":{"scope":"local","basis":["manifest","structural"],"summary":"Focus analysis still expanding with background work."},"direction":"outgoing","query_id":"q_123","result":{"name":"run","file":"src/lib.rs","line":3}}"#.into(),
            false,
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), "impact", &view, false, 0))
            .unwrap();
        let output = rendered(&terminal);

        assert!(output.contains("Impact"));
        assert!(output.contains("Code facts"));
        assert!(output.contains("src/lib.rs:3"));
        assert!(output.contains("r raw"));
        assert!(output.contains("Resume ready"));
        assert!(!output.contains("Direction"));
    }
}
