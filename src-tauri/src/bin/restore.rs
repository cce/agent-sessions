//! TUI for browsing and restoring agent sessions from a JSONL backup.
//!
//! Usage: cargo run --bin restore <backup.jsonl> [--dry-run]
//!
//! Keys:
//!   up/down, j/k   Navigate sessions
//!   space, x        Toggle selection
//!   right, l        Detail view (full message context)
//!   left, esc, h    Back from detail
//!   m               Load more context from session file on disk
//!   enter            Restore selected sessions
//!   q                Quit

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, MouseEventKind, EnableMouseCapture, DisableMouseCapture},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::collections::HashMap;
use std::io::{self, BufRead};

use tauri_temp_lib::session::convert_path_to_dir_name;
use tauri_temp_lib::terminal;

// --- Data types ---

#[derive(serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct BackupSession {
    id: String,
    agent_type: String,
    project_name: String,
    project_path: String,
    git_branch: Option<String>,
    last_message: Option<String>,
    last_message_role: Option<String>,
    last_activity_at: Option<String>,
    window_id: Option<String>,
}

struct WindowGroup {
    label: String,
    #[allow(dead_code)]
    window_id: String,
    sessions: Vec<BackupSession>,
}

/// A row in the flattened list: either a group header or a session
enum Row {
    Header(usize),
    Session(usize, usize),
}

// --- Loaded conversation context ---

struct LoadedContext {
    messages: Vec<ContextMessage>,
    fully_loaded: bool,
}

struct ContextMessage {
    role: String,
    text: String,
}

// --- App state ---

enum Mode {
    List,
    Detail,
}

struct App {
    groups: Vec<WindowGroup>,
    rows: Vec<Row>,
    selected: HashMap<(usize, usize), bool>,
    list_state: ListState,
    scroll_offset: u16,
    /// Total lines in the detail pane content (set during render)
    detail_content_height: u16,
    /// Visible height of the detail pane (set during render)
    detail_visible_height: u16,
    mode: Mode,
    dry_run: bool,
    should_quit: bool,
    should_restore: bool,
    /// Cached loaded context per (group_idx, session_idx)
    loaded_context: HashMap<(usize, usize), LoadedContext>,
}

impl App {
    fn new(groups: Vec<WindowGroup>, dry_run: bool) -> Self {
        let rows = build_rows(&groups);
        let mut list_state = ListState::default();
        let first_session = rows.iter().position(|r| matches!(r, Row::Session(_, _)));
        list_state.select(first_session.or(Some(0)));
        App {
            groups,
            rows,
            selected: HashMap::new(),
            list_state,
            scroll_offset: 0,
            detail_content_height: 0,
            detail_visible_height: 0,
            mode: Mode::List,
            dry_run,
            should_quit: false,
            should_restore: false,
            loaded_context: HashMap::new(),
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        self.list_state.selected().and_then(|i| self.rows.get(i))
    }

    fn current_session_key(&self) -> Option<(usize, usize)> {
        match self.selected_row() {
            Some(Row::Session(g, s)) => Some((*g, *s)),
            _ => None,
        }
    }

    fn toggle_selection(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            match &self.rows[idx] {
                Row::Session(g, s) => {
                    let key = (*g, *s);
                    let current = self.selected.get(&key).copied().unwrap_or(false);
                    self.selected.insert(key, !current);
                }
                Row::Header(g) => {
                    let g = *g;
                    let count = self.groups[g].sessions.len();
                    let all_selected = (0..count)
                        .all(|s| self.selected.get(&(g, s)).copied().unwrap_or(false));
                    for s in 0..count {
                        self.selected.insert((g, s), !all_selected);
                    }
                }
            }
        }
    }

    fn selection_count(&self) -> usize {
        self.selected.values().filter(|&&v| v).count()
    }

    fn move_down(&mut self) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some((i + 1).min(len - 1)));
    }

    fn move_up(&mut self) {
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(i.saturating_sub(1)));
    }

    fn enter_detail(&mut self) {
        if let Some(Row::Session(_, _)) = self.selected_row() {
            self.scroll_offset = 0;
            self.mode = Mode::Detail;
        }
    }

    fn get_selected_sessions(&self) -> Vec<(usize, &BackupSession)> {
        let mut result = Vec::new();
        for (&(g, s), &selected) in &self.selected {
            if selected {
                result.push((g, &self.groups[g].sessions[s]));
            }
        }
        result.sort_by_key(|(g, _)| *g);
        result
    }

    fn max_scroll(&self) -> u16 {
        self.detail_content_height
            .saturating_sub(self.detail_visible_height)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_offset = self.scroll_offset.min(self.max_scroll());
    }

    fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = (self.scroll_offset + lines).min(self.max_scroll());
    }

    fn scroll_up(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll();
    }

    fn page_size(&self) -> u16 {
        self.detail_visible_height.saturating_sub(2).max(1)
    }

    fn load_more_context(&mut self) {
        let Some((g, s)) = self.current_session_key() else {
            return;
        };
        let session = &self.groups[g].sessions[s];

        // Already fully loaded
        if let Some(ctx) = self.loaded_context.get(&(g, s)) {
            if ctx.fully_loaded {
                return;
            }
        }

        let messages = load_session_messages(&session.agent_type, &session.project_path, &session.id);
        let fully_loaded = true;
        self.loaded_context.insert(
            (g, s),
            LoadedContext {
                messages,
                fully_loaded,
            },
        );
        // Reset scroll to bottom to show newest messages
        self.scroll_offset = 0;
    }
}

fn build_rows(groups: &[WindowGroup]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        rows.push(Row::Header(gi));
        for si in 0..group.sessions.len() {
            rows.push(Row::Session(gi, si));
        }
    }
    rows
}

// --- Loading messages from disk ---

fn load_session_messages(agent_type: &str, project_path: &str, session_id: &str) -> Vec<ContextMessage> {
    match agent_type {
        "codex" => load_codex_messages(session_id),
        _ => load_claude_messages(project_path, session_id),
    }
}

fn load_claude_messages(project_path: &str, session_id: &str) -> Vec<ContextMessage> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    let dir_name = convert_path_to_dir_name(project_path);
    let project_dir = home.join(".claude").join("projects").join(&dir_name);

    if !project_dir.exists() {
        return Vec::new();
    }

    let mut jsonl_files: Vec<_> = std::fs::read_dir(&project_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".jsonl") && !name.starts_with("agent-")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let mtime = meta.modified().ok()?;
            Some((e.path(), mtime))
        })
        .collect();
    jsonl_files.sort_by(|a, b| b.1.cmp(&a.1));

    for (path, _) in &jsonl_files {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = io::BufReader::new(file);
        let mut file_messages = Vec::new();
        let mut matched_session = false;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let parsed: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(sid) = parsed.get("sessionId").and_then(|v| v.as_str()) {
                if sid == session_id {
                    matched_session = true;
                }
            }

            if !matched_session {
                continue;
            }

            let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if msg_type != "user" && msg_type != "assistant" {
                continue;
            }

            let message = match parsed.get("message") {
                Some(m) => m,
                None => continue,
            };

            let role = message
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or(msg_type)
                .to_string();

            let text = extract_text_from_content(message.get("content"));
            if text.is_empty() {
                continue;
            }

            file_messages.push(ContextMessage { role, text });
        }

        if !file_messages.is_empty() {
            return file_messages;
        }
    }

    Vec::new()
}

/// Find a codex session file by scanning ~/.codex/sessions/ date dirs for a
/// filename containing the session ID.
fn find_codex_session_file(session_id: &str) -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let sessions_dir = home.join(".codex").join("sessions");
    if !sessions_dir.exists() {
        return None;
    }

    // Session ID is embedded in the filename: rollout-DATE-SESSION_ID.jsonl
    // Scan year/month/day dirs, newest first
    let mut year_dirs: Vec<_> = std::fs::read_dir(&sessions_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    year_dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    for year in &year_dirs {
        let mut month_dirs: Vec<_> = std::fs::read_dir(year.path())
            .ok()?
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        month_dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

        for month in &month_dirs {
            let mut day_dirs: Vec<_> = std::fs::read_dir(month.path())
                .ok()?
                .flatten()
                .filter(|e| e.path().is_dir())
                .collect();
            day_dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

            for day in &day_dirs {
                if let Ok(entries) = std::fs::read_dir(day.path()) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if name.contains(session_id) && name.ends_with(".jsonl") {
                            return Some(entry.path());
                        }
                    }
                }
            }
        }
    }

    None
}

fn load_codex_messages(session_id: &str) -> Vec<ContextMessage> {
    let path = match find_codex_session_file(session_id) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = io::BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let parsed: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Codex stores messages as "response_item" events with payload.role
        if event_type != "response_item" {
            continue;
        }

        let payload = match parsed.get("payload") {
            Some(p) => p,
            None => continue,
        };

        let role = match payload.get("role").and_then(|v| v.as_str()) {
            Some("user") => "user".to_string(),
            Some("assistant") => "assistant".to_string(),
            _ => continue,
        };

        let text = extract_text_from_content(payload.get("content"));
        if text.is_empty() {
            continue;
        }

        messages.push(ContextMessage { role, text });
    }

    messages
}

/// Extract readable text from a message content field (works for both
/// Claude and Codex formats)
fn extract_text_from_content(content: Option<&serde_json::Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };

    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(block_type) = block.get("type").and_then(|v| v.as_str()) {
                    match block_type {
                        "text" | "input_text" | "output_text" => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                parts.push(text.to_string());
                            }
                        }
                        "tool_use" | "function_call" => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool");
                            parts.push(format!("[tool_use: {}]", name));
                        }
                        "tool_result" | "function_call_output" => {
                            parts.push("[tool_result]".to_string());
                        }
                        _ => {}
                    }
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

// --- Data loading ---

fn load_backup(path: &str) -> Vec<BackupSession> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| {
        eprintln!("Failed to open {}: {}", path, e);
        std::process::exit(1);
    });
    let reader = io::BufReader::new(file);
    let mut sessions = Vec::new();
    for line in reader.lines() {
        let line = line.unwrap();
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        if let Ok(s) = serde_json::from_str::<BackupSession>(line) {
            sessions.push(s);
        }
    }
    sessions
}

fn group_by_window(sessions: Vec<BackupSession>) -> Vec<WindowGroup> {
    let mut groups: Vec<WindowGroup> = Vec::new();
    let mut key_index: HashMap<String, usize> = HashMap::new();
    let mut ungrouped = 0usize;

    for s in sessions {
        let key = match &s.window_id {
            Some(wid) => wid.clone(),
            None => {
                ungrouped += 1;
                format!("(ungrouped #{})", ungrouped)
            }
        };
        if let Some(&idx) = key_index.get(&key) {
            groups[idx].sessions.push(s);
        } else {
            let idx = groups.len();
            key_index.insert(key.clone(), idx);
            groups.push(WindowGroup {
                label: String::new(),
                window_id: key,
                sessions: vec![s],
            });
        }
    }

    for group in &mut groups {
        group.sessions.sort_by(|a, b| {
            b.last_activity_at
                .as_deref()
                .unwrap_or("")
                .cmp(a.last_activity_at.as_deref().unwrap_or(""))
        });
        let mut projects = Vec::new();
        for s in &group.sessions {
            if !projects.contains(&s.project_name) {
                projects.push(s.project_name.clone());
            }
        }
        group.label = if projects.len() > 3 {
            format!("{} +{}", projects[..3].join(", "), projects.len() - 3)
        } else {
            projects.join(", ")
        };
    }

    groups.sort_by(|a, b| {
        let a_ts = a
            .sessions
            .first()
            .and_then(|s| s.last_activity_at.as_deref())
            .unwrap_or("");
        let b_ts = b
            .sessions
            .first()
            .and_then(|s| s.last_activity_at.as_deref())
            .unwrap_or("");
        b_ts.cmp(a_ts)
    });

    groups
}

fn format_age(iso_ts: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso_ts) else {
        return String::new();
    };
    let delta = chrono::Utc::now().signed_duration_since(dt);
    let days = delta.num_days();
    if days == 0 {
        let hours = delta.num_hours();
        if hours == 0 {
            return format!("{}m", delta.num_minutes());
        }
        return format!("{}h", hours);
    }
    if days < 30 {
        format!("{}d", days)
    } else {
        format!("{}mo", days / 30)
    }
}

fn resume_command(session: &BackupSession) -> String {
    match session.agent_type.as_str() {
        "codex" => format!("codex --resume {}", session.id),
        _ => format!("claude --resume {}", session.id),
    }
}

fn one_line_message(msg: &str, max_len: usize) -> String {
    let cleaned: String = msg
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = cleaned
        .trim_start_matches('#')
        .trim_start_matches('-')
        .trim_start_matches('*')
        .trim();
    if cleaned.len() <= max_len {
        cleaned.to_string()
    } else {
        format!("{}...", &cleaned[..max_len.saturating_sub(3)])
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}~", &s[..n.saturating_sub(1)])
    }
}

// --- Rendering ---

fn render_list(f: &mut Frame, app: &App) {
    let chunks =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());

    let list_area = chunks[0];
    let status_area = chunks[1];
    let total_width = list_area.width as usize;

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            Row::Header(g) => {
                let group = &app.groups[*g];
                let n = group.sessions.len();
                let all_selected =
                    (0..n).all(|s| app.selected.get(&(*g, s)).copied().unwrap_or(false));
                let check = if all_selected && n > 0 { "[x]" } else { "[ ]" };
                let text = format!(
                    " {} {} ({} session{})",
                    check,
                    group.label,
                    n,
                    if n != 1 { "s" } else { "" }
                );
                let style = if app.list_state.selected() == Some(i) {
                    Style::default()
                        .fg(Color::Cyan)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                };
                ListItem::new(Line::from(Span::styled(text, style)))
            }
            Row::Session(g, s) => {
                let session = &app.groups[*g].sessions[*s];
                let is_selected = app.selected.get(&(*g, *s)).copied().unwrap_or(false);
                let check = if is_selected { "[x]" } else { "[ ]" };
                let is_cursor = app.list_state.selected() == Some(i);

                let agent_color = match session.agent_type.as_str() {
                    "claude" => Color::Yellow,
                    "codex" => Color::Green,
                    _ => Color::White,
                };

                let age = session
                    .last_activity_at
                    .as_deref()
                    .map(format_age)
                    .unwrap_or_default();

                let branch = session.git_branch.as_deref().unwrap_or("");
                let role_marker = match session.last_message_role.as_deref() {
                    Some("user") => "<<",
                    _ => ">>",
                };

                let prefix_len = 4 + 4 + 7 + 21 + 17 + 5 + 3;
                let msg_width = total_width.saturating_sub(prefix_len);
                let msg = session
                    .last_message
                    .as_deref()
                    .map(|m| one_line_message(m, msg_width))
                    .unwrap_or_default();

                let bg = if is_cursor {
                    Some(Color::DarkGray)
                } else {
                    None
                };
                let style = |fg: Color| {
                    let mut s = Style::default().fg(fg);
                    if let Some(bg) = bg {
                        s = s.bg(bg);
                    }
                    s
                };

                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("   {} ", check),
                        if is_selected {
                            style(Color::Green)
                        } else {
                            style(Color::Gray)
                        },
                    ),
                    Span::styled(format!("{:6} ", session.agent_type), style(agent_color)),
                    Span::styled(
                        format!("{:20} ", truncate(&session.project_name, 20)),
                        style(Color::White),
                    ),
                    Span::styled(
                        format!("{:16} ", truncate(branch, 16)),
                        style(Color::DarkGray),
                    ),
                    Span::styled(format!("{:4} ", age), style(Color::DarkGray)),
                    Span::styled(format!("{} ", role_marker), style(Color::DarkGray)),
                    Span::styled(msg, style(Color::Gray)),
                ]))
            }
        })
        .collect();

    let sel_count = app.selection_count();
    let footer = if sel_count > 0 {
        format!(
            " {} selected | enter: restore | space: toggle | right: detail | q: quit ",
            sel_count
        )
    } else {
        " space: toggle | right: detail | enter: restore | q: quit ".to_string()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Sessions ")
                .title_bottom(footer),
        )
        .highlight_style(Style::default());

    f.render_stateful_widget(list, list_area, &mut app.list_state.clone());

    let total: usize = app.groups.iter().map(|g| g.sessions.len()).sum();
    let status = format!(
        " {} sessions in {} groups | {} selected",
        total,
        app.groups.len(),
        sel_count
    );
    f.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
        status_area,
    );
}

fn render_detail(f: &mut Frame, app: &mut App) {
    let Some(Row::Session(g, s)) = app.selected_row() else {
        return;
    };
    let g = *g;
    let s = *s;
    let key = (g, s);

    // Clone what we need to avoid borrow conflicts with app
    let session = app.groups[g].sessions[s].clone();

    let area = centered_rect(85, 85, f.area());
    app.detail_visible_height = area.height.saturating_sub(2);
    f.render_widget(Clear, area);

    let branch = session.git_branch.as_deref().unwrap_or("");
    let age = session
        .last_activity_at
        .as_deref()
        .map(format_age)
        .unwrap_or_default();

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Agent:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.agent_type, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("Project: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.project_name, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Path:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.project_path, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Branch:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(branch, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("ID:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.id, Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("Active:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                session.last_activity_at.as_deref().unwrap_or(""),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(format!(" ({})", age), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("Restore: ", Style::default().fg(Color::DarkGray)),
            Span::styled(resume_command(&session), Style::default().fg(Color::Green)),
        ]),
        Line::from(""),
    ];

    // Check if we have loaded context from disk
    if let Some(ctx) = app.loaded_context.get(&key) {
        lines.push(Line::from(Span::styled(
            format!("Conversation ({} messages):", ctx.messages.len()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        for msg in &ctx.messages {
            let (role_label, role_color) = if msg.role == "user" {
                ("User", Color::Blue)
            } else {
                ("Assistant", Color::Yellow)
            };

            lines.push(Line::from(Span::styled(
                format!("[{}]", role_label),
                Style::default()
                    .fg(role_color)
                    .add_modifier(Modifier::BOLD),
            )));
            for text_line in msg.text.lines() {
                lines.push(Line::from(text_line.to_string()));
            }
            lines.push(Line::from(""));
        }
    } else {
        // Show the backup's last_message (may be truncated)
        lines.push(Line::from(Span::styled(
            "Last message (press m to load full context from disk):",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        if let Some(msg) = &session.last_message {
            let role = session.last_message_role.as_deref().unwrap_or("?");
            let (role_label, role_color) = if role == "user" {
                ("User", Color::Blue)
            } else {
                ("Assistant", Color::Yellow)
            };
            lines.push(Line::from(Span::styled(
                format!("[{}]", role_label),
                Style::default()
                    .fg(role_color)
                    .add_modifier(Modifier::BOLD),
            )));
            for text_line in msg.lines() {
                lines.push(Line::from(text_line.to_string()));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "(no messages)",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let is_selected = app.selected.get(&key).copied().unwrap_or(false);
    let sel_indicator = if is_selected { " [selected]" } else { "" };
    let has_context = app.loaded_context.contains_key(&key);

    let title = format!(" {} - {}{} ", session.project_name, branch, sel_indicator);
    let footer = if has_context {
        " left: back | space: toggle | scroll/pgup/pgdn | g/G: top/bottom "
    } else {
        " left: back | space: toggle | m: load context | scroll/pgup/pgdn "
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(footer),
        )
        .wrap(Wrap { trim: false });

    app.detail_content_height = paragraph.line_count(area.width.saturating_sub(2)) as u16;
    app.clamp_scroll();

    let paragraph = paragraph.scroll((app.scroll_offset, 0));
    f.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

// --- Restore ---

fn do_restore(rt: &tokio::runtime::Runtime, app: &App) {
    let selected = app.get_selected_sessions();
    if selected.is_empty() {
        return;
    }

    let mut by_group: HashMap<usize, Vec<&BackupSession>> = HashMap::new();
    for (g, session) in &selected {
        by_group.entry(*g).or_default().push(session);
    }

    let mut group_indices: Vec<usize> = by_group.keys().copied().collect();
    group_indices.sort();

    if app.dry_run {
        println!("\n[dry-run] Would restore {} sessions:\n", selected.len());
        for gi in &group_indices {
            let group = &app.groups[*gi];
            let sessions = &by_group[gi];
            println!("  Window: {}", group.label);
            for s in sessions {
                println!("    cd {} && {}", s.project_path, resume_command(s));
            }
        }
        return;
    }

    rt.block_on(async {
        let mut ws = match terminal::connect().await {
            Ok(ws) => ws,
            Err(e) => {
                eprintln!("Failed to connect to iTerm2: {}", e);
                return;
            }
        };

        let mut req_id = 1i64;

        for gi in &group_indices {
            let group = &app.groups[*gi];
            let sessions = &by_group[gi];
            println!(
                "Restoring window: {} ({} sessions)",
                group.label,
                sessions.len()
            );

            let mut window_id: Option<String> = None;

            for s in sessions {
                let tab_title = format!(
                    "{} ({})",
                    s.project_name,
                    s.git_branch.as_deref().unwrap_or(""),
                );

                let (wid, _tab_id, iterm_session_id) = match terminal::create_tab(
                    &mut ws,
                    req_id,
                    window_id.as_deref(),
                    Some(&s.project_path),
                    Some(&tab_title),
                )
                .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        eprintln!("  Failed to create tab for {}: {}", s.project_name, e);
                        req_id += 1;
                        continue;
                    }
                };
                req_id += 1;

                if window_id.is_none() {
                    window_id = Some(wid);
                }

                let cmd = format!("{}\n", resume_command(s));
                if let Err(e) =
                    terminal::send_text(&mut ws, req_id, &iterm_session_id, &cmd).await
                {
                    eprintln!("  Failed to send command to {}: {}", s.project_name, e);
                }
                req_id += 1;

                println!("  -> {} [{}]", s.project_name, s.agent_type);
            }
        }
    });

    println!("\nDone. Restored {} sessions.", selected.len());
}

// --- Main ---

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let path = args
        .iter()
        .find(|a| !a.starts_with('-') && *a != &args[0])
        .unwrap_or_else(|| {
            eprintln!("Usage: restore <backup.jsonl> [--dry-run]");
            std::process::exit(1);
        })
        .clone();

    let sessions = load_backup(&path);
    if sessions.is_empty() {
        eprintln!("No sessions found in {}", path);
        std::process::exit(1);
    }

    let groups = group_by_window(sessions);
    let mut app = App::new(groups, dry_run);

    let rt = tokio::runtime::Runtime::new()?;

    enable_raw_mode()?;
    io::stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| match app.mode {
            Mode::List => render_list(f, &app),
            Mode::Detail => {
                render_list(f, &app);
                render_detail(f, &mut app);
            }
        })?;

        if app.should_quit || app.should_restore {
            break;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match app.mode {
                Mode::List => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                    KeyCode::Char(' ') | KeyCode::Char('x') => app.toggle_selection(),
                    KeyCode::Right | KeyCode::Char('l') => app.enter_detail(),
                    KeyCode::Enter => {
                        if app.selection_count() > 0 {
                            app.should_restore = true;
                        }
                    }
                    _ => {}
                },
                Mode::Detail => match key.code {
                    KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                        app.mode = Mode::List;
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.scroll_down(1),
                    KeyCode::Up | KeyCode::Char('k') => app.scroll_up(1),
                    KeyCode::PageDown => {
                        let pg = app.page_size();
                        app.scroll_down(pg);
                    }
                    KeyCode::PageUp => {
                        let pg = app.page_size();
                        app.scroll_up(pg);
                    }
                    KeyCode::Home | KeyCode::Char('g') => app.scroll_to_top(),
                    KeyCode::End | KeyCode::Char('G') => app.scroll_to_bottom(),
                    KeyCode::Char(' ') | KeyCode::Char('x') => app.toggle_selection(),
                    KeyCode::Char('m') => app.load_more_context(),
                    _ => {}
                },
            },
            Event::Mouse(mouse) => {
                if matches!(app.mode, Mode::Detail) {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => app.scroll_down(3),
                        MouseEventKind::ScrollUp => app.scroll_up(3),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    disable_raw_mode()?;
    io::stdout()
        .execute(DisableMouseCapture)?
        .execute(LeaveAlternateScreen)?;

    if app.should_restore {
        do_restore(&rt, &app);
    }

    Ok(())
}
