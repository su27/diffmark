use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::clipboard::copy_to_clipboard;
use crate::diff_parser::{DiffLine, LineKind, ParsedDiff, parse_unified_diff};
use crate::git_diff::{DiffOptions, read_uncommitted_diff};
use crate::payload::{build_review_payload, describe_target, describe_target_lines};

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub diff: DiffOptions,
    pub refresh_interval: Duration,
    pub max_copy_lines: usize,
    pub allow_osc52: bool,
}

#[derive(Clone, Debug)]
struct InlineComment {
    target_lines: Vec<DiffLine>,
    text: String,
    created_at: u64,
}

#[derive(Clone, Debug)]
enum RenderRow {
    Diff(usize),
    Comment(InlineComment),
}

#[derive(Clone, Debug)]
struct TextEditor {
    target_lines: Vec<DiffLine>,
    replacing_comment: Option<u64>,
    lines: Vec<String>,
    cursor_y: usize,
    cursor_x: usize,
}

impl TextEditor {
    fn new(target_lines: Vec<DiffLine>) -> Self {
        Self {
            target_lines,
            replacing_comment: None,
            lines: vec![String::new()],
            cursor_y: 0,
            cursor_x: 0,
        }
    }

    fn replacing(target_lines: Vec<DiffLine>, comment: &InlineComment) -> Self {
        let mut lines: Vec<String> = comment.text.split('\n').map(str::to_string).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        let cursor_y = lines.len() - 1;
        let cursor_x = char_len(&lines[cursor_y]);
        Self {
            target_lines,
            replacing_comment: Some(comment.created_at),
            lines,
            cursor_y,
            cursor_x,
        }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.newline();
            return;
        }
        if ch.is_control() && ch != '\t' {
            return;
        }
        let line = &mut self.lines[self.cursor_y];
        let byte_index = byte_index_for_char(line, self.cursor_x);
        line.insert(byte_index, ch);
        self.cursor_x += 1;
    }

    fn newline(&mut self) {
        let line = &mut self.lines[self.cursor_y];
        let byte_index = byte_index_for_char(line, self.cursor_x);
        let tail = line.split_off(byte_index);
        self.lines.insert(self.cursor_y + 1, tail);
        self.cursor_y += 1;
        self.cursor_x = 0;
    }

    fn backspace(&mut self) {
        if self.cursor_x > 0 {
            let line = &mut self.lines[self.cursor_y];
            let start = byte_index_for_char(line, self.cursor_x - 1);
            let end = byte_index_for_char(line, self.cursor_x);
            line.replace_range(start..end, "");
            self.cursor_x -= 1;
            return;
        }
        if self.cursor_y > 0 {
            let current = self.lines.remove(self.cursor_y);
            self.cursor_y -= 1;
            self.cursor_x = char_len(&self.lines[self.cursor_y]);
            self.lines[self.cursor_y].push_str(&current);
        }
    }

    fn delete(&mut self) {
        let line_len = char_len(&self.lines[self.cursor_y]);
        if self.cursor_x < line_len {
            let line = &mut self.lines[self.cursor_y];
            let start = byte_index_for_char(line, self.cursor_x);
            let end = byte_index_for_char(line, self.cursor_x + 1);
            line.replace_range(start..end, "");
            return;
        }
        if self.cursor_y + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_y + 1);
            self.lines[self.cursor_y].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor_x > 0 {
            self.cursor_x -= 1;
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.cursor_x = char_len(&self.lines[self.cursor_y]);
        }
    }

    fn move_right(&mut self) {
        if self.cursor_x < char_len(&self.lines[self.cursor_y]) {
            self.cursor_x += 1;
        } else if self.cursor_y + 1 < self.lines.len() {
            self.cursor_y += 1;
            self.cursor_x = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.cursor_x = self.cursor_x.min(char_len(&self.lines[self.cursor_y]));
        }
    }

    fn move_down(&mut self) {
        if self.cursor_y + 1 < self.lines.len() {
            self.cursor_y += 1;
            self.cursor_x = self.cursor_x.min(char_len(&self.lines[self.cursor_y]));
        }
    }

    fn home(&mut self) {
        self.cursor_x = 0;
    }

    fn end(&mut self) {
        self.cursor_x = char_len(&self.lines[self.cursor_y]);
    }

    fn kill_line(&mut self) {
        self.lines[self.cursor_y].clear();
        self.cursor_x = 0;
    }
}

pub struct App {
    config: AppConfig,
    parsed: ParsedDiff,
    rows: Vec<RenderRow>,
    comments: HashMap<String, Vec<InlineComment>>,
    selected_row: Option<usize>,
    selection_anchor: Option<usize>,
    mouse_drag_anchor: Option<usize>,
    visual_mode: bool,
    scroll: usize,
    status: String,
    error: Option<String>,
    last_diff_text: Option<String>,
    watch_started: Instant,
    editor: Option<TextEditor>,
    running: bool,
    last_body: Rect,
    comment_counter: u64,
}

impl App {
    fn new(config: AppConfig) -> Self {
        Self {
            config,
            parsed: ParsedDiff {
                lines: Vec::new(),
                hunks: Vec::new(),
            },
            rows: Vec::new(),
            comments: HashMap::new(),
            selected_row: None,
            selection_anchor: None,
            mouse_drag_anchor: None,
            visual_mode: false,
            scroll: 0,
            status: "Loading diff...".to_string(),
            error: None,
            last_diff_text: None,
            watch_started: Instant::now(),
            editor: None,
            running: true,
            last_body: Rect::default(),
            comment_counter: 0,
        }
    }

    fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        self.refresh_diff(true);
        let mut next_refresh = Instant::now() + self.config.refresh_interval;

        while self.running {
            if self.editor.is_none() && Instant::now() >= next_refresh {
                self.refresh_diff(false);
                next_refresh = Instant::now() + self.config.refresh_interval;
            }

            terminal.draw(|frame| self.render(frame))?;

            if event::poll(Duration::from_millis(100))? {
                let event = event::read()?;
                if self.editor.is_some() {
                    self.handle_editor_event(event);
                } else {
                    self.handle_normal_event(event);
                }
            }
        }

        Ok(())
    }

    fn refresh_diff(&mut self, force: bool) {
        let result = read_uncommitted_diff(&self.config.diff);
        let result_error = result.error.clone();
        if !force
            && self.last_diff_text.as_ref() == Some(&result.text)
            && self.error == result_error
        {
            return;
        }

        let previous_target = self
            .selected_row
            .and_then(|row_index| self.rows.get(row_index))
            .and_then(|row| self.row_stable_id(row));
        self.error = result_error;
        self.last_diff_text = Some(result.text.clone());
        self.parsed = parse_unified_diff(&result.text);
        self.rebuild_rows();
        self.restore_selection(previous_target.as_deref());

        if let Some(error) = &self.error {
            self.status = error.clone();
        } else if result.text.trim().is_empty() {
            self.status = "No uncommitted tracked diff. Waiting for changes...".to_string();
        } else {
            self.status = format!(
                "Loaded {} hunks, {} selectable lines.",
                self.parsed.hunks.len(),
                self.parsed.selectable_count()
            );
        }
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for (index, line) in self.parsed.lines.iter().enumerate() {
            rows.push(RenderRow::Diff(index));
            if let Some(comments) = self.comments.get(&line.stable_id()) {
                for comment in comments {
                    rows.push(RenderRow::Comment(comment.clone()));
                }
            }
        }
        self.rows = rows;
    }

    fn restore_selection(&mut self, previous_target: Option<&str>) {
        self.selection_anchor = None;
        self.mouse_drag_anchor = None;
        self.visual_mode = false;
        if let Some(previous_target) = previous_target {
            for (index, row) in self.rows.iter().enumerate() {
                if self.row_selectable(row)
                    && self.row_stable_id(row).as_deref() == Some(previous_target)
                {
                    self.selected_row = Some(index);
                    self.ensure_selection_visible();
                    return;
                }
            }
        }

        self.selected_row = self.selectable_indices().first().copied();
        self.ensure_selection_visible();
    }

    fn current_selection_lines(&self) -> Vec<DiffLine> {
        let Some(selected) = self.selected_row else {
            return Vec::new();
        };
        let anchor = self.selection_anchor.unwrap_or(selected);
        let (start, end) = normalized_range(anchor, selected);

        (start..=end)
            .filter_map(|row_index| match self.rows.get(row_index) {
                Some(RenderRow::Diff(line_index)) => self.parsed.lines.get(*line_index),
                Some(RenderRow::Comment(_)) | None => None,
            })
            .filter(|line| line.selectable)
            .cloned()
            .collect()
    }

    fn selectable_indices(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| self.row_selectable(row).then_some(index))
            .collect()
    }

    fn row_selectable(&self, row: &RenderRow) -> bool {
        match row {
            RenderRow::Diff(index) => self
                .parsed
                .lines
                .get(*index)
                .map(|line| line.selectable)
                .unwrap_or(false),
            RenderRow::Comment(_) => true,
        }
    }

    fn is_row_selected(&self, row_index: usize) -> bool {
        let Some(selected) = self.selected_row else {
            return false;
        };
        let Some(row) = self.rows.get(row_index) else {
            return false;
        };
        if !self.row_selectable(row) {
            return false;
        }
        if !self.visual_mode {
            return row_index == selected;
        }
        let anchor = self.selection_anchor.unwrap_or(selected);
        let (start, end) = normalized_range(anchor, selected);
        row_index >= start && row_index <= end
    }

    fn selected_comment_for_edit(&self) -> Option<&InlineComment> {
        if self.visual_mode {
            return None;
        }
        let selected = self.selected_row?;
        if self
            .selection_anchor
            .is_some_and(|anchor| anchor != selected)
        {
            return None;
        }
        match self.rows.get(selected)? {
            RenderRow::Comment(comment) => Some(comment),
            RenderRow::Diff(_) => None,
        }
    }

    fn row_stable_id(&self, row: &RenderRow) -> Option<String> {
        match row {
            RenderRow::Diff(index) => self.parsed.lines.get(*index).map(DiffLine::stable_id),
            RenderRow::Comment(comment) => Some(format!("comment:{}", comment.created_at)),
        }
    }

    fn row_index_for_stable_id(&self, stable_id: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| self.row_stable_id(row).as_deref() == Some(stable_id))
    }

    fn handle_normal_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if is_key_action(key) => self.handle_normal_key(key),
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_by(-3),
                MouseEventKind::ScrollDown => self.scroll_by(3),
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(row_index) = self.row_at(mouse.row)
                        && self.row_selectable(&self.rows[row_index])
                    {
                        self.selected_row = Some(row_index);
                        self.selection_anchor = Some(row_index);
                        self.mouse_drag_anchor = Some(row_index);
                        self.visual_mode = true;
                        self.ensure_selection_visible();
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if self.mouse_drag_anchor.is_some()
                        && let Some(row_index) = self.row_at(mouse.row)
                        && self.row_selectable(&self.rows[row_index])
                    {
                        self.selected_row = Some(row_index);
                        self.ensure_selection_visible();
                    }
                }
                MouseEventKind::Up(MouseButton::Left)
                    if self.mouse_drag_anchor.take().is_some() =>
                {
                    self.finish_mouse_selection();
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc if self.visual_mode => self.exit_visual_mode(),
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.running = false;
            }
            KeyCode::Char('V') if self.visual_mode => self.exit_visual_mode(),
            KeyCode::Char('V') => self.enter_visual_mode(),
            KeyCode::Char('J') => self.extend_selection(1),
            KeyCode::Char('K') => self.extend_selection(-1),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.extend_selection(1)
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.extend_selection(-1),
            KeyCode::Char('j') | KeyCode::Down if self.visual_mode => self.extend_selection(1),
            KeyCode::Char('k') | KeyCode::Up if self.visual_mode => self.extend_selection(-1),
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('f') | KeyCode::Char('F') => {
                self.scroll_by(self.last_body.height as isize)
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.scroll_by(-(self.last_body.height as isize))
            }
            KeyCode::PageDown => self.scroll_by(self.last_body.height as isize),
            KeyCode::PageUp => self.scroll_by(-(self.last_body.height as isize)),
            KeyCode::Char('g') => self.select_edge(true),
            KeyCode::Char('G') => self.select_edge(false),
            KeyCode::Char('r') | KeyCode::Char('R') => self.refresh_diff(true),
            KeyCode::Enter => self.start_comment(),
            _ => {}
        }
    }

    fn handle_editor_event(&mut self, event: Event) {
        if let Event::Key(key) = event
            && is_key_action(key)
        {
            self.handle_editor_key(key);
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };

        match key.code {
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                editor.newline();
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                editor.newline();
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.running = false;
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.submit_comment();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                editor.kill_line()
            }
            KeyCode::Esc => {
                self.editor = None;
                self.status = if self.visual_mode {
                    "Comment cancelled. Still in visual mode.".to_string()
                } else {
                    "Comment cancelled.".to_string()
                };
            }
            KeyCode::Enter => self.submit_comment(),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::Left => editor.move_left(),
            KeyCode::Right => editor.move_right(),
            KeyCode::Up => editor.move_up(),
            KeyCode::Down => editor.move_down(),
            KeyCode::Home => editor.home(),
            KeyCode::End => editor.end(),
            KeyCode::Char(ch) => editor.insert_char(ch),
            KeyCode::Tab => editor.insert_char('\t'),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        self.selection_anchor = None;
        self.mouse_drag_anchor = None;
        self.visual_mode = false;
        let selectable = self.selectable_indices();
        if selectable.is_empty() {
            self.selected_row = None;
            return;
        }

        let current_pos = self
            .selected_row
            .and_then(|selected| selectable.iter().position(|index| *index == selected))
            .unwrap_or(0);
        let next_pos = (current_pos as isize + delta).clamp(0, selectable.len() as isize - 1);
        self.selected_row = Some(selectable[next_pos as usize]);
        self.ensure_selection_visible();
    }

    fn enter_visual_mode(&mut self) {
        if self.selected_row.is_none() {
            self.selected_row = self.selectable_indices().first().copied();
        }
        let Some(selected) = self.selected_row else {
            self.status = "No selectable diff line.".to_string();
            return;
        };
        self.selection_anchor = Some(selected);
        self.mouse_drag_anchor = None;
        self.visual_mode = true;
        self.status =
            "Visual mode. Use j/k to extend, Enter to comment, Esc to cancel.".to_string();
    }

    fn exit_visual_mode(&mut self) {
        self.selection_anchor = None;
        self.mouse_drag_anchor = None;
        self.visual_mode = false;
        self.status = "Visual mode cancelled.".to_string();
    }

    fn extend_selection(&mut self, delta: isize) {
        let selectable = self.selectable_indices();
        if selectable.is_empty() {
            self.selected_row = None;
            self.selection_anchor = None;
            self.visual_mode = false;
            return;
        }

        let selected = self.selected_row.unwrap_or(selectable[0]);
        self.selection_anchor.get_or_insert(selected);
        self.visual_mode = true;
        let current_pos = selectable
            .iter()
            .position(|index| *index == selected)
            .unwrap_or(0);
        let next_pos = (current_pos as isize + delta).clamp(0, selectable.len() as isize - 1);
        self.selected_row = Some(selectable[next_pos as usize]);
        self.ensure_selection_visible();
    }

    fn select_edge(&mut self, first: bool) {
        self.selection_anchor = None;
        self.mouse_drag_anchor = None;
        self.visual_mode = false;
        let selectable = self.selectable_indices();
        if selectable.is_empty() {
            return;
        }
        self.selected_row = Some(if first {
            selectable[0]
        } else {
            *selectable.last().unwrap()
        });
        self.ensure_selection_visible();
    }

    fn finish_mouse_selection(&mut self) {
        let target_lines = self.current_selection_lines();
        if target_lines.is_empty() {
            self.selection_anchor = None;
            self.visual_mode = false;
            self.status = if self.selected_comment_for_edit().is_some() {
                "Comment selected. Press Enter to edit it.".to_string()
            } else {
                "No diff line selected.".to_string()
            };
            return;
        }

        let multi_line = target_lines.len() > 1;
        self.visual_mode = multi_line;
        self.start_comment();
        if !multi_line {
            self.selection_anchor = None;
        }
    }

    fn start_comment(&mut self) {
        if let Some(comment) = self.selected_comment_for_edit().cloned() {
            let target_lines = comment.target_lines.clone();
            let Some(target) = target_lines.first() else {
                self.status = "No diff line selected.".to_string();
                return;
            };
            self.status = format!(
                "Editing comment on {}: {}",
                target.file_path.as_deref().unwrap_or("(unknown file)"),
                describe_selection(&target_lines)
            );
            self.editor = Some(TextEditor::replacing(target_lines, &comment));
            return;
        }

        let target_lines = self.current_selection_lines();
        let Some(target) = target_lines.first() else {
            self.status = "No diff line selected.".to_string();
            return;
        };
        self.status = format!(
            "Commenting on {}: {}",
            target.file_path.as_deref().unwrap_or("(unknown file)"),
            describe_selection(&target_lines)
        );
        self.editor = Some(TextEditor::new(target_lines));
    }

    fn submit_comment(&mut self) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        let comment_text = editor.text();
        if comment_text.trim().is_empty() {
            self.status = "Empty comment cancelled.".to_string();
            return;
        }

        let target_lines = editor.target_lines;
        let Some(target) = target_lines.first() else {
            self.status = "No diff line selected.".to_string();
            return;
        };
        let payload = build_review_payload(
            &self.parsed,
            &target_lines,
            &comment_text,
            self.config.max_copy_lines,
        );
        let result = copy_to_clipboard(&payload, self.config.allow_osc52);
        let target_id = target_lines
            .last()
            .map(DiffLine::stable_id)
            .unwrap_or_else(|| target.stable_id());
        self.add_inline_comment(
            target_id.clone(),
            target_lines,
            comment_text.trim_end().to_string(),
            editor.replacing_comment,
        );
        self.rebuild_rows();
        self.restore_selection(Some(&target_id));
        self.status = result.message;
    }

    fn add_inline_comment(
        &mut self,
        target_id: String,
        target_lines: Vec<DiffLine>,
        text: String,
        replacing_comment: Option<u64>,
    ) {
        if let Some(created_at) = replacing_comment {
            self.remove_inline_comment(created_at);
        }
        self.comment_counter += 1;
        self.comments
            .entry(target_id)
            .or_default()
            .push(InlineComment {
                target_lines,
                text,
                created_at: self.comment_counter,
            });
    }

    fn remove_inline_comment(&mut self, created_at: u64) {
        self.comments.retain(|_, comments| {
            comments.retain(|comment| comment.created_at != created_at);
            !comments.is_empty()
        });
    }

    fn row_at(&self, terminal_row: u16) -> Option<usize> {
        let body = self.last_body;
        if terminal_row < body.y || terminal_row >= body.y.saturating_add(body.height) {
            return None;
        }
        let row_index = self.scroll + usize::from(terminal_row - body.y);
        (row_index < self.rows.len()).then_some(row_index)
    }

    fn scroll_by(&mut self, amount: isize) {
        if self.rows.is_empty() {
            self.scroll = 0;
            self.selected_row = None;
            self.selection_anchor = None;
            self.mouse_drag_anchor = None;
            self.visual_mode = false;
            return;
        }
        if self.mouse_drag_anchor.is_none() && !self.visual_mode {
            self.selection_anchor = None;
        }

        let height = usize::from(self.last_body.height.max(1));
        let max_scroll = self.rows.len().saturating_sub(height);
        let previous_scroll = self.scroll;
        let previous_offset = self
            .selected_row
            .filter(|selected| {
                *selected >= previous_scroll && *selected < previous_scroll.saturating_add(height)
            })
            .map(|selected| selected - previous_scroll)
            .unwrap_or(height / 2);

        self.scroll = (self.scroll as isize + amount).clamp(0, max_scroll as isize) as usize;
        if self.scroll != previous_scroll {
            self.select_visible_near(self.scroll.saturating_add(previous_offset));
        }
    }

    fn ensure_selection_visible(&mut self) {
        let Some(selected) = self.selected_row else {
            return;
        };
        let height = usize::from(self.last_body.height.max(1));
        if selected < self.scroll {
            self.scroll = selected;
        } else if selected >= self.scroll + height {
            self.scroll = selected.saturating_sub(height.saturating_sub(1));
        }
        let max_scroll = self.rows.len().saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn select_visible_near(&mut self, target_row: usize) {
        let height = usize::from(self.last_body.height.max(1));
        let start = self.scroll;
        let end = self.rows.len().min(start.saturating_add(height));

        self.selected_row = (start..end)
            .filter(|index| self.row_selectable(&self.rows[*index]))
            .min_by_key(|index| index.abs_diff(target_row));
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.height < 6 || area.width < 30 {
            frame.render_widget(
                Paragraph::new("Terminal too small.").style(Style::default().fg(Color::Red)),
                area,
            );
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        self.last_body = chunks[2];
        self.ensure_selection_visible();

        self.render_header(frame, chunks[0], chunks[1]);
        self.render_rows(frame, chunks[2]);
        self.render_footer(frame, chunks[3], chunks[4]);
    }

    fn watch_pulse_style(&self) -> Style {
        match (self.watch_started.elapsed().as_millis() / 300) % 6 {
            0 | 5 => Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            1 | 4 => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
            _ => Style::default().fg(Color::Green),
        }
    }

    fn render_header(&self, frame: &mut Frame, title_area: Rect, detail_area: Rect) {
        let mode = if self.editor.is_some() {
            "COMMENT"
        } else if self.visual_mode {
            "VISUAL"
        } else {
            "NORMAL"
        };
        let diff_label = if self.config.diff.include_untracked {
            "git diff HEAD + untracked"
        } else {
            "git diff HEAD"
        };
        let title = Line::from(vec![
            Span::styled(" diffmark ", Style::default().fg(Color::Gray)),
            Span::styled(format!("[{mode}]"), Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(
                    " | {diff_label} | {} hunks | watching ",
                    self.parsed.hunks.len()
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("● ", self.watch_pulse_style()),
        ]);
        frame.render_widget(Paragraph::new(title), title_area);

        let selected_lines = self.current_selection_lines();
        let detail = if let Some(line) = selected_lines.first() {
            format!(
                "{} | {}",
                line.file_path.as_deref().unwrap_or("(unknown file)"),
                describe_selection(&selected_lines)
            )
        } else if let Some(comment) = self.selected_comment_for_edit() {
            comment
                .target_lines
                .first()
                .map(|line| {
                    format!(
                        "{} | comment at {} | Enter to edit",
                        line.file_path.as_deref().unwrap_or("(unknown file)"),
                        describe_selection(&comment.target_lines)
                    )
                })
                .unwrap_or_else(|| "Comment selected. Press Enter to edit.".to_string())
        } else {
            "No selectable diff line.".to_string()
        };
        frame.render_widget(
            Paragraph::new(detail).style(Style::default().fg(Color::Blue)),
            detail_area,
        );
    }

    fn render_rows(&self, frame: &mut Frame, area: Rect) {
        let height = usize::from(area.height);
        let width = usize::from(area.width);
        let lines = if let Some(editor) = self.editor.as_ref() {
            self.render_rows_with_editor(height, width, editor)
        } else if self.rows.is_empty() {
            vec![Line::from(Span::styled(
                self.error
                    .clone()
                    .unwrap_or_else(|| "No uncommitted tracked diff.".to_string()),
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.render_visible_rows(height)
        };

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_visible_rows(&self, height: usize) -> Vec<Line<'static>> {
        (0..height)
            .filter_map(|offset| {
                let row_index = self.scroll + offset;
                self.render_row_line(row_index)
            })
            .collect()
    }

    fn render_rows_with_editor(
        &self,
        height: usize,
        width: usize,
        editor: &TextEditor,
    ) -> Vec<Line<'static>> {
        let cursor_visible = (self.watch_started.elapsed().as_millis() / 500).is_multiple_of(2);
        let target_row = self
            .row_index_for_stable_id(
                &editor
                    .target_lines
                    .last()
                    .map(DiffLine::stable_id)
                    .unwrap_or_default(),
            )
            .or(self.selected_row);

        let mut lines = Vec::new();
        let mut row_index = self.scroll;
        while lines.len() < height && row_index < self.rows.len() {
            if let Some(line) = self.render_row_line_with_editor(row_index, editor) {
                lines.push(line);
            }
            if Some(row_index) == target_row {
                lines.extend(self.editor_lines(editor, cursor_visible, width));
            }
            row_index += 1;
        }
        lines.truncate(height);
        lines
    }

    fn render_row_line(&self, row_index: usize) -> Option<Line<'static>> {
        let row = self.rows.get(row_index)?;
        let selected = self.is_row_selected(row_index);
        Some(Line::from(Span::styled(
            self.row_text(row),
            self.row_style(row, selected),
        )))
    }

    fn render_row_line_with_editor(
        &self,
        row_index: usize,
        editor: &TextEditor,
    ) -> Option<Line<'static>> {
        let row = self.rows.get(row_index)?;
        if self.is_comment_hidden_by_editor(row, editor) {
            return None;
        }
        self.render_row_line(row_index)
    }

    fn is_comment_hidden_by_editor(&self, row: &RenderRow, editor: &TextEditor) -> bool {
        matches!(
            (row, editor.replacing_comment),
            (RenderRow::Comment(comment), Some(created_at)) if comment.created_at == created_at
        )
    }

    fn editor_lines(
        &self,
        editor: &TextEditor,
        cursor_visible: bool,
        width: usize,
    ) -> Vec<Line<'static>> {
        let panel_style = Style::default()
            .fg(Color::Yellow)
            .bg(Color::Rgb(30, 28, 20));
        let panel_dim = panel_style.add_modifier(Modifier::DIM);
        let panel_hint = Style::default()
            .fg(Color::DarkGray)
            .bg(Color::Rgb(30, 28, 20))
            .add_modifier(Modifier::DIM);
        let mut output = vec![
            styled_full_width_line("       |", panel_dim, width),
            styled_full_width_line("       | comment", panel_hint, width),
        ];
        output.extend(editor.lines.iter().enumerate().map(|(index, line)| {
            styled_full_width_line(
                format!(
                    "       | >> {}",
                    line_with_cursor(
                        line,
                        (index == editor.cursor_y).then_some(editor.cursor_x),
                        cursor_visible,
                    )
                ),
                panel_style,
                width,
            )
        }));
        output.push(styled_full_width_line("       |", panel_dim, width));
        output
    }

    fn render_footer(&self, frame: &mut Frame, help_area: Rect, status_area: Rect) {
        let help = if self.editor.is_some() {
            " Enter submit | Ctrl-J newline | Esc cancel | arrows move | Backspace/Delete edit "
        } else {
            " j/k move | V visual | f/b page | drag select | Enter comment | q quit "
        };
        frame.render_widget(
            Paragraph::new(help).style(Style::default().fg(Color::Black).bg(Color::Gray)),
            help_area,
        );

        let style = if self.error.is_some() {
            Style::default().fg(Color::White).bg(Color::Red)
        } else {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        };
        frame.render_widget(
            Paragraph::new(format!(" {} ", self.status)).style(style),
            status_area,
        );
    }

    fn row_text(&self, row: &RenderRow) -> String {
        match row {
            RenderRow::Diff(index) => {
                let Some(line) = self.parsed.lines.get(*index) else {
                    return String::new();
                };
                let old = line
                    .old_lineno
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                let new = line
                    .new_lineno
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                format!("{old:>6} {new:>6} | {}", line.raw)
            }
            RenderRow::Comment(comment) => {
                let first_line = comment.text.lines().next().unwrap_or_default();
                let extra = comment.text.lines().count().saturating_sub(1);
                let suffix = if extra > 0 {
                    format!(" (+{extra} lines)")
                } else {
                    String::new()
                };
                format!("{:>6} {:>6} | >> {first_line}{suffix}", "", "")
            }
        }
    }

    fn row_style(&self, row: &RenderRow, selected: bool) -> Style {
        let mut style = match row {
            RenderRow::Comment(_) => Style::default().fg(Color::Yellow),
            RenderRow::Diff(index) => match self.parsed.lines.get(*index).map(|line| line.kind) {
                Some(LineKind::Add) => Style::default().fg(Color::Green),
                Some(LineKind::Delete) => Style::default().fg(Color::Red),
                Some(LineKind::Hunk) => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                Some(LineKind::File | LineKind::Meta | LineKind::Note) => {
                    Style::default().fg(Color::Blue).add_modifier(Modifier::DIM)
                }
                Some(LineKind::Context) | None => Style::default(),
            },
        };
        if selected {
            style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        style
    }
}

pub fn run_tui(config: AppConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )?;
    let mut cleanup = TerminalCleanup::active();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(config);

    let result = app.run(&mut terminal);
    drop(terminal);

    let cleanup_result = cleanup.restore();
    result?;
    cleanup_result?;
    Ok(())
}

struct TerminalCleanup {
    active: bool,
}

impl TerminalCleanup {
    fn active() -> Self {
        Self { active: true }
    }

    fn restore(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        disable_raw_mode()?;
        execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            cursor::Show
        )?;
        Ok(())
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn normalized_range(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

fn is_key_action(key: KeyEvent) -> bool {
    !matches!(key.kind, KeyEventKind::Release)
}

fn describe_selection(lines: &[DiffLine]) -> String {
    match lines {
        [] => "no lines".to_string(),
        [line] => describe_target(line),
        lines => describe_target_lines(lines),
    }
}

fn byte_index_for_char(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn char_len(value: &str) -> usize {
    value.chars().count()
}

fn styled_full_width_line(text: impl Into<String>, style: Style, width: usize) -> Line<'static> {
    Line::from(Span::styled(pad_to_width(text.into(), width), style))
}

fn pad_to_width(mut text: String, width: usize) -> String {
    let padding = width.saturating_sub(char_len(&text));
    text.extend(std::iter::repeat_n(' ', padding));
    text
}

fn line_with_cursor(line: &str, cursor_x: Option<usize>, cursor_visible: bool) -> String {
    let Some(cursor_x) = cursor_x else {
        return line.to_string();
    };
    let byte_index = byte_index_for_char(line, cursor_x);
    let cursor = if cursor_visible { "|" } else { " " };
    format!("{}{}{}", &line[..byte_index], cursor, &line[byte_index..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new(AppConfig {
            diff: DiffOptions {
                context: 3,
                include_untracked: true,
                max_untracked_bytes: 1024 * 1024,
                pathspecs: Vec::new(),
            },
            refresh_interval: Duration::from_secs(1),
            max_copy_lines: 11,
            allow_osc52: false,
        })
    }

    fn sample_diff() -> ParsedDiff {
        parse_unified_diff(concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "-old\n",
            "+new\n",
        ))
    }

    #[test]
    fn selecting_comment_row_reopens_prefilled_editor() {
        let mut app = test_app();
        app.parsed = sample_diff();
        let target = app
            .parsed
            .lines
            .iter()
            .find(|line| line.raw == "+new")
            .unwrap()
            .clone();
        app.comments
            .entry(target.stable_id())
            .or_default()
            .push(InlineComment {
                target_lines: vec![target.clone()],
                text: "keep this\nand this".to_string(),
                created_at: 1,
            });
        app.rebuild_rows();

        let comment_row = app
            .rows
            .iter()
            .position(|row| matches!(row, RenderRow::Comment(_)))
            .unwrap();
        app.selected_row = Some(comment_row);

        assert!(app.row_selectable(&app.rows[comment_row]));
        app.start_comment();

        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.target_lines, vec![target]);
        assert_eq!(editor.replacing_comment, Some(1));
        assert_eq!(editor.lines, vec!["keep this", "and this"]);
        assert_eq!(editor.cursor_y, 1);
        assert_eq!(editor.cursor_x, "and this".len());
    }

    #[test]
    fn editing_comment_hides_original_row_until_cancelled() {
        let mut app = test_app();
        app.parsed = sample_diff();
        let target = app
            .parsed
            .lines
            .iter()
            .find(|line| line.raw == "+new")
            .unwrap()
            .clone();
        let target_id = target.stable_id();
        app.comments
            .entry(target_id.clone())
            .or_default()
            .push(InlineComment {
                target_lines: vec![target],
                text: "old comment".to_string(),
                created_at: 1,
            });
        app.rebuild_rows();

        let comment_row = app
            .rows
            .iter()
            .position(|row| matches!(row, RenderRow::Comment(_)))
            .unwrap();
        app.selected_row = Some(comment_row);
        app.start_comment();

        let editor = app.editor.as_ref().unwrap();
        assert!(
            app.render_row_line_with_editor(comment_row, editor)
                .is_none()
        );
        assert_eq!(app.comments.get(&target_id).unwrap().len(), 1);

        app.editor = None;
        assert!(app.render_row_line(comment_row).is_some());
        assert_eq!(app.comments.get(&target_id).unwrap().len(), 1);
    }

    #[test]
    fn edited_comment_replaces_original_inline_row() {
        let mut app = test_app();
        app.parsed = sample_diff();
        let target = app
            .parsed
            .lines
            .iter()
            .find(|line| line.raw == "+new")
            .unwrap()
            .clone();
        let target_id = target.stable_id();
        app.comments
            .entry(target_id.clone())
            .or_default()
            .push(InlineComment {
                target_lines: vec![target.clone()],
                text: "old comment".to_string(),
                created_at: 7,
            });

        app.add_inline_comment(
            target_id.clone(),
            vec![target],
            "new comment".to_string(),
            Some(7),
        );

        let comments = app.comments.get(&target_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "new comment");
        assert_ne!(comments[0].created_at, 7);
    }

    #[test]
    fn visual_selection_ignores_comment_rows() {
        let mut app = test_app();
        app.parsed = parse_unified_diff(concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "+first\n",
            "+second\n",
        ));
        let first = app
            .parsed
            .lines
            .iter()
            .find(|line| line.raw == "+first")
            .unwrap()
            .clone();
        app.comments
            .entry(first.stable_id())
            .or_default()
            .push(InlineComment {
                target_lines: vec![first],
                text: "old comment".to_string(),
                created_at: 1,
            });
        app.rebuild_rows();

        let first_row = app
            .rows
            .iter()
            .position(|row| matches!(row, RenderRow::Diff(index) if app.parsed.lines[*index].raw == "+first"))
            .unwrap();
        let second_row = app
            .rows
            .iter()
            .position(|row| matches!(row, RenderRow::Diff(index) if app.parsed.lines[*index].raw == "+second"))
            .unwrap();
        app.selection_anchor = Some(first_row);
        app.selected_row = Some(second_row);
        app.visual_mode = true;

        let selected: Vec<_> = app
            .current_selection_lines()
            .into_iter()
            .map(|line| line.raw)
            .collect();

        assert_eq!(selected, vec!["+first", "+second"]);
        app.start_comment();
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.lines, vec![""]);
        assert_eq!(editor.target_lines.len(), 2);
    }

    #[test]
    fn key_release_events_are_ignored() {
        let mut app = test_app();
        app.parsed = sample_diff();
        app.rebuild_rows();
        app.selected_row = app.selectable_indices().first().copied();

        app.handle_normal_event(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )));
        assert!(app.editor.is_none());

        app.start_comment();
        assert!(app.editor.is_some());
        app.handle_editor_event(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )));
        assert!(app.editor.is_some());
        assert!(app.running);
    }
}
