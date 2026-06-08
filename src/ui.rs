// SPDX-License-Identifier: Apache-2.0
//! Pure rendering: turn fetched results into styled Ratatui lines. The same
//! output drives both the one-shot snapshot and the live watch TUI.

use chrono::{DateTime, Local, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::model::{
    Badge, Dot, RateRow, RepoResult, RunPoint, StatsRow, WorkflowDetail, WorkflowRow,
};

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
        Badge::Running | Badge::Queued | Badge::Pending => Style::default().fg(Color::Yellow),
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
            "   ↑↓ · ⏎ detail · r rerun · o open · t stats · g rate · +/− speed · q quit"
                .to_string(),
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

/// A single-span text cell truncated to `w` columns. Returns (spans, `content_width`).
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
    t.map_or_else(
        || "--".to_string(),
        |t| t.with_timezone(&Local).format("%m-%d %H:%M:%S").to_string(),
    )
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
    use std::fmt::Write as _;
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            let codes = sgr_codes(&span.style);
            if codes.is_empty() {
                out.push_str(&span.content);
            } else {
                let _ = write!(out, "\x1b[{codes}m{}\x1b[0m", span.content);
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

// ── stats view ───────────────────────────────────────────────────
const W_VAL: usize = 7;
const W_DELTA: usize = 6;
const CHART_H: usize = 12;
const BLOCKS: [char; 8] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇'];

/// Max bars that fit at one column each, given the terminal `width`.
fn chart_capacity(width: usize, label_w: usize) -> usize {
    width.saturating_sub(label_w + 5).clamp(8, 600)
}

/// Lay out `n` bars across the available width: (bar width, gap, total columns).
fn bar_layout(n: usize, width: usize, label_w: usize) -> (usize, usize, usize) {
    let n = n.max(1);
    let avail = chart_capacity(width, label_w);
    let unit = (avail / n).max(1);
    let gap = usize::from(unit >= 3);
    let bar_w = (unit - gap).clamp(1, 12);
    (bar_w, gap, n * (bar_w + gap))
}

/// Render block bars — each column is `(fill_in_eighths, color)` — widened to
/// fill `width`, with a `top`/`bot` y-axis label. Returns (rows, total columns).
fn render_bars(
    cols: &[(i64, Color)],
    height: usize,
    label_w: usize,
    top: &str,
    bot: &str,
    width: usize,
) -> (Vec<Line<'static>>, usize) {
    let (bar_w, gap, total) = bar_layout(cols.len(), width, label_w);
    let mut lines = Vec::new();
    for r in 0..height {
        let rfb = (height - 1 - r) as i64;
        let ylab = if r == 0 {
            format!("{top:>label_w$}")
        } else if r == height - 1 {
            format!("{bot:>label_w$}")
        } else {
            " ".repeat(label_w)
        };
        let axis = if r == height - 1 { '┼' } else { '┤' };
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{ylab} {axis}"), dim()),
        ];
        for &(fl, color) in cols {
            let e = fl - rfb * 8;
            let ch = if e >= 8 {
                '█'
            } else if e <= 0 {
                ' '
            } else {
                BLOCKS[e as usize]
            };
            spans.push(Span::styled(
                ch.to_string().repeat(bar_w),
                Style::default().fg(color),
            ));
            if gap > 0 {
                spans.push(Span::raw(" ".repeat(gap)));
            }
        }
        lines.push(Line::from(spans));
    }
    (lines, total)
}

/// Everything the stats renderer needs for one frame.
pub struct StatsFrame<'a> {
    pub rows: &'a [StatsRow],
    pub now: DateTime<Local>,
    pub watch: Option<WatchInfo>,
    pub spinner: usize,
    pub loading: bool,
    pub selected: Option<usize>,
    /// Terminal width, so the chart fills it.
    pub width: u16,
}

pub fn build_stats_lines(f: &StatsFrame) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(stats_header_line(f));
    lines.push(Line::from(""));

    if f.rows.is_empty() {
        let msg = if f.loading {
            "fetching stats…"
        } else {
            "no stats"
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(msg.to_string(), dim()),
        ]));
        return lines;
    }

    let repo_w = f
        .rows
        .iter()
        .map(|r| short_name(&r.stats.repo).chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 40);
    let widths = stats_widths(repo_w);

    lines.push(border(&widths, '┌', '┬', '┐'));
    lines.push(stats_table_header(repo_w));
    lines.push(border(&widths, '├', '┼', '┤'));
    for (i, row) in f.rows.iter().enumerate() {
        lines.push(stats_row(row, repo_w, f.selected == Some(i)));
    }
    lines.push(border(&widths, '└', '┴', '┘'));
    lines.push(Line::from(""));

    // Full-width stars chart for the selected repo.
    if let Some(sel) = f.selected.and_then(|i| f.rows.get(i)) {
        for l in star_chart(
            &sel.stats.repo,
            &sel.trend,
            sel.stats.snapshot.stars,
            f.width as usize,
        ) {
            lines.push(l);
        }
    }
    lines
}

fn stats_header_line(f: &StatsFrame) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("GitHub Stats", bold(Color::Cyan)),
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
            "   ↑↓ select · t CI · g rate · +/− speed · q quit".to_string(),
            dim(),
        ));
    }
    Line::from(spans)
}

fn stats_widths(repo_w: usize) -> Vec<usize> {
    vec![
        repo_w, W_VAL, W_DELTA, W_VAL, W_DELTA, W_VAL, W_DELTA, W_VAL, W_DELTA, W_VAL, W_DELTA,
    ]
}

fn stats_table_header(repo_w: usize) -> Line<'static> {
    let widths = stats_widths(repo_w);
    let titles = [
        "Repo", "Stars", "Δ", "Forks", "Δ", "Watch", "Δ", "Issues", "Δ", "PRs", "Δ",
    ];
    let cells: Vec<(Vec<Span>, usize)> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            // Right-align value headers, left-align repo + deltas.
            let txt = if i > 0 && i % 2 == 1 {
                format!("{t:>w$}", w = widths[i])
            } else {
                truncate(t, widths[i])
            };
            let len = txt.chars().count().min(widths[i]);
            (vec![Span::styled(txt, bold(Color::White))], len)
        })
        .collect();
    row_line(cells, &widths)
}

fn stats_row(row: &StatsRow, repo_w: usize, selected: bool) -> Line<'static> {
    let widths = stats_widths(repo_w);
    let name = short_name(&row.stats.repo);

    if let Some(err) = &row.stats.error {
        let mut cells = vec![text_cell_w(&name, repo_w, Style::default().fg(Color::Red))];
        for _ in 0..10 {
            cells.push(text_cell("--", dim()));
        }
        let _ = err;
        let line = row_line(cells, &widths);
        return if selected { highlight(line) } else { line };
    }

    let s = &row.stats.snapshot;
    let p = row.prev;
    let metrics: [(i64, Option<i64>); 5] = [
        (s.stars, p.map(|p| p.stars)),
        (s.forks, p.map(|p| p.forks)),
        (s.watchers, p.map(|p| p.watchers)),
        (s.issues, p.map(|p| p.issues)),
        (s.prs, p.map(|p| p.prs)),
    ];

    let mut cells = vec![text_cell_w(
        &name,
        repo_w,
        Style::default().fg(Color::White),
    )];
    for (cur, prev) in metrics {
        cells.push(value_cell(cur));
        cells.push(delta_cell(cur, prev));
    }
    let line = row_line(cells, &widths);
    if selected { highlight(line) } else { line }
}

/// Right-justified numeric value cell.
fn value_cell(v: i64) -> (Vec<Span<'static>>, usize) {
    let t = format!("{v:>W_VAL$}");
    let len = t.chars().count().min(W_VAL);
    (
        vec![Span::styled(t, Style::default().fg(Color::White))],
        len,
    )
}

/// Day-over-day delta cell: ▲ green up, ▼ red down, · dim flat, "new" if no prior.
fn delta_cell(cur: i64, prev: Option<i64>) -> (Vec<Span<'static>>, usize) {
    let (text, color) = prev.map_or_else(
        || ("new".to_string(), Color::DarkGray),
        |p| match (cur - p).cmp(&0) {
            std::cmp::Ordering::Greater => (format!("▲{}", cur - p), Color::Green),
            std::cmp::Ordering::Less => (format!("▼{}", p - cur), Color::Red),
            std::cmp::Ordering::Equal => ("·".to_string(), Color::DarkGray),
        },
    );
    let t = truncate(&text, W_DELTA);
    let len = t.chars().count();
    (vec![Span::styled(t, Style::default().fg(color))], len)
}

/// A full-width block chart of a repo's star history.
fn star_chart(
    repo: &str,
    trend: &[(String, i64)],
    current: i64,
    width: usize,
) -> Vec<Line<'static>> {
    let short = short_name(repo);
    if trend.len() < 2 {
        return vec![Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{short} — stars: {current}"), bold(Color::Cyan)),
            Span::styled(
                "   (history builds daily — need ≥2 days for a chart)".to_string(),
                dim(),
            ),
        ])];
    }

    let cap = chart_capacity(width, 6);
    let pts: Vec<(&str, i64)> = trend
        .iter()
        .rev()
        .take(cap)
        .rev()
        .map(|(d, v)| (d.as_str(), *v))
        .collect();
    let vals: Vec<i64> = pts.iter().map(|(_, v)| *v).collect();
    let n = vals.len();
    let minv = vals.iter().copied().min().unwrap_or(0);
    let maxv = vals.iter().copied().max().unwrap_or(0);
    let lw = digits(maxv).max(digits(minv));
    let (_, _, total) = bar_layout(n, width, lw);

    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{short} — stars ({n}d)"), bold(Color::Cyan)),
        Span::styled(format!("   {minv} → {maxv}"), dim()),
    ])];

    if maxv == minv {
        // Flat series — a single level line.
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{maxv:>lw$} ┤"), dim()),
            Span::styled("─".repeat(total), Style::default().fg(Color::Green)),
        ]));
    } else {
        let levels = (CHART_H * 8) as f64;
        let cols: Vec<(i64, Color)> = vals
            .iter()
            .map(|&v| {
                let fl = (((v - minv) as f64 / (maxv - minv) as f64) * levels).round() as i64;
                (fl, Color::Green)
            })
            .collect();
        let (rows, _) = render_bars(
            &cols,
            CHART_H,
            lw,
            &maxv.to_string(),
            &minv.to_string(),
            width,
        );
        lines.extend(rows);
    }

    // X axis + first/last date labels.
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{} └{}", " ".repeat(lw), "─".repeat(total)), dim()),
    ]));
    let first = pts.first().map(|(d, _)| short_date(d)).unwrap_or_default();
    let last = pts.last().map(|(d, _)| short_date(d)).unwrap_or_default();
    let gap = total.saturating_sub(first.len() + last.len()).max(1);
    lines.push(Line::from(Span::styled(
        format!("  {} {first}{}{last}", " ".repeat(lw), " ".repeat(gap)),
        dim(),
    )));
    lines
}

fn short_name(repo: &str) -> String {
    repo.rsplit('/').next().unwrap_or(repo).to_string()
}

fn short_date(d: &str) -> String {
    // "YYYY-MM-DD" → "MM-DD"
    d.get(5..).unwrap_or(d).to_string()
}

fn digits(n: i64) -> usize {
    n.abs().to_string().len() + usize::from(n < 0)
}

// ── rate-limit view ──────────────────────────────────────────────
const W_RATE_RESET: usize = 22;

/// Everything the rate-limit renderer needs for one frame.
pub struct RateFrame<'a> {
    pub rows: &'a [RateRow],
    pub now: DateTime<Local>,
    pub watch: Option<WatchInfo>,
    pub spinner: usize,
    pub loading: bool,
}

pub fn build_rate_lines(f: &RateFrame) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(rate_header_line(f));
    lines.push(Line::from(""));

    if f.rows.is_empty() {
        let msg = if f.loading {
            "fetching rate limits…"
        } else {
            "no rate-limit data"
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(msg.to_string(), dim()),
        ]));
        return lines;
    }

    let name_w = f
        .rows
        .iter()
        .map(|r| r.bucket.name.chars().count())
        .max()
        .unwrap_or(10)
        .clamp(10, 24);
    let widths = rate_widths(name_w);

    lines.push(border(&widths, '┌', '┬', '┐'));
    lines.push(rate_header_row(name_w));
    lines.push(border(&widths, '├', '┼', '┤'));
    for row in f.rows {
        lines.push(rate_row(row, name_w, f.now));
    }
    lines.push(border(&widths, '└', '┴', '┘'));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "the rate_limit endpoint is free — polling it costs no quota".to_string(),
            dim(),
        ),
    ]));
    lines
}

fn rate_header_line(f: &RateFrame) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("GitHub API Rate Limits", bold(Color::Cyan)),
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
            "   g CI · +/− speed · q quit".to_string(),
            dim(),
        ));
    }
    Line::from(spans)
}

fn rate_widths(name_w: usize) -> Vec<usize> {
    vec![name_w, W_VAL, W_VAL, W_VAL, W_DELTA, W_RATE_RESET]
}

fn rate_header_row(name_w: usize) -> Line<'static> {
    let widths = rate_widths(name_w);
    let titles = ["Bucket", "Remaining", "Limit", "Used", "Δ", "Resets"];
    let cells: Vec<(Vec<Span>, usize)> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let txt = if (1..=3).contains(&i) {
                format!("{t:>w$}", w = widths[i])
            } else {
                truncate(t, widths[i])
            };
            let len = txt.chars().count().min(widths[i]);
            (vec![Span::styled(txt, bold(Color::White))], len)
        })
        .collect();
    row_line(cells, &widths)
}

fn rate_row(row: &RateRow, name_w: usize, now: DateTime<Local>) -> Line<'static> {
    let widths = rate_widths(name_w);
    let b = &row.bucket;
    let cells = vec![
        text_cell_w(&b.name, name_w, Style::default().fg(Color::White)),
        rate_value(b.remaining, rate_color(b.remaining, b.limit)),
        rate_value(b.limit, dim()),
        rate_value(b.used, dim()),
        rate_delta(row.delta_used),
        rate_reset_cell(b.reset, now),
    ];
    row_line(cells, &widths)
}

fn rate_color(remaining: i64, limit: i64) -> Style {
    let c = if remaining == 0 {
        Color::Red
    } else if limit > 0 && remaining < limit / 10 {
        Color::Yellow
    } else {
        Color::Green
    };
    Style::default().fg(c)
}

/// Right-justified value cell with an explicit style.
fn rate_value(v: i64, style: Style) -> (Vec<Span<'static>>, usize) {
    let t = format!("{v:>W_VAL$}");
    let len = t.chars().count().min(W_VAL);
    (vec![Span::styled(t, style)], len)
}

fn rate_delta(delta: Option<i64>) -> (Vec<Span<'static>>, usize) {
    let (text, color) = match delta {
        None => ("—".to_string(), Color::DarkGray),
        Some(d) if d > 0 => (format!("+{d}"), Color::Red),
        Some(d) if d < 0 => (format!("{d}"), Color::DarkGray),
        Some(_) => ("0".to_string(), Color::DarkGray),
    };
    let t = truncate(&text, W_DELTA);
    let len = t.chars().count();
    (vec![Span::styled(t, Style::default().fg(color))], len)
}

fn rate_reset_cell(reset: DateTime<Utc>, now: DateTime<Local>) -> (Vec<Span<'static>>, usize) {
    let secs = (reset - now.with_timezone(&Utc)).num_seconds();
    let when = reset.with_timezone(&Local).format("%H:%M:%S");
    let text = if secs > 0 {
        format!("{when} (in {})", fmt_duration(secs))
    } else {
        format!("{when} (now)")
    };
    text_cell_w(&text, W_RATE_RESET, dim())
}

// ── workflow detail view ─────────────────────────────────────────
const DETAIL_CHART_H: usize = 16;

/// Everything the workflow-detail renderer needs for one frame.
pub struct DetailFrame<'a> {
    pub repo: &'a str,
    pub workflow: &'a str,
    pub detail: Option<&'a WorkflowDetail>,
    pub now: DateTime<Local>,
    pub watch: Option<WatchInfo>,
    pub spinner: usize,
    pub loading: bool,
    /// Terminal width, so the chart fills it.
    pub width: u16,
}

pub fn build_detail_lines(f: &DetailFrame) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(detail_header_line(f));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{}/", f.repo), dim()),
        Span::styled(f.workflow.to_string(), bold(Color::White)),
    ]));
    lines.push(Line::from(""));

    let Some(detail) = f.detail else {
        let msg = if f.loading {
            "fetching run history…"
        } else {
            "no history"
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(msg.to_string(), dim()),
        ]));
        return lines;
    };

    if detail.runs.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("no runs in the last {} days", detail.days), dim()),
        ]));
        return lines;
    }

    lines.push(detail_summary_line(detail));
    lines.push(Line::from(""));
    for l in duration_chart(&detail.runs, f.width as usize) {
        lines.push(l);
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("bar height = run duration · ".to_string(), dim()),
        Span::styled("█".to_string(), Style::default().fg(Color::Green)),
        Span::styled(" pass  ".to_string(), dim()),
        Span::styled("█".to_string(), Style::default().fg(Color::Red)),
        Span::styled(" fail  ".to_string(), dim()),
        Span::styled("◐".to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(" running".to_string(), dim()),
    ]));
    lines
}

fn detail_header_line(f: &DetailFrame) -> Line<'static> {
    let days = f.detail.map_or(7, |d| d.days);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("Workflow — last {days} days"), bold(Color::Cyan)),
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
            "   esc back · r rerun · +/− speed · q quit".to_string(),
            dim(),
        ));
    }
    Line::from(spans)
}

fn detail_summary_line(d: &WorkflowDetail) -> Line<'static> {
    let total = d.runs.len();
    let pass = d.runs.iter().filter(|r| matches!(r.dot, Dot::Pass)).count();
    let fail = d.runs.iter().filter(|r| matches!(r.dot, Dot::Fail)).count();
    let decided = pass + fail;
    let rate = if decided > 0 { pass * 100 / decided } else { 0 };
    let completed: Vec<i64> = d
        .runs
        .iter()
        .filter(|r| r.duration_secs > 0)
        .map(|r| r.duration_secs)
        .collect();
    let avg = if completed.is_empty() {
        0
    } else {
        completed.iter().sum::<i64>() / completed.len() as i64
    };
    let slowest = completed.iter().copied().max().unwrap_or(0);

    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{total} runs"), bold(Color::White)),
        Span::styled("  ·  ".to_string(), dim()),
        Span::styled(format!("{pass} pass"), Style::default().fg(Color::Green)),
        Span::styled(" / ".to_string(), dim()),
        Span::styled(format!("{fail} fail"), Style::default().fg(Color::Red)),
        Span::styled(format!("  ·  {rate}% green"), dim()),
        Span::styled(
            format!(
                "  ·  avg {}  ·  slowest {}",
                fmt_duration(avg),
                fmt_duration(slowest)
            ),
            dim(),
        ),
    ])
}

fn dot_color(d: Dot) -> Color {
    match d {
        Dot::Pass => Color::Green,
        Dot::Fail => Color::Red,
        Dot::Active => Color::Yellow,
        Dot::Other => Color::DarkGray,
    }
}

/// A per-column-colored block bar chart of run durations (baseline 0), widened
/// to fill the terminal.
fn duration_chart(runs: &[RunPoint], width: usize) -> Vec<Line<'static>> {
    let cap = chart_capacity(width, 6);
    let pts: Vec<&RunPoint> = runs.iter().rev().take(cap).rev().collect();
    let maxv = pts
        .iter()
        .map(|r| r.duration_secs)
        .max()
        .unwrap_or(0)
        .max(1);
    let top_label = fmt_duration(maxv);
    let label_w = top_label.chars().count().max(2);
    let levels = (DETAIL_CHART_H * 8) as f64;
    let cols: Vec<(i64, Color)> = pts
        .iter()
        .map(|r| {
            let fl = ((r.duration_secs as f64 / maxv as f64) * levels).round() as i64;
            (fl, dot_color(r.dot))
        })
        .collect();

    let (mut lines, total) = render_bars(&cols, DETAIL_CHART_H, label_w, &top_label, "0", width);
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{} └{}", " ".repeat(label_w), "─".repeat(total)),
            dim(),
        ),
    ]));
    let fmt_d = |r: &&RunPoint| r.started.with_timezone(&Local).format("%m-%d").to_string();
    let first = pts.first().map(fmt_d).unwrap_or_default();
    let last = pts.last().map(fmt_d).unwrap_or_default();
    let gap = total.saturating_sub(first.len() + last.len()).max(1);
    lines.push(Line::from(Span::styled(
        format!("  {} {first}{}{last}", " ".repeat(label_w), " ".repeat(gap)),
        dim(),
    )));
    lines
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RepoStats, Snapshot, StatsRow};

    #[test]
    fn stats_view_renders_delta_and_chart() {
        let row = StatsRow {
            stats: RepoStats {
                repo: "owner/repo".to_string(),
                snapshot: Snapshot {
                    stars: 20,
                    forks: 2,
                    watchers: 3,
                    issues: 1,
                    prs: 0,
                },
                error: None,
            },
            prev: Some(Snapshot {
                stars: 17,
                forks: 2,
                watchers: 3,
                issues: 1,
                prs: 0,
            }),
            trend: vec![
                ("2026-06-01".to_string(), 10),
                ("2026-06-02".to_string(), 14),
                ("2026-06-03".to_string(), 20),
            ],
        };
        let f = StatsFrame {
            rows: std::slice::from_ref(&row),
            now: Local::now(),
            watch: None,
            spinner: 0,
            loading: false,
            selected: Some(0),
            width: 120,
        };
        let txt = lines_to_ansi(&build_stats_lines(&f));

        assert!(txt.contains("repo"), "short repo name in table");
        assert!(txt.contains("stars"), "chart title present");
        assert!(txt.contains("▲3"), "stars delta 20-17 = +3");
        let has_bar = "▁▂▃▄▅▆▇█".chars().any(|c| txt.contains(c));
        assert!(has_bar, "chart should render at least one block bar");
    }

    #[test]
    fn rate_view_renders() {
        use crate::model::{RateBucket, RateRow};
        let row = RateRow {
            bucket: RateBucket {
                name: "core".to_string(),
                limit: 5000,
                used: 1601,
                remaining: 3399,
                reset: Utc::now() + chrono::Duration::minutes(12),
            },
            delta_used: Some(7),
        };
        let f = RateFrame {
            rows: std::slice::from_ref(&row),
            now: Local::now(),
            watch: None,
            spinner: 0,
            loading: false,
        };
        let txt = lines_to_ansi(&build_rate_lines(&f));
        assert!(txt.contains("core"), "bucket name shown");
        assert!(txt.contains("3399"), "remaining shown");
        assert!(txt.contains("+7"), "used delta shown");
        assert!(txt.contains("costs no quota"), "free-endpoint footer shown");
    }

    #[test]
    fn detail_view_renders() {
        use crate::model::{RunPoint, WorkflowDetail};
        let now = Utc::now();
        let runs = vec![
            RunPoint {
                started: now - chrono::Duration::days(2),
                duration_secs: 120,
                dot: Dot::Pass,
            },
            RunPoint {
                started: now - chrono::Duration::days(1),
                duration_secs: 300,
                dot: Dot::Fail,
            },
            RunPoint {
                started: now,
                duration_secs: 60,
                dot: Dot::Pass,
            },
        ];
        let detail = WorkflowDetail { days: 7, runs };
        let f = DetailFrame {
            repo: "owner/repo",
            workflow: "CI",
            detail: Some(&detail),
            now: Local::now(),
            watch: None,
            spinner: 0,
            loading: false,
            width: 120,
        };
        let txt = lines_to_ansi(&build_detail_lines(&f));
        assert!(txt.contains("CI"), "workflow name shown");
        assert!(txt.contains("3 runs"), "run count shown");
        assert!(txt.contains("2 pass"), "pass count shown");
        assert!(txt.contains("1 fail"), "fail count shown");
        let has_bar = "▁▂▃▄▅▆▇█".chars().any(|c| txt.contains(c));
        assert!(has_bar, "duration chart renders bars");
    }

    #[test]
    fn chart_needs_two_points() {
        let row = StatsRow {
            stats: RepoStats {
                repo: "owner/repo".to_string(),
                snapshot: Snapshot {
                    stars: 5,
                    forks: 0,
                    watchers: 0,
                    issues: 0,
                    prs: 0,
                },
                error: None,
            },
            prev: None,
            trend: vec![("2026-06-03".to_string(), 5)],
        };
        let f = StatsFrame {
            rows: std::slice::from_ref(&row),
            now: Local::now(),
            watch: None,
            spinner: 0,
            loading: false,
            selected: Some(0),
            width: 120,
        };
        let txt = lines_to_ansi(&build_stats_lines(&f));
        assert!(
            txt.contains("history builds daily"),
            "single point → note, not chart"
        );
        assert!(txt.contains("new"), "no prior snapshot → 'new' delta");
    }
}
