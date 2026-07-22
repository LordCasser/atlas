//! Command palette for the less frequent analysis tools.

use ratatui::{
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub context: &'static str,
}

#[derive(Debug, Clone)]
pub enum FieldKind {
    Text,
    StringList,
    Number,
    Boolean,
    Choice(&'static [&'static str]),
}

#[derive(Debug, Clone)]
pub struct FormField {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
    pub kind: FieldKind,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct CommandForm {
    pub command: &'static str,
    pub fields: Vec<FormField>,
    pub selected: usize,
    pub editing: bool,
    pub edit_buffer: String,
    pub edit_cursor: usize,
    pub error: Option<String>,
}

impl CommandForm {
    pub fn new(command: &'static str, context: Option<(&str, Option<&str>)>) -> Self {
        let symbol = context.map(|(name, _)| name).unwrap_or_default();
        let file = context.and_then(|(_, path)| path).unwrap_or_default();
        let fields = match command {
            "symbol" => vec![
                text("symbol", "Symbol", symbol, true),
                choice("view", "View", "detail", &["detail", "context", "usages"]),
                boolean("includeCode", "Include code", false),
                string_list("include_roots", "Include roots", ""),
            ],
            "calls" => vec![
                text("symbol", "Symbol", symbol, true),
                choice(
                    "direction",
                    "Direction",
                    "both",
                    &["incoming", "outgoing", "both"],
                ),
                number("depth", "Depth", "1"),
                number("limit", "Limit", ""),
                string_list("include_roots", "Include roots", ""),
            ],
            "explore" => vec![
                text("symbol", "Symbol", symbol, true),
                choice(
                    "source_mode",
                    "Source",
                    "excerpt",
                    &["excerpt", "full", "none"],
                ),
                number("source_lines", "Source lines", "40"),
                number("evidence_limit", "Evidence", "5"),
                boolean("include_file_context", "File context", true),
                string_list("include_roots", "Include roots", ""),
            ],
            "impact" => vec![
                text("symbol", "Symbol", symbol, true),
                number("depth", "Depth", "3"),
                choice(
                    "direction",
                    "Direction",
                    "outgoing",
                    &["outgoing", "incoming", "both"],
                ),
                boolean("semantic", "Semantic", false),
            ],
            "path" => vec![
                text("from", "From", symbol, true),
                text("to", "To", "", true),
                number("max_depth", "Max depth", "5"),
                choice(
                    "direction",
                    "Direction",
                    "outgoing",
                    &["outgoing", "incoming", "both"],
                ),
                boolean("prefer_production", "Prefer production", false),
                string_list("include_roots", "Include roots", ""),
            ],
            "trace" => vec![
                choice(
                    "kind",
                    "Kind",
                    "callers",
                    &["callers", "forward", "point", "variable"],
                ),
                text("symbol", "Symbol", symbol, false),
                text("from", "From", "", false),
                text("to", "To", "", false),
                text("file_path", "File", file, false),
                number("line", "Line", ""),
                number("column", "Column", ""),
                number("max_depth", "Max depth", ""),
                string_list("include_roots", "Include roots", ""),
            ],
            "file_dependencies" => vec![
                text("file_path", "File", file, true),
                choice(
                    "direction",
                    "Direction",
                    "outgoing",
                    &["outgoing", "incoming", "both"],
                ),
                choice(
                    "analysis",
                    "Analysis",
                    "manifest",
                    &["manifest", "structural"],
                ),
                number("limit", "Limit", "50"),
            ],
            "lifecycle" => vec![
                text("symbol", "Symbol", symbol, true),
                text("field", "Field", "", true),
                string_list("include_roots", "Include roots", ""),
            ],
            "branch_diff" => vec![
                text("symbol", "Symbol", symbol, true),
                string_list("include_roots", "Include roots", ""),
            ],
            "domain_rules" => vec![
                choice(
                    "action",
                    "Action",
                    "list",
                    &["list", "add", "delete", "learn"],
                ),
                choice(
                    "rule_kind",
                    "Rule kind",
                    "alloc_fn",
                    &["alloc_fn", "free_fn", "owned_pattern", "cleanup_fn"],
                ),
                text("pattern", "Pattern", "", false),
                text("rule_id", "Rule ID", "", false),
                choice("source", "Source", "", &["user", "builtin", "learned"]),
                number("confidence", "Confidence", "1.0"),
                number("min_confidence", "Min confidence", "0.5"),
                number("limit", "Limit", ""),
            ],
            "fp_dispatches" => vec![
                choice("action", "Action", "list", &["list", "add", "delete"]),
                text("field_qname", "Field", "", false),
                text("target_qname", "Target", "", false),
                text("annotation_id", "Annotation ID", "", false),
                number("confidence", "Confidence", "1.0"),
                number("limit", "Limit", ""),
            ],
            "tasks" => vec![text("query_id", "Query ID", "", false)],
            "resume_query" => vec![text("query_id", "Query ID", "", true)],
            _ => Vec::new(),
        };
        let mut form = Self {
            command,
            fields,
            selected: 0,
            editing: false,
            edit_buffer: String::new(),
            edit_cursor: 0,
            error: None,
        };
        form.selected = form
            .visible_field_indices()
            .into_iter()
            .find(|&index| {
                form.field_required(&form.fields[index]) && form.fields[index].value.is_empty()
            })
            .unwrap_or(form.fields.len());
        form
    }

    pub fn arguments(&self) -> Result<Value, String> {
        let action = self.action();
        if matches!((self.command, action), ("fp_dispatches", Some("delete")))
            && self.value("annotation_id").is_empty()
            && self.value("field_qname").is_empty()
        {
            return Err("Annotation ID or Field is required for delete".into());
        }

        let mut args = Map::new();
        for field in &self.fields {
            if !self.field_applies(field.key) {
                continue;
            }
            if field.value.trim().is_empty() {
                if self.field_required(field) {
                    return Err(format!("{} is required", field.label));
                }
                continue;
            }
            let value = match field.kind {
                FieldKind::StringList => Value::Array(
                    field
                        .value
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| Value::String(value.to_string()))
                        .collect(),
                ),
                FieldKind::Number => parse_number(&field.value)
                    .ok_or_else(|| format!("{} must be a number", field.label))?,
                FieldKind::Boolean => json!(field.value == "true"),
                _ => json!(field.value),
            };
            args.insert(field.key.to_string(), value);
        }
        Ok(Value::Object(args))
    }

    fn value(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.value.trim())
            .unwrap_or_default()
    }

    fn action(&self) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.key == "action")
            .map(|field| field.value.as_str())
    }

    fn trace_kind(&self) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.key == "kind")
            .map(|field| field.value.as_str())
    }

    fn direction(&self) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.key == "direction")
            .map(|field| field.value.as_str())
    }

    fn field_applies(&self, key: &str) -> bool {
        field_applies(
            self.command,
            self.action(),
            self.trace_kind(),
            self.direction(),
            key,
        )
    }

    fn field_required(&self, field: &FormField) -> bool {
        field.required
            || matches!(
                (self.command, self.action(), self.trace_kind(), field.key),
                ("trace", _, Some("callers"), "symbol")
                    | ("trace", _, Some("forward"), "from" | "to")
                    | (
                        "trace",
                        _,
                        Some("point" | "variable"),
                        "file_path" | "line" | "column"
                    )
                    | ("domain_rules", Some("add"), _, "pattern")
                    | ("domain_rules", Some("delete"), _, "rule_id")
                    | (
                        "fp_dispatches",
                        Some("add"),
                        _,
                        "field_qname" | "target_qname"
                    )
            )
    }

    fn visible_field_indices(&self) -> Vec<usize> {
        self.fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| self.field_applies(field.key).then_some(index))
            .collect()
    }

    pub fn first_missing_required(&self) -> Option<usize> {
        self.visible_field_indices().into_iter().find(|&index| {
            self.field_required(&self.fields[index]) && self.fields[index].value.trim().is_empty()
        })
    }

    pub fn prefill_query_id(&mut self, query_id: Option<&str>) {
        let Some(query_id) = query_id.filter(|value| !value.is_empty()) else {
            return;
        };
        let Some(index) = self.fields.iter().position(|field| field.key == "query_id") else {
            return;
        };
        self.fields[index].value = query_id.into();
        if self.selected == index {
            self.selected = self.fields.len();
        }
    }

    #[cfg(test)]
    fn visible_field_keys(&self) -> Vec<&'static str> {
        self.visible_field_indices()
            .into_iter()
            .map(|index| self.fields[index].key)
            .collect()
    }

    pub fn move_next(&mut self) {
        let mut choices = self.visible_field_indices();
        choices.push(self.fields.len());
        let position = choices
            .iter()
            .position(|&index| index == self.selected)
            .unwrap_or(choices.len() - 1);
        self.selected = choices[(position + 1) % choices.len()];
    }

    pub fn move_previous(&mut self) {
        let mut choices = self.visible_field_indices();
        choices.push(self.fields.len());
        let position = choices
            .iter()
            .position(|&index| index == self.selected)
            .unwrap_or(0);
        self.selected = choices[(position + choices.len() - 1) % choices.len()];
    }

    pub fn cycle(&mut self, forward: bool) {
        let Some(field) = self.fields.get_mut(self.selected) else {
            return;
        };
        match field.kind {
            FieldKind::Boolean => field.value = (field.value != "true").to_string(),
            FieldKind::Choice(values) => {
                let current = values.iter().position(|value| *value == field.value);
                let next = match (current, forward) {
                    (Some(current), true) => (current + 1) % values.len(),
                    (Some(current), false) => (current + values.len() - 1) % values.len(),
                    (None, true) => 0,
                    (None, false) => values.len() - 1,
                };
                field.value = values[next].to_string();
            }
            _ => {}
        }
        self.error = None;
    }

    pub fn begin_edit(&mut self) {
        let Some(field) = self.fields.get(self.selected) else {
            return;
        };
        if matches!(
            field.kind,
            FieldKind::Text | FieldKind::StringList | FieldKind::Number
        ) {
            self.edit_buffer = field.value.clone();
            self.edit_cursor = self.edit_buffer.chars().count();
            self.editing = true;
        } else {
            self.cycle(true);
        }
    }

    pub fn commit_edit(&mut self) {
        if let Some(field) = self.fields.get_mut(self.selected) {
            field.value = std::mem::take(&mut self.edit_buffer);
        }
        self.editing = false;
        self.error = None;
    }
}

fn text(key: &'static str, label: &'static str, value: &str, required: bool) -> FormField {
    FormField {
        key,
        label,
        value: value.into(),
        kind: FieldKind::Text,
        required,
    }
}

fn number(key: &'static str, label: &'static str, value: &str) -> FormField {
    FormField {
        key,
        label,
        value: value.into(),
        kind: FieldKind::Number,
        required: false,
    }
}

fn string_list(key: &'static str, label: &'static str, value: &str) -> FormField {
    FormField {
        key,
        label,
        value: value.into(),
        kind: FieldKind::StringList,
        required: false,
    }
}

fn boolean(key: &'static str, label: &'static str, value: bool) -> FormField {
    FormField {
        key,
        label,
        value: value.to_string(),
        kind: FieldKind::Boolean,
        required: false,
    }
}

fn choice(
    key: &'static str,
    label: &'static str,
    value: &str,
    values: &'static [&'static str],
) -> FormField {
    FormField {
        key,
        label,
        value: value.into(),
        kind: FieldKind::Choice(values),
        required: false,
    }
}

fn parse_number(value: &str) -> Option<Value> {
    value.parse::<i64>().map(Value::from).ok().or_else(|| {
        value
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
    })
}

fn field_applies(
    command: &str,
    action: Option<&str>,
    trace_kind: Option<&str>,
    direction: Option<&str>,
    key: &str,
) -> bool {
    match (command, action, trace_kind, direction) {
        ("calls", _, _, Some("incoming" | "outgoing")) => key != "depth",
        ("trace", _, Some("callers"), _) => {
            matches!(key, "kind" | "symbol" | "max_depth" | "include_roots")
        }
        ("trace", _, Some("forward"), _) => {
            matches!(key, "kind" | "from" | "to" | "max_depth" | "include_roots")
        }
        ("trace", _, Some("point"), _) => {
            matches!(
                key,
                "kind" | "file_path" | "line" | "column" | "include_roots"
            )
        }
        ("trace", _, Some("variable"), _) => {
            matches!(
                key,
                "kind" | "file_path" | "line" | "column" | "max_depth" | "include_roots"
            )
        }
        ("domain_rules", Some("list"), _, _) => matches!(key, "action" | "source" | "limit"),
        ("domain_rules", Some("add"), _, _) => {
            matches!(key, "action" | "rule_kind" | "pattern" | "confidence")
        }
        ("domain_rules", Some("delete"), _, _) => matches!(key, "action" | "rule_id"),
        ("domain_rules", Some("learn"), _, _) => {
            matches!(key, "action" | "min_confidence" | "limit")
        }
        ("fp_dispatches", Some("list"), _, _) => matches!(key, "action" | "limit"),
        ("fp_dispatches", Some("add"), _, _) => {
            matches!(
                key,
                "action" | "field_qname" | "target_qname" | "confidence"
            )
        }
        ("fp_dispatches", Some("delete"), _, _) => {
            matches!(key, "action" | "annotation_id" | "field_qname")
        }
        _ => true,
    }
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "symbol",
        description: "Symbol detail, context, or usages",
        context: "current symbol",
    },
    CommandSpec {
        name: "calls",
        description: "Incoming/outgoing call graph",
        context: "current symbol",
    },
    CommandSpec {
        name: "explore",
        description: "Symbol dossier with evidence",
        context: "current symbol",
    },
    CommandSpec {
        name: "impact",
        description: "Change impact radius",
        context: "current symbol",
    },
    CommandSpec {
        name: "path",
        description: "Shortest path to another symbol",
        context: "current symbol; choose a target in the form",
    },
    CommandSpec {
        name: "trace",
        description: "Caller, forward, point, or variable trace",
        context: "current symbol",
    },
    CommandSpec {
        name: "file_dependencies",
        description: "File imports and importers",
        context: "current file",
    },
    CommandSpec {
        name: "lifecycle",
        description: "C/C++ field lifecycle",
        context: "current symbol; enter a field in the form",
    },
    CommandSpec {
        name: "branch_diff",
        description: "Compare sibling branch effects",
        context: "current symbol",
    },
    CommandSpec {
        name: "domain_rules",
        description: "List ownership domain rules",
        context: "no selection required",
    },
    CommandSpec {
        name: "fp_dispatches",
        description: "List function-pointer annotations",
        context: "no selection required",
    },
    CommandSpec {
        name: "tasks",
        description: "Inspect focus and lazy analysis jobs",
        context: "optionally enter a query ID",
    },
    CommandSpec {
        name: "resume_query",
        description: "Resume a non-terminal analysis query",
        context: "enter the query ID to resume",
    },
];

#[derive(Debug, Default)]
pub struct PaletteState {
    pub input: String,
    pub cursor: usize,
    pub selected: usize,
    pub form: Option<CommandForm>,
}

impl PaletteState {
    pub fn clear(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.selected = 0;
        self.form = None;
    }

    pub fn matches(&self) -> Vec<&'static CommandSpec> {
        let needle = self.input.split_whitespace().next().unwrap_or_default();
        COMMANDS
            .iter()
            .filter(|command| needle.is_empty() || command.name.contains(needle))
            .collect()
    }

    pub fn select_current(&mut self) {
        if let Some(command) = self.matches().get(self.selected) {
            self.input = format!("{} ", command.name);
            self.cursor = self.input.chars().count();
        }
    }
}

pub fn render(frame: &mut ratatui::Frame, area: Rect, state: &PaletteState, error: Option<&str>) {
    if let Some(form) = &state.form {
        render_form(frame, area, form);
        return;
    }

    let width = area.width.saturating_sub(8).min(76);
    let height = area.height.saturating_sub(4).min(18);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(popup);
    let input = Paragraph::new(format!(":{}", state.input))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Command Palette "),
        )
        .style(Style::default().fg(Color::White));
    frame.render_widget(input, chunks[0]);
    let input_inner = chunks[0].inner(ratatui::layout::Margin::new(1, 1));
    let cursor_x = input_inner
        .x
        .saturating_add(1)
        .saturating_add(state.cursor as u16)
        .min(input_inner.right().saturating_sub(1));
    frame.set_cursor_position(Position::new(cursor_x, input_inner.y));

    let matches = state.matches();
    let visible_height = chunks[1].height as usize;
    let start = state
        .selected
        .saturating_add(1)
        .saturating_sub(visible_height);
    let items = matches
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_height)
        .map(|(index, command)| {
            let selected = index == state.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:<18}", command.name), style),
                Span::styled(command.description, style),
            ]))
        });
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::LEFT | Borders::RIGHT)),
        chunks[1],
    );

    let hint = error
        .map(str::to_string)
        .or_else(|| {
            matches.get(state.selected).map(|command| {
                format!(
                    " {}/{} | {} | Enter configure | Tab complete",
                    state.selected + 1,
                    matches.len(),
                    command.context
                )
            })
        })
        .unwrap_or_else(|| " No matching command".to_string());
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(if error.is_some() {
            Color::Red
        } else {
            Color::DarkGray
        })),
        chunks[2],
    );
}

fn render_form(frame: &mut ratatui::Frame, area: Rect, form: &CommandForm) {
    let visible_fields = form.visible_field_indices();
    let width = area.width.saturating_sub(8).min(76);
    let height = (visible_fields.len() as u16 + 6)
        .min(area.height.saturating_sub(4))
        .max(8);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let inner = popup.inner(ratatui::layout::Margin::new(1, 1));

    let mut lines = Vec::with_capacity(visible_fields.len() + 3);
    for &index in &visible_fields {
        let field = &form.fields[index];
        let selected = index == form.selected;
        let marker = if selected { ">" } else { " " };
        let field_required = form.field_required(field);
        let required = if field_required { "*" } else { " " };
        let raw_value = if selected && form.editing {
            form.edit_buffer.as_str()
        } else {
            field.value.as_str()
        };
        let value = if raw_value.is_empty() {
            if field_required {
                "<required>"
            } else {
                "(optional)"
            }
        } else {
            raw_value
        };
        let style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::styled(
            format!("{marker} {:<16}{required} {value}", field.label),
            style,
        ));
    }
    lines.push(Line::default());
    let run_selected = form.selected == form.fields.len();
    lines.push(Line::styled(
        if run_selected {
            "> [ Run ]"
        } else {
            "  [ Run ]"
        },
        if run_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
        },
    ));
    lines.push(Line::styled(
        form.error
            .as_deref()
            .unwrap_or("Enter edit/run | Tab move | L/R choose | Esc back"),
        Style::default().fg(if form.error.is_some() {
            Color::Red
        } else {
            Color::DarkGray
        }),
    ));

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} parameters ", form.command)),
        ),
        popup,
    );

    if form.editing {
        let row = visible_fields
            .iter()
            .position(|&index| index == form.selected)
            .unwrap_or_default() as u16;
        let cursor_x = inner
            .x
            .saturating_add(19)
            .saturating_add(form.edit_cursor as u16)
            .min(inner.right().saturating_sub(1));
        frame.set_cursor_position(Position::new(cursor_x, inner.y.saturating_add(row)));
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, layout::Position};

    #[test]
    fn domain_rules_form_has_default_list_action() {
        let form = CommandForm::new("domain_rules", None);
        let args = form.arguments().unwrap();
        assert_eq!(args["action"], "list");
    }

    #[test]
    fn impact_form_injects_symbol_and_accepts_field_edits() {
        let mut form = CommandForm::new("impact", Some(("crate::handler", Some("src/lib.rs"))));
        form.fields[1].value = "5".into();
        let args = form.arguments().unwrap();
        assert_eq!(args["symbol"], "crate::handler");
        assert_eq!(args["depth"], 5);
    }

    #[test]
    fn file_dependencies_form_uses_selected_file() {
        let form = CommandForm::new(
            "file_dependencies",
            Some(("crate::handler", Some("src/lib.rs"))),
        );
        let args = form.arguments().unwrap();
        assert_eq!(args["file_path"], "src/lib.rs");
    }

    #[test]
    fn path_form_reports_missing_target() {
        let form = CommandForm::new("path", Some(("crate::handler", None)));
        assert_eq!(form.arguments().unwrap_err(), "To is required");
    }

    #[test]
    fn trace_form_requires_only_fields_for_the_selected_kind() {
        let mut form = CommandForm::new("trace", None);
        assert_eq!(form.arguments().unwrap_err(), "Symbol is required");
        assert_eq!(
            form.visible_field_keys(),
            vec!["kind", "symbol", "max_depth", "include_roots"]
        );

        form.fields[0].value = "point".into();
        assert_eq!(
            form.visible_field_keys(),
            vec!["kind", "file_path", "line", "column", "include_roots"]
        );
        assert_eq!(form.arguments().unwrap_err(), "File is required");
    }

    #[test]
    fn palette_defaults_do_not_override_mcp_query_defaults() {
        let calls = CommandForm::new("calls", Some(("crate::handler", None)));
        let calls = calls.arguments().unwrap();
        assert!(calls.get("limit").is_none());

        let mut incoming_calls = CommandForm::new("calls", Some(("crate::handler", None)));
        incoming_calls.fields[1].value = "incoming".into();
        assert!(incoming_calls.arguments().unwrap().get("depth").is_none());
        assert!(!incoming_calls.visible_field_keys().contains(&"depth"));

        let path = CommandForm::new("path", Some(("crate::handler", None)));
        assert_eq!(path.arguments().unwrap_err(), "To is required");
        let prefer_production = path
            .fields
            .iter()
            .find(|field| field.key == "prefer_production")
            .unwrap();
        assert_eq!(prefer_production.value, "false");

        let trace = CommandForm::new("trace", Some(("crate::handler", None)));
        let trace = trace.arguments().unwrap();
        assert!(
            trace.get("max_depth").is_none(),
            "each trace kind must retain its handler-specific default"
        );
    }

    #[test]
    fn include_roots_are_forwarded_as_a_string_array() {
        let mut form = CommandForm::new("calls", Some(("crate::handler", None)));
        form.fields
            .iter_mut()
            .find(|field| field.key == "include_roots")
            .unwrap()
            .value = "include, third_party/include".into();

        assert_eq!(
            form.arguments().unwrap()["include_roots"],
            json!(["include", "third_party/include"])
        );
    }

    #[test]
    fn domain_rule_actions_expose_only_relevant_fields() {
        let mut form = CommandForm::new("domain_rules", None);
        assert_eq!(form.visible_field_keys(), vec!["action", "source", "limit"]);

        form.fields[0].value = "learn".into();
        assert_eq!(
            form.visible_field_keys(),
            vec!["action", "min_confidence", "limit"]
        );
        form.fields
            .iter_mut()
            .find(|field| field.key == "min_confidence")
            .unwrap()
            .value = "0.75".into();
        let args = form.arguments().unwrap();
        assert_eq!(args["min_confidence"], 0.75);
        assert!(args.get("confidence").is_none());
    }

    #[test]
    fn form_navigation_skips_hidden_fields() {
        let mut form = CommandForm::new("domain_rules", None);
        form.selected = 0;
        form.move_next();
        assert_eq!(form.fields[form.selected].key, "source");
        form.move_next();
        assert_eq!(form.fields[form.selected].key, "limit");
        form.move_next();
        assert_eq!(form.selected, form.fields.len());
    }

    #[test]
    fn resume_form_accepts_session_query_id() {
        let mut form = CommandForm::new("resume_query", None);
        form.prefill_query_id(Some("q_123"));

        assert_eq!(form.arguments().unwrap()["query_id"], "q_123");
        assert_eq!(form.selected, form.fields.len());
    }

    #[test]
    fn render_places_terminal_cursor_at_input_cursor() {
        let state = PaletteState {
            input: "impact ".into(),
            cursor: 3,
            selected: 0,
            form: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &state, None))
            .unwrap();

        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(9, 3));
    }

    #[test]
    fn narrow_palette_scrolls_to_selected_command() {
        let state = PaletteState {
            input: String::new(),
            cursor: 0,
            selected: COMMANDS.len() - 1,
            form: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &state, None))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("resume_query"));
    }
}
