//! Pure text builders for the Strix modal: the header, the findings pane and the agent
//! stream. No widget code lives here, only `Line`s, so the follow-the-tail math in `app`
//! and the drawing in `ui` can both measure exactly the same content.
//!
//! This is a faithful port of the Ink dashboard's pure builders: the wrapping rules, the
//! vocabulary and the styling all mirror the TypeScript original so the two dashboards
//! read as one product.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::data::{elapsed, human, CoverageSummary, FindingDetail, ProgressDetail};
use crate::theme;

// --- the wrapping primitives ------------------------------------------------------

/// One styled run of text before it becomes a ratatui `Span`. The builders work in terms
/// of these because the wrapper has to be able to split and re-style a run without losing
/// what style it carried.
#[derive(Clone)]
struct Cell {
    text: String,
    style: Style,
}

fn cell(text: impl Into<String>, style: Style) -> Cell {
    Cell {
        text: text.into(),
        style,
    }
}

/// Build a cell from the Ink span's three optional attributes. An absent colour leaves the
/// foreground unset rather than forcing `Color::Reset`, so a plain span keeps the terminal
/// default; `dim` and `bold` are modifiers and can combine with any colour.
fn styled(text: impl Into<String>, colour: Option<Color>, dim: bool, bold: bool) -> Cell {
    let mut style = Style::default();
    if let Some(colour) = colour {
        style = style.fg(colour);
    }
    if dim {
        style = style.add_modifier(Modifier::DIM);
    }
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    cell(text, style)
}

fn to_line(cells: Vec<Cell>) -> Line<'static> {
    let spans: Vec<Span<'static>> = cells
        .into_iter()
        .map(|cell| Span::styled(cell.text, cell.style))
        .collect();
    Line::from(spans)
}

/// Wrap spans to the pane width, breaking at spaces when possible and hard-breaking any
/// token longer than the width. The leading indent of the first span is preserved on
/// continuation lines. A width below 4 is treated as 4 so the pane can never collapse to
/// nothing. Empty input still yields one (empty) line.
fn wrap_spans(spans: &[Cell], width: usize) -> Vec<Line<'static>> {
    let limit = width.max(4);
    let head: String = spans
        .first()
        .map(|span| {
            span.text
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect()
        })
        .unwrap_or_default();

    let mut wrapper = Wrapper {
        limit,
        head,
        lines: Vec::new(),
        current: Vec::new(),
        length: 0,
    };

    for span in spans {
        let parts: Vec<&str> = span.text.split('\n').collect();
        for (p, part) in parts.iter().enumerate() {
            if p > 0 {
                wrapper.flush();
            }
            let words: Vec<&str> = part.split(' ').collect();
            for (w, word) in words.iter().enumerate() {
                let sep = if w > 0 { " " } else { "" };
                if word.is_empty() {
                    if !sep.is_empty() {
                        wrapper.add(sep, span.style);
                    }
                    continue;
                }
                let needed = sep.chars().count() + word.chars().count();
                if wrapper.length + needed <= limit {
                    if !sep.is_empty() {
                        wrapper.add(sep, span.style);
                    }
                    wrapper.add(word, span.style);
                } else {
                    if wrapper.length > 0 {
                        wrapper.flush();
                    }
                    wrapper.add(word, span.style);
                }
            }
        }
    }
    if !wrapper.current.is_empty() {
        wrapper.flush();
    }
    if wrapper.lines.is_empty() {
        vec![Line::default()]
    } else {
        wrapper.lines
    }
}

struct Wrapper {
    limit: usize,
    head: String,
    lines: Vec<Line<'static>>,
    current: Vec<Cell>,
    length: usize,
}

impl Wrapper {
    /// Close the current line and start the next one, re-emitting the leading indent.
    fn flush(&mut self) {
        self.lines.push(to_line(std::mem::take(&mut self.current)));
        if self.head.is_empty() {
            self.current = Vec::new();
            self.length = 0;
        } else {
            self.length = self.head.chars().count();
            self.current = vec![cell(self.head.clone(), Style::default())];
        }
    }

    /// Append text with a style, hard-breaking mid-token whenever the current line fills up.
    fn add(&mut self, text: &str, style: Style) {
        let mut rest = text;
        while !rest.is_empty() {
            let room = self.limit.saturating_sub(self.length);
            if room == 0 {
                self.flush();
                continue;
            }
            let chunk: String = rest.chars().take(room).collect();
            let chunk_chars = chunk.chars().count();
            let chunk_bytes = chunk.len();
            self.current.push(cell(chunk, style));
            self.length += chunk_chars;
            rest = &rest[chunk_bytes..];
            if !rest.is_empty() {
                self.flush();
            }
        }
    }
}

/// The status block is exactly three clamped rows: no wrapping, just an ellipsis once the
/// row is full.
fn clamp_line(spans: Vec<Cell>, width: usize) -> Line<'static> {
    let mut out: Vec<Cell> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        if used >= width {
            break;
        }
        let room = width - used;
        let len = span.text.chars().count();
        if len <= room {
            used += len;
            out.push(span);
        } else {
            let keep = room.saturating_sub(1);
            let prefix: String = span.text.chars().take(keep).collect();
            out.push(cell(format!("{prefix}…"), span.style));
            break;
        }
    }
    if out.is_empty() {
        Line::from(vec![Span::raw(" ")])
    } else {
        to_line(out)
    }
}

/// Wrap `spans` and append the resulting lines to `lines`.
fn wrap_into(lines: &mut Vec<Line<'static>>, spans: Vec<Cell>, width: usize) {
    lines.extend(wrap_spans(&spans, width));
}

// --- the pure data helpers --------------------------------------------------------

/// Coverage is split across the summary and the rendered gap list. Return both, plus the
/// count the heading should show: the rendered list when there is one, else the summary's
/// own number (via `Coverage::gap_count`), else zero.
fn coverage_parts(
    detail: &ProgressDetail,
) -> (Option<&CoverageSummary>, &[serde_json::Value], i64) {
    match detail.coverage.as_ref() {
        Some(coverage) => (
            coverage.summary.as_ref(),
            coverage.gaps.as_slice(),
            coverage.gap_count(0),
        ),
        None => (None, &[], 0),
    }
}

/// The findings pane's one-line severity tally, ordered critical → info, unknown severities
/// last, ties in first-seen order.
fn severity_tally(findings: &[FindingDetail]) -> String {
    let order = ["critical", "high", "medium", "low", "info"];
    let mut counts: Vec<(String, i64)> = Vec::new();
    for finding in findings {
        let key = finding.severity.as_deref().unwrap_or("?").to_lowercase();
        match counts.iter_mut().find(|(name, _)| name == &key) {
            Some((_, count)) => *count += 1,
            None => counts.push((key, 1)),
        }
    }
    counts.sort_by_key(|(name, _)| {
        order
            .iter()
            .position(|known| known == name)
            .map(|index| index + 1)
            .unwrap_or(99)
    });
    counts
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A whole-word, case-insensitive substring search, standing in for the Ink regex's `\b`.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let hay: Vec<char> = haystack.chars().collect();
    let nee: Vec<char> = needle.chars().collect();
    if nee.is_empty() || nee.len() > hay.len() {
        return false;
    }
    for start in 0..=hay.len() - nee.len() {
        if hay[start..start + nee.len()] == nee[..] {
            let before = start == 0 || !is_word_char(hay[start - 1]);
            let after = start + nee.len() == hay.len() || !is_word_char(hay[start + nee.len()]);
            if before && after {
                return true;
            }
        }
    }
    false
}

/// The Ink `FAILURE_RE`, hand-rolled because the dashboard carries no regex dependency.
fn failure_re(text: &str) -> bool {
    let haystack = text.to_lowercase();
    [
        "error",
        "failed",
        "failure",
        "exception",
        "traceback",
        "denied",
        "refused",
        "not found",
        "no such file",
    ]
    .iter()
    .any(|needle| contains_word(&haystack, needle))
}

// --- the public builders ----------------------------------------------------------

/// The Strix modal's header, clamped to `width` and to three rows: run and state with the
/// elapsed time and pid, then agents and todos, then findings, coverage gaps, tokens and
/// cost. The current activity rides on the first row when there is one.
pub fn build_status(
    detail: &ProgressDetail,
    width: usize,
    state: &str,
    pid: Option<i64>,
    activity: Option<&str>,
) -> Vec<Line<'static>> {
    let findings_len = detail.findings_detail.len() as i64;
    let (summary, _gaps, gap_count) = coverage_parts(detail);

    let mut row1 = vec![
        styled(" ", None, false, false),
        styled(detail.run.clone(), Some(Color::Cyan), false, true),
        styled("  ", None, false, false),
        styled(
            state.to_string(),
            Some(theme::state_colour(state)),
            false,
            false,
        ),
        styled(
            format!(
                "  {}  pid {}",
                elapsed(detail.start_time.as_deref(), detail.end_time.as_deref()),
                pid.map_or_else(|| "-".to_string(), |pid| pid.to_string()),
            ),
            None,
            true,
            false,
        ),
    ];
    if let Some(activity) = activity {
        row1.push(cell(" · ", theme::dim()));
        row1.push(cell(activity.to_string(), theme::accent()));
    }

    let mut row2 = vec![
        styled(" ", None, false, false),
        styled(
            format!(
                "agents {}/{} done · todos {}/{}",
                detail.agents.completed, detail.agents.total, detail.todos.done, detail.todos.total
            ),
            None,
            false,
            false,
        ),
    ];
    if detail.agents.failed != 0 {
        row2.push(styled(
            format!(" · {} failed", detail.agents.failed),
            Some(Color::Red),
            false,
            false,
        ));
    }

    let surfaces = summary.and_then(|s| s.surfaces_reviewed).unwrap_or(0);
    let filed = summary
        .and_then(|s| s.findings_filed)
        .unwrap_or(findings_len);
    let gap_word = if gap_count == 1 { "gap" } else { "gaps" };
    let gap_colour = if gap_count != 0 {
        Color::Yellow
    } else {
        Color::Green
    };
    let cost = detail
        .cost_usd
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let row3 = vec![
        styled(" coverage ", None, false, false),
        styled(
            format!("{surfaces} surfaces · {filed} filed · "),
            None,
            false,
            false,
        ),
        styled(
            format!("{gap_count} {gap_word}"),
            Some(gap_colour),
            false,
            false,
        ),
        styled(" · ", None, false, false),
        styled(
            format!(
                "usage {} in / {} out",
                human(detail.usage.input_tokens),
                human(detail.usage.output_tokens)
            ),
            None,
            true,
            false,
        ),
        styled(format!(" · ${cost}"), None, true, false),
    ];

    vec![
        clamp_line(row1, width),
        clamp_line(row2, width),
        clamp_line(row3, width),
    ]
}

/// The findings pane content: the severity tally, each finding coloured by severity, then
/// the coverage gaps. The status block is not repeated here; it is the header.
pub fn build_findings(detail: &ProgressDetail, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let findings = &detail.findings_detail;

    if !findings.is_empty() {
        wrap_into(
            &mut lines,
            vec![styled(
                format!(" severity  {}", severity_tally(findings)),
                None,
                true,
                false,
            )],
            width,
        );
    }

    for finding in findings {
        let severity = finding.severity.as_deref().unwrap_or("?");
        let colour = theme::severity_colour(severity);
        let title = finding
            .title
            .clone()
            .unwrap_or_else(|| "(untitled)".to_string());
        wrap_into(
            &mut lines,
            vec![
                styled("  ", None, false, false),
                styled(
                    format!("[{}] ", severity.to_lowercase()),
                    Some(colour),
                    false,
                    false,
                ),
                styled(title, Some(colour), false, false),
            ],
            width,
        );
    }

    let (_summary, gaps, gap_count) = coverage_parts(detail);
    wrap_into(
        &mut lines,
        vec![styled(
            format!(" COVERAGE GAPS ({gap_count})"),
            Some(Color::White),
            false,
            true,
        )],
        width,
    );
    for gap in gaps {
        wrap_into(
            &mut lines,
            vec![
                styled("  · ", None, false, false),
                styled(gap_text(gap), Some(Color::Yellow), false, false),
            ],
            width,
        );
    }
    lines
}

/// The agent stream: one block per message, oldest first, the agent name leading the line
/// and the type deciding the treatment.
pub fn build_stream(detail: &ProgressDetail, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if detail.messages.is_empty() {
        wrap_into(
            &mut lines,
            vec![styled("  (no agent messages yet)", None, true, false)],
            width,
        );
        return lines;
    }

    for message in &detail.messages {
        let failed = message.kind == "function_call_output" && failure_re(&message.text);
        let name = if message.agent_name.is_empty() {
            "?"
        } else {
            message.agent_name.as_str()
        };
        let at = message.at.as_deref().unwrap_or("");
        let stamp: String = at.chars().skip(11).take(8).collect();

        let mut head = vec![styled("  ", None, false, false)];
        if !stamp.is_empty() {
            head.push(styled(format!("{stamp} "), None, true, false));
        }
        head.push(styled(
            name.to_string(),
            Some(agent_colour(name)),
            false,
            false,
        ));
        head.push(styled(" · ", None, true, false));
        match message.kind.as_str() {
            "function_call" => {
                head.push(styled("call ", Some(Color::Cyan), false, false));
                head.push(styled(
                    message.tool.as_deref().unwrap_or("?").to_string(),
                    Some(Color::Cyan),
                    false,
                    true,
                ));
            }
            "function_call_output" => {
                let colour = if failed { Some(Color::Red) } else { None };
                head.push(styled("result", colour, !failed, false));
            }
            "reasoning" => {
                head.push(styled("reasoning", None, true, false));
            }
            other => {
                head.push(styled(other.to_string(), None, false, false));
            }
        }
        if message.truncated {
            head.push(styled(" [truncated]", None, true, false));
        }
        wrap_into(&mut lines, head, width);

        let mut dim = false;
        let mut colour: Option<Color> = None;
        match message.kind.as_str() {
            "reasoning" | "function_call" => dim = true,
            "function_call_output" => {
                if failed {
                    colour = Some(Color::Red);
                } else {
                    dim = true;
                }
            }
            _ => {}
        }
        let body = message.text.replace('\r', "");
        let parts: Vec<String> = if body.is_empty() {
            vec!["(empty)".to_string()]
        } else {
            body.split('\n').map(|part| part.to_string()).collect()
        };
        for raw in parts {
            wrap_into(
                &mut lines,
                vec![
                    styled("    ", None, false, false),
                    styled(raw.replace('\t', "  "), colour, dim, false),
                ],
                width,
            );
        }
        lines.push(Line::default());
    }
    lines
}

/// The pane title marker: the last visible row over the total, and how much is hidden on
/// either side. A pane that fits adds nothing.
pub fn overflow_mark(offset: usize, total: usize, visible: usize) -> String {
    if visible == 0 || total <= visible {
        return String::new();
    }
    let last = total.min(offset + visible);
    let up = if offset > 0 { "↑ " } else { "" };
    let down = if last < total {
        format!(" ↓ {} more", total - last)
    } else {
        String::new()
    };
    format!("{up}{last}/{total}{down}")
}

/// Flatten one coverage gap, which the CLI may hand back as a string or an object, into the
/// text the pane shows.
pub fn gap_text(gap: &serde_json::Value) -> String {
    match gap {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(map) => {
            for key in ["message", "title", "rule"] {
                if let Some(value) = map.get(key).and_then(|value| value.as_str()) {
                    if !value.is_empty() {
                        return value.to_string();
                    }
                }
            }
            serde_json::to_string(gap).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

/// A stable colour per agent, hashed by name so an agent keeps its colour across frames.
pub fn agent_colour(name: &str) -> Color {
    let palette = [
        Color::Cyan,
        Color::Magenta,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::White,
    ];
    let mut hash: u32 = 0;
    for unit in name.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(unit as u32);
    }
    palette[(hash % palette.len() as u32) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::AgentMessage;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn overflow_mark_formats() {
        assert_eq!(overflow_mark(0, 145, 12), "12/145 ↓ 133 more");
        assert_eq!(overflow_mark(0, 5, 5), "");
        assert_eq!(overflow_mark(10, 145, 12), "↑ 22/145 ↓ 123 more");
        assert_eq!(overflow_mark(12, 145, 12), "↑ 24/145 ↓ 121 more");
    }

    #[test]
    fn gap_text_reads_strings_and_objects() {
        assert_eq!(gap_text(&serde_json::json!("a gap")), "a gap");
        assert_eq!(
            gap_text(&serde_json::json!({"message": "unexamined"})),
            "unexamined"
        );
        assert_eq!(gap_text(&serde_json::json!({"title": "t"})), "t");
        assert_eq!(gap_text(&serde_json::json!({"rule": "r"})), "r");
    }

    #[test]
    fn agent_colour_is_stable() {
        assert_eq!(agent_colour("Reporter"), agent_colour("Reporter"));
        assert_eq!(agent_colour("Reporter"), agent_colour("Reporter"));
    }

    #[test]
    fn wrap_spans_hard_breaks_long_tokens() {
        let spans = vec![styled("x".repeat(50), None, false, false)];
        let lines = wrap_spans(&spans, 10);
        assert!(lines.len() >= 5);
        for line in &lines {
            let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(width <= 10, "line over width: {width}");
        }
        assert_eq!(text_of(&lines), "x".repeat(50));
    }

    #[test]
    fn build_stream_empty_has_placeholder() {
        let detail = ProgressDetail::default();
        let lines = build_stream(&detail, 80);
        assert!(text_of(&lines).contains("(no agent messages yet)"));
    }

    #[test]
    fn build_stream_renders_a_message() {
        let mut detail = ProgressDetail::default();
        detail.messages.push(AgentMessage {
            id: 1,
            agent_name: "Reporter".to_string(),
            kind: "message".to_string(),
            text: "done".to_string(),
            at: Some("2026-10-05 12:13:12".to_string()),
            ..Default::default()
        });
        let lines = build_stream(&detail, 80);
        let text = text_of(&lines);
        assert!(text.contains("Reporter"));
        assert!(text.contains("12:13:12"));
        assert!(text.contains("done"));
    }

    #[test]
    fn build_findings_includes_coverage_heading() {
        let detail = ProgressDetail::default();
        let lines = build_findings(&detail, 80);
        assert!(text_of(&lines).contains("COVERAGE GAPS"));
    }
}
