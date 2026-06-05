//! Pure rendering: turn fetched results into styled Ratatui lines. The same
//! output drives both the one-shot snapshot and the live watch TUI.

use chrono::{DateTime, Local, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::model::{Badge, Dot, RepoResult, WorkflowRow};

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const W_STATUS: usize = 9;
const W_STARTED: usize = 14;
const W_FINISHED: usize = 14;
const W_DURATION: usize = 9;
const W_ETA: usize = 9;
const W_RECENT: usize = 13;
const W_COMMIT: usize = 8;

/// Everything the renderer needs for one frame.
pub struct Frame<'a> {
    pub results: &'a [RepoResult],
    pub aggregate: bool,
    pub branch: &'a str,
    pub now: DateTime<Local>,
    pub watch: Option<WatchInfo>,
    pub spinner: usize,
    pub loading: bool,
    /// Emit OSC-8 hyperlinks (one-shot ANSI only; the TUI buffer can't carry them).
    pub hyperlinks: bool,
    /// Flat index (over all data rows in render order) of the selected row.
    pub selected: Option<usize>,
    /// A prompt/status line shown under the header (e.g. a re-run confirm).
    pub prompt: Option<Line<'static>>,
}

pub struct WatchInfo {
    pub interval: u64,
    pub remaining: i64,
}

// ── style helpers ────────────────────────────────────────────────
fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn bold(c: Color) -> Style {
    Style::default().fg(c).add_modifier(Modifier::BOLD)
}
fn sep(s: &str) -> Span<'static> {
    Span::styled(s.to_string(), dim())
}

fn badge_style(b: &Badge) -> Style {
    match b {
        Badge::Pass => Style::default().fg(Color::Green),
        Badge::Fail => bold(Color::Red),
        Badge::Running => Style::default().fg(Color::Yellow),
        Badge::Queued | Badge::Pending => Style::default().fg(Color::Yellow),
        _ => dim(),
    }
}

// ── top-level builder ────────────────────────────────────────────
pub fn build_lines(f: &Frame) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(header_line(f));
    if let Some(prompt) = &f.prompt {
        lines.push(prompt.clone());
    }
    lines.push(Line::from(""));

    // Counter over every data row, in render order, for selection highlighting.
    let mut row_idx = 0usize;
    if f.aggregate {
        build_aggregate(f, &mut lines, &mut row_idx);
    } else {
        for repo in f.results {
            build_repo(repo, f, &mut lines, &mut row_idx);
        }
    }
    lines
}

fn header_line(f: &Frame) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("GitHub Actions", bold(Color::Cyan)),
        Span::styled(format!("  {}", f.now.format("%Y-%m-%d %H:%M:%S")), dim()),
    ];
    if let Some(w) = &f.watch {
        let spin = if f.loading {
            format!(" {} refreshing", SPINNER[f.spinner % SPINNER.len()])
        } else {
            format!(" next in {}", fmt_duration(w.remaining.max(0)))
        };
        spans.push(Span::styled(
            format!("  ⟳ every {}s ·{spin}", w.interval),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::styled(
            "   ↑↓ select · x rerun · o open · t test · r refresh · q quit".to_string(),
            dim(),
        ));
    }
    Line::from(spans)
}

// ── per-repo tables ──────────────────────────────────────────────
fn build_repo(repo: &RepoResult, f: &Frame, lines: &mut Vec<Line<'static>>, row_idx: &mut usize) {
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(repo.repo.clone(), bold(Color::Cyan)),
        Span::styled(format!(" ({})", f.branch), dim()),
    ]));

    if let Some(err) = &repo.error {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("✗ {err}"), Style::default().fg(Color::Red)),
        ]));
        lines.push(Line::from(""));
        return;
    }
    if repo.rows.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("no workflow runs found".to_string(), dim()),
        ]));
        lines.push(Line::from(""));
        return;
    }

    let name_w = repo
        .rows
        .iter()
        .map(|r| sanitize(&r.workflow_name).chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 40);

    let widths = column_widths(name_w);
    lines.push(border(&widths, '┌', '┬', '┐'));
    lines.push(header_row(name_w, "Workflow"));
    lines.push(border(&widths, '├', '┼', '┤'));
    for row in &repo.rows {
        let selected = f.selected == Some(*row_idx);
        *row_idx += 1;
        lines.push(data_row(
            &row.workflow_name,
            &repo.repo,
            row,
            f,
            name_w,
            selected,
        ));
    }
    lines.push(border(&widths, '└', '┴', '┘'));
    lines.push(Line::from(""));
}

// ── aggregate (single) table ─────────────────────────────────────
fn build_aggregate(f: &Frame, lines: &mut Vec<Line<'static>>, row_idx: &mut usize) {
    // Compute label width across all (short_repo/workflow) labels.
    let mut entries: Vec<(&str, &WorkflowRow, String)> = Vec::new();
    for repo in f.results {
        let short = repo.repo.rsplit('/').next().unwrap_or(&repo.repo);
        for row in &repo.rows {
            let label = format!("{short}/{}", sanitize(&row.workflow_name));
            entries.push((&repo.repo, row, label));
        }
    }

    let errors: Vec<&RepoResult> = f.results.iter().filter(|r| r.error.is_some()).collect();
    if entries.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("no workflow runs found".to_string(), dim()),
        ]));
        for e in &errors {
            if let Some(err) = &e.error {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("✗ {}: {err}", e.repo),
                        Style::default().fg(Color::Red),
                    ),
                ]));
            }
        }
        return;
    }

    let name_w = entries
        .iter()
        .map(|(_, _, l)| l.chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 40);
    let widths = column_widths(name_w);

    lines.push(border(&widths, '┌', '┬', '┐'));
    lines.push(header_row(name_w, "Repo/Workflow"));
    lines.push(border(&widths, '├', '┼', '┤'));

    let mut prev_short: Option<String> = None;
    for (repo, row, label) in &entries {
        let short = label.split('/').next().unwrap_or("").to_string();
        if prev_short.as_ref().is_some_and(|p| p != &short) {
            lines.push(border(&widths, '├', '┼', '┤'));
        }
        prev_short = Some(short);
        let selected = f.selected == Some(*row_idx);
        *row_idx += 1;
        lines.push(data_row(label, repo, row, f, name_w, selected));
    }
    lines.push(border(&widths, '└', '┴', '┘'));
    lines.push(Line::from(""));

    for e in &errors {
        if let Some(err) = &e.error {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("✗ {}: {err}", e.repo),
                    Style::default().fg(Color::Red),
                ),
            ]));
        }
    }
}

// ── row + cell construction ──────────────────────────────────────
fn column_widths(name_w: usize) -> Vec<usize> {
    vec![
        name_w, W_STATUS, W_STARTED, W_FINISHED, W_DURATION, W_ETA, W_RECENT, W_COMMIT,
    ]
}

fn header_row(name_w: usize, first: &str) -> Line<'static> {
    let titles = [
        first, "Status", "Started", "Finished", "Duration", "ETA", "Recent", "Commit",
    ];
    let widths = column_widths(name_w);
    let cells: Vec<(Vec<Span>, usize)> = titles
        .iter()
        .zip(&widths)
        .map(|(t, w)| {
            let txt = truncate(t, *w);
            let len = txt.chars().count();
            (vec![Span::styled(txt, bold(Color::White))], len)
        })
        .collect();
    row_line(cells, &widths)
}

fn data_row(
    label: &str,
    repo: &str,
    row: &WorkflowRow,
    f: &Frame,
    name_w: usize,
    selected: bool,
) -> Line<'static> {
    let widths = column_widths(name_w);
    let now = f.now.with_timezone(&Utc);

    // Status badge (spinner suffix while active).
    let status_text = if row.badge.is_active() {
        format!(
            "{} {}",
            SPINNER[f.spinner % SPINNER.len()],
            row.badge.label()
        )
    } else {
        row.badge.label().to_string()
    };
    let status_cell = text_cell(&status_text, badge_style(&row.badge));

    let started = fmt_time(row.started_at);
    let finished = fmt_time(row.finished_at);

    // Duration: elapsed for active runs, total for completed.
    let duration = match (&row.badge, row.started_at, row.finished_at) {
        (b, Some(s), _) if b.is_active() => fmt_duration((now - s).num_seconds().max(0)),
        (_, Some(s), Some(e)) => fmt_duration((e - s).num_seconds().max(0)),
        _ => "--".to_string(),
    };

    // ETA cell.
    let (eta_text, eta_style) = eta_cell(row, now);

    let cells = vec![
        text_cell_w(label, name_w, Style::default().fg(Color::White)),
        status_cell,
        text_cell(&started, dim()),
        text_cell(&finished, dim()),
        text_cell(&duration, dim()),
        (
            vec![Span::styled(truncate(&eta_text, W_ETA), eta_style)],
            eta_text.chars().count().min(W_ETA),
        ),
        recent_cell(&row.recent),
        commit_cell(row, repo, f.hyperlinks),
    ];
    let line = row_line(cells, &widths);
    if selected { highlight(line) } else { line }
}

/// Invert a row's spans to mark it as the current selection.
fn highlight(line: Line<'static>) -> Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|s| {
            let style = s.style.add_modifier(Modifier::REVERSED);
            Span::styled(s.content, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn eta_cell(row: &WorkflowRow, now: DateTime<Utc>) -> (String, Style) {
    if !row.badge.is_active() {
        return ("--".into(), dim());
    }
    match (row.eta_total_secs, row.started_at) {
        (Some(total), Some(start)) => {
            let elapsed = (now - start).num_seconds();
            let remaining = total - elapsed;
            if remaining >= 0 {
                (
                    format!("~{}", fmt_duration(remaining)),
                    Style::default().fg(Color::Yellow),
                )
            } else {
                (
                    format!("+{}", fmt_duration(-remaining)),
                    Style::default().fg(Color::Red),
                )
            }
        }
        _ => ("--".into(), dim()),
    }
}

/// The latest run's head commit, as a 7-char SHA. Red when the workflow is
/// failing, otherwise blue. In one-shot output it's an OSC-8 hyperlink to the
/// commit on GitHub (⌘-click in iTerm2); the TUI shows the same SHA without the
/// link (use the `o` key to open the selected row's commit).
fn commit_cell(row: &WorkflowRow, repo: &str, hyperlinks: bool) -> (Vec<Span<'static>>, usize) {
    match &row.head_sha {
        Some(sha) if !sha.is_empty() => {
            let short: String = sha.chars().take(7).collect();
            let len = short.chars().count();
            let color = if row.badge.is_failure() {
                Color::Red
            } else {
                Color::Blue
            };
            let style = Style::default().fg(color);
            if hyperlinks {
                let url = commit_url(repo, sha);
                (vec![Span::styled(osc8(&url, &short), style)], len)
            } else {
                (vec![Span::styled(short, style)], len)
            }
        }
        _ => text_cell("--", dim()),
    }
}

/// GitHub commit URL for a repo + sha.
pub fn commit_url(repo: &str, sha: &str) -> String {
    format!("https://github.com/{repo}/commit/{sha}")
}

/// Wrap `text` in an OSC-8 hyperlink to `url`.
fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

fn recent_cell(dots: &[Dot]) -> (Vec<Span<'static>>, usize) {
    let mut spans = Vec::new();
    let mut width = 0;
    for (i, d) in dots.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            width += 1;
        }
        let (ch, color) = match d {
            Dot::Pass => ("●", Color::Green),
            Dot::Fail => ("●", Color::Red),
            Dot::Active => ("◐", Color::Yellow),
            Dot::Other => ("○", Color::DarkGray),
        };
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        width += 1;
    }
    if spans.is_empty() {
        return text_cell("--", dim());
    }
    (spans, width)
}

/// A single-span text cell, hard-capped so it can never overflow a column.
fn text_cell(s: &str, style: Style) -> (Vec<Span<'static>>, usize) {
    text_cell_w(s, 64, style)
}

/// A single-span text cell truncated to `w` columns. Returns (spans, content_width).
fn text_cell_w(s: &str, w: usize, style: Style) -> (Vec<Span<'static>>, usize) {
    let t = truncate(s, w);
    let len = t.chars().count();
    (vec![Span::styled(t, style)], len)
}

/// Assemble a full table row from cells, padding each to its column width.
fn row_line(cells: Vec<(Vec<Span<'static>>, usize)>, widths: &[usize]) -> Line<'static> {
    let n = cells.len();
    let mut spans = vec![sep("│ ")];
    for (i, (cell_spans, cw)) in cells.into_iter().enumerate() {
        let w = widths[i];
        // Truncate already applied for text; clamp content width to column.
        let cw = cw.min(w);
        spans.extend(cell_spans);
        if w > cw {
            spans.push(Span::raw(" ".repeat(w - cw)));
        }
        spans.push(sep(if i + 1 < n { " │ " } else { " │" }));
    }
    Line::from(spans)
}

fn border(widths: &[usize], left: char, mid: char, right: char) -> Line<'static> {
    let mut s = String::new();
    s.push(left);
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            s.push(mid);
        }
        s.extend(std::iter::repeat_n('─', w + 2));
    }
    s.push(right);
    Line::from(Span::styled(s, dim()))
}

// ── formatting ───────────────────────────────────────────────────
fn fmt_time(t: Option<DateTime<Utc>>) -> String {
    match t {
        Some(t) => t.with_timezone(&Local).format("%m-%d %H:%M:%S").to_string(),
        None => "--".to_string(),
    }
}

fn fmt_duration(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

/// Render lines to an ANSI-colored string for one-shot (non-TUI) output.
pub fn lines_to_ansi(lines: &[Line]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            let codes = sgr_codes(&span.style);
            if codes.is_empty() {
                out.push_str(&span.content);
            } else {
                out.push_str(&format!("\x1b[{codes}m{}\x1b[0m", span.content));
            }
        }
        out.push('\n');
    }
    out
}

fn sgr_codes(style: &Style) -> String {
    let mut codes: Vec<&str> = Vec::new();
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1");
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("2");
    }
    if let Some(fg) = style.fg {
        codes.push(match fg {
            Color::Red => "31",
            Color::Green => "32",
            Color::Yellow => "33",
            Color::Blue => "34",
            Color::Cyan => "36",
            Color::White => "37",
            Color::DarkGray => "90",
            _ => "39",
        });
    }
    codes.join(";")
}

fn sanitize(s: &str) -> String {
    s.replace(['—', '–'], "-")
}

fn truncate(s: &str, w: usize) -> String {
    let s = sanitize(s);
    let len = s.chars().count();
    if len <= w {
        s
    } else if w == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(w - 1).collect();
        t.push('…');
        t
    }
}
