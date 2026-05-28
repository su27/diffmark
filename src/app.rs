use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEventKind,
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
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::clipboard::copy_to_clipboard;
use crate::diff_parser::{DiffLine, LineKind, ParsedDiff, parse_unified_diff};
use crate::git_diff::read_uncommitted_diff;
use crate::payload::{build_review_payload, describe_target};

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub context: usize,
    pub refresh_interval: Duration,
    pub max_copy_lines: usize,
    pub allow_osc52: bool,
}

#[derive(Clone, Debug)]
struct InlineComment {
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
    target: DiffLine,
    lines: Vec<String>,
    cursor_y: usize,
    cursor_x: usize,
    scroll: usize,
}

impl TextEditor {
    fn new(target: DiffLine) -> Self {
        Self {
            target,
            lines: vec![String::new()],
            cursor_y: 0,
            cursor_x: 0,
            scroll: 0,
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

    fn ensure_visible(&mut self, height: usize) {
        if self.cursor_y < self.scroll {
            self.scroll = self.cursor_y;
        } else if self.cursor_y >= self.scroll + height {
            self.scroll = self.cursor_y.saturating_sub(height.saturating_sub(1));
        }
    }
}

pub struct App {
    config: AppConfig,
    parsed: ParsedDiff,
    rows: Vec<RenderRow>,
    comments: HashMap<String, Vec<InlineComment>>,
    selected_row: Option<usize>,
    scroll: usize,
    status: String,
    error: Option<String>,
    last_diff_text: Option<String>,
    last_refresh: Instant,
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
            scroll: 0,
            status: "Loading diff...".to_string(),
            error: None,
            last_diff_text: None,
            last_refresh: Instant::now(),
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
        let result = read_uncommitted_diff(self.config.context);
        let result_error = result.error.clone();
        if !force
            && self.last_diff_text.as_ref() == Some(&result.text)
            && self.error == result_error
        {
            self.last_refresh = Instant::now();
            return;
        }

        let previous_target = self.current_line().map(DiffLine::stable_id);
        self.error = result_error;
        self.last_diff_text = Some(result.text.clone());
        self.last_refresh = Instant::now();
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

    fn current_line(&self) -> Option<&DiffLine> {
        let row_index = self.selected_row?;
        match self.rows.get(row_index)? {
            RenderRow::Diff(line_index) => self.parsed.lines.get(*line_index),
            RenderRow::Comment(_) => None,
        }
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
            RenderRow::Comment(_) => false,
        }
    }

    fn row_stable_id(&self, row: &RenderRow) -> Option<String> {
        match row {
            RenderRow::Diff(index) => self.parsed.lines.get(*index).map(DiffLine::stable_id),
            RenderRow::Comment(comment) => Some(format!("comment:{}", comment.created_at)),
        }
    }

    fn handle_normal_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_normal_key(key),
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_by(-3),
                MouseEventKind::ScrollDown => self.scroll_by(3),
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(row_index) = self.row_at(mouse.row)
                        && self.row_selectable(&self.rows[row_index])
                    {
                        self.selected_row = Some(row_index);
                        self.ensure_selection_visible();
                        self.start_comment();
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.running = false;
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
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
        if let Event::Key(key) = event {
            self.handle_editor_key(key);
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };

        match key.code {
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
                self.status = "Comment cancelled.".to_string();
            }
            KeyCode::Enter => editor.newline(),
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

    fn select_edge(&mut self, first: bool) {
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

    fn start_comment(&mut self) {
        let Some(target) = self.current_line().cloned() else {
            self.status = "No diff line selected.".to_string();
            return;
        };
        self.status = format!(
            "Commenting on {}: {}",
            target.file_path.as_deref().unwrap_or("(unknown file)"),
            describe_target(&target)
        );
        self.editor = Some(TextEditor::new(target));
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

        let target = editor.target;
        let payload = build_review_payload(
            &self.parsed,
            &target,
            &comment_text,
            self.config.max_copy_lines,
        );
        let result = copy_to_clipboard(&payload, self.config.allow_osc52);
        let target_id = target.stable_id();
        self.comment_counter += 1;
        self.comments
            .entry(target_id.clone())
            .or_default()
            .push(InlineComment {
                text: comment_text.trim_end().to_string(),
                created_at: self.comment_counter,
            });
        self.rebuild_rows();
        self.restore_selection(Some(&target_id));
        self.status = result.message;
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
            return;
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
        if self.editor.is_some() {
            self.render_editor(frame, area);
        }
    }

    fn render_header(&self, frame: &mut Frame, title_area: Rect, detail_area: Rect) {
        let mode = if self.editor.is_some() {
            "COMMENT"
        } else {
            "NORMAL"
        };
        let title = format!(
            " vdiff [{mode}] | git diff HEAD | {} hunks | refresh {:.1}s ago ",
            self.parsed.hunks.len(),
            self.last_refresh.elapsed().as_secs_f32()
        );
        frame.render_widget(
            Paragraph::new(title).style(Style::default().fg(Color::Black).bg(Color::Cyan)),
            title_area,
        );

        let detail = self
            .current_line()
            .map(|line| {
                format!(
                    "{} | {}",
                    line.file_path.as_deref().unwrap_or("(unknown file)"),
                    describe_target(line)
                )
            })
            .unwrap_or_else(|| "No selectable diff line.".to_string());
        frame.render_widget(
            Paragraph::new(detail).style(Style::default().fg(Color::Blue)),
            detail_area,
        );
    }

    fn render_rows(&self, frame: &mut Frame, area: Rect) {
        let height = usize::from(area.height);
        let lines = if self.rows.is_empty() {
            vec![Line::from(Span::styled(
                self.error
                    .clone()
                    .unwrap_or_else(|| "No uncommitted tracked diff.".to_string()),
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            (0..height)
                .filter_map(|offset| {
                    let row_index = self.scroll + offset;
                    let row = self.rows.get(row_index)?;
                    let selected = self.selected_row == Some(row_index);
                    Some(Line::from(Span::styled(
                        self.row_text(row),
                        self.row_style(row, selected),
                    )))
                })
                .collect()
        };

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_footer(&self, frame: &mut Frame, help_area: Rect, status_area: Rect) {
        let help =
            " j/k move | wheel scroll | click/Enter comment | Ctrl-D submit | r refresh | q quit ";
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

    fn render_editor(&mut self, frame: &mut Frame, area: Rect) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };

        let width = area.width.saturating_sub(4).clamp(30, area.width);
        let desired_height =
            (editor.lines.len() as u16 + 6).clamp(8, area.height.saturating_sub(2));
        let left = area.x + area.width.saturating_sub(width) / 2;
        let top = area.y + area.height.saturating_sub(desired_height + 1);
        let popup = Rect::new(left, top, width, desired_height);
        let text_height = usize::from(desired_height.saturating_sub(4).max(1));
        editor.ensure_visible(text_height);

        let mut rendered_lines = vec![Line::from(
            "Enter newline | Ctrl-D submit+copy | Esc cancel",
        )];
        for offset in 0..text_height {
            let line_index = editor.scroll + offset;
            if let Some(line) = editor.lines.get(line_index) {
                rendered_lines.push(Line::from(line_with_cursor(
                    line,
                    (line_index == editor.cursor_y).then_some(editor.cursor_x),
                )));
            }
        }

        let title = format!(
            " Comment: {} | {} ",
            editor
                .target
                .file_path
                .as_deref()
                .unwrap_or("(unknown file)"),
            describe_target(&editor.target)
        );
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::White).bg(Color::Black));

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(rendered_lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            popup,
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
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(config);

    let result = app.run(&mut terminal);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        cursor::Show
    )?;
    terminal.show_cursor()?;

    result
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

fn line_with_cursor(line: &str, cursor_x: Option<usize>) -> String {
    let Some(cursor_x) = cursor_x else {
        return line.to_string();
    };
    let byte_index = byte_index_for_char(line, cursor_x);
    format!("{}|{}", &line[..byte_index], &line[byte_index..])
}
