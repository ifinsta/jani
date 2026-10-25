use crate::agent::Agent;
use crate::config::{Config, JResult};
use crate::repl::{execute_command, Flow, COMMAND_NAMES};
use crate::skills::Catalog;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::{
    cursor,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::io::{self, IsTerminal, Stdout};
use std::panic;
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const BG: Color = Color::Rgb(10, 11, 13);
const PANEL: Color = Color::Rgb(18, 20, 24);
const TEXT: Color = Color::Rgb(232, 234, 238);
const DIM: Color = Color::Rgb(128, 134, 146);
const FAINT: Color = Color::Rgb(62, 68, 78);
const ACCENT: Color = Color::Rgb(94, 208, 230);
const USER: Color = Color::Rgb(240, 184, 96);
const ERROR: Color = Color::Rgb(240, 110, 110);

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

const LOGO: [&str; 6] = [
    "     ██╗ █████╗ ███╗   ██╗██╗",
    "     ██║██╔══██╗████╗  ██║██║",
    "     ██║███████║██╔██╗ ██║██║",
    "██   ██║██╔══██║██║╚██╗██║██║",
    "╚█████╔╝██║  ██║██║ ╚████║██║",
    " ╚════╝ ╚═╝  ╚═╝╚═╝  ╚═══╝╚═╝",
];

const LOGO_COLORS: [Color; 6] = [
    Color::Rgb(156, 238, 246),
    Color::Rgb(132, 228, 241),
    Color::Rgb(110, 216, 236),
    Color::Rgb(92, 202, 228),
    Color::Rgb(78, 186, 218),
    Color::Rgb(66, 170, 204),
];

type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn run(agent: &mut Agent<'_>, cfg: &Config, catalog: &Catalog) -> JResult<()> {
    if !io::stdout().is_terminal() {
        return Err("the full-screen terminal requires stdout to be a terminal".into());
    }
    install_panic_restore();
    let meta = Meta::new(cfg, agent);
    let mut state = State::new(meta, agent.turns(), agent.pending_approvals().len());
    let _guard = TerminalGuard::enter().map_err(io_error)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout())).map_err(io_error)?;
    terminal.clear().map_err(io_error)?;
    event_loop(&mut terminal, &mut state, agent, catalog)
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
}

fn install_panic_restore() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

fn set_cursor_visible(visible: bool) {
    let mut out = io::stdout();
    let _ = if visible {
        execute!(out, cursor::Show)
    } else {
        execute!(out, cursor::Hide)
    };
}

fn event_loop(
    terminal: &mut Tui,
    state: &mut State,
    agent: &mut Agent<'_>,
    catalog: &Catalog,
) -> JResult<()> {
    loop {
        draw_screen(terminal, state)?;
        set_cursor_visible(!state.busy);
        if state.quit {
            return Ok(());
        }
        if !event::poll(Duration::from_millis(250)).map_err(io_error)? {
            continue;
        }
        let event = event::read().map_err(io_error)?;
        handle_event(terminal, state, agent, catalog, event)?;
    }
}

fn draw_screen(terminal: &mut Tui, state: &mut State) -> JResult<()> {
    terminal
        .draw(|frame| draw(frame, state))
        .map(|_| ())
        .map_err(io_error)
}

fn handle_event(
    terminal: &mut Tui,
    state: &mut State,
    agent: &mut Agent<'_>,
    catalog: &Catalog,
    event: Event,
) -> JResult<()> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if key.code == KeyCode::Enter {
                submit(terminal, state, agent, catalog)?;
            } else if is_ctrl(&key, 'c') {
                if state.editor.is_empty() {
                    state.quit = true;
                } else {
                    state.editor.clear();
                }
            } else if is_ctrl(&key, 'd') && state.editor.is_empty() {
                state.quit = true;
            } else {
                editor_key(state, key);
            }
        }
        Event::Paste(text) => state.editor.insert_str(&text),
        Event::Resize(_, _) => {}
        _ => {}
    }
    Ok(())
}

fn submit(
    terminal: &mut Tui,
    state: &mut State,
    agent: &mut Agent<'_>,
    catalog: &Catalog,
) -> JResult<()> {
    let line = state.editor.take();
    if line.is_empty() {
        return Ok(());
    }
    state.follow = true;
    if line.starts_with('/') {
        let name = command_name(&line);
        let result = execute_command(agent, catalog, &line);
        if name == "new" {
            state.entries.clear();
            state.scroll_top = 0;
        }
        state.push(Entry::new(Kind::Command, line.clone()));
        match result {
            Ok((flow, text)) => {
                if !text.is_empty() {
                    state.push(Entry::new(Kind::Note, text));
                }
                if matches!(flow, Flow::Exit) {
                    state.quit = true;
                }
            }
            Err(error) => state.push(Entry::new(Kind::Error, format!("jani: {error}"))),
        }
        state.sync(agent);
        return Ok(());
    }

    state.push(Entry::new(Kind::User, line.clone()));
    state.busy = true;
    let outcome = run_turn(terminal, state, agent, line);
    state.busy = false;
    outcome?;
    state.sync(agent);
    Ok(())
}

fn run_turn(
    terminal: &mut Tui,
    state: &mut State,
    agent: &mut Agent<'_>,
    prompt: String,
) -> JResult<()> {
    let mut outcome: Option<JResult<String>> = None;
    std::thread::scope(|scope| -> JResult<()> {
        let handle = scope.spawn(|| agent.run_turn_quiet(&prompt));
        loop {
            draw_screen(terminal, state)?;
            set_cursor_visible(false);
            state.spinner = state.spinner.wrapping_add(1);
            if handle.is_finished() {
                break;
            }
            if event::poll(Duration::from_millis(120)).map_err(io_error)? {
                let event = event::read().map_err(io_error)?;
                if let Event::Key(key) = event {
                    if key.kind == KeyEventKind::Press {
                        if is_ctrl(&key, 'c') {
                            state.quit_after_turn = true;
                        } else {
                            editor_key(state, key);
                        }
                    }
                } else if let Event::Paste(text) = event {
                    state.editor.insert_str(&text);
                }
            }
        }
        let result = handle
            .join()
            .unwrap_or_else(|_| Err("the model turn panicked".into()));
        outcome = Some(result);
        Ok(())
    })?;

    match outcome.expect("turn outcome") {
        Ok(answer) => state.push(Entry::new(Kind::Assistant, answer.trim_end())),
        Err(error) => state.push(Entry::new(Kind::Error, format!("jani: {error}"))),
    }
    if state.quit_after_turn {
        state.quit_after_turn = false;
        state.quit = true;
    }
    Ok(())
}

fn editor_key(state: &mut State, key: KeyEvent) {
    let editor = &mut state.editor;
    match key.code {
        KeyCode::Left => editor.left(),
        KeyCode::Right => editor.right(),
        KeyCode::Home => editor.home(),
        KeyCode::End => editor.end(),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete(),
        KeyCode::Up => editor.history_previous(),
        KeyCode::Down => editor.history_next(),
        KeyCode::Tab => {
            editor.complete(&COMMAND_NAMES);
        }
        KeyCode::Esc => editor.clear(),
        KeyCode::PageUp => {
            state.follow = false;
            state.scroll_top = state.scroll_top.saturating_sub(10);
        }
        KeyCode::PageDown => {
            state.scroll_top = state.scroll_top.saturating_add(10);
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => editor.home(),
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => editor.end(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => editor.clear(),
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            editor.insert_char(ch);
        }
        _ => {}
    }
}

fn is_ctrl(key: &KeyEvent, ch: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(ch)
}

fn command_name(line: &str) -> String {
    line.trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

struct Meta {
    worktree: String,
    task: String,
    mode: &'static str,
    model: String,
    profile: Option<String>,
    writes: String,
    exec: bool,
    sandbox: &'static str,
    network: &'static str,
    version: &'static str,
}

impl Meta {
    fn new(cfg: &Config, agent: &Agent<'_>) -> Self {
        let writes = if cfg.allow_write {
            format!(
                "writes {} path{}",
                cfg.write_paths.len(),
                if cfg.write_paths.len() == 1 { "" } else { "s" }
            )
        } else {
            "writes off".to_string()
        };
        Self {
            worktree: short_path(&cfg.root, 44),
            task: short_task(agent.task_id()),
            mode: match cfg.mode.as_str() {
                "enterprise" => "enterprise",
                _ => "local",
            },
            model: cfg.model.clone(),
            profile: cfg.settings_profile.clone(),
            writes,
            exec: cfg.allow_exec,
            sandbox: agent.sandbox_profile(),
            network: agent.network_policy(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

struct State {
    meta: Meta,
    entries: Vec<Entry>,
    editor: Editor,
    scroll_top: usize,
    follow: bool,
    busy: bool,
    quit_after_turn: bool,
    spinner: usize,
    pending: usize,
    seen_approvals: Vec<String>,
    turns: u64,
    quit: bool,
}

impl State {
    fn new(meta: Meta, turns: u64, pending: usize) -> Self {
        Self {
            meta,
            entries: Vec::new(),
            editor: Editor::default(),
            scroll_top: 0,
            follow: true,
            busy: false,
            quit_after_turn: false,
            spinner: 0,
            pending,
            seen_approvals: Vec::new(),
            turns,
            quit: false,
        }
    }

    fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    fn sync(&mut self, agent: &mut Agent<'_>) {
        self.turns = agent.turns();
        let pending = agent.pending_approvals();
        self.pending = pending.len();
        let mut fresh = Vec::new();
        let mut ids = Vec::new();
        for request in &pending {
            if let Some(id) = request["approvalId"].as_str() {
                ids.push(id.to_string());
                if !self.seen_approvals.iter().any(|seen| seen == id) {
                    fresh.push(request.clone());
                }
            }
        }
        self.seen_approvals = ids;
        if !fresh.is_empty() {
            self.push(Entry::new(Kind::Note, approval_summary(&fresh)));
            self.follow = true;
        }
    }
}

struct Entry {
    kind: Kind,
    text: String,
}

impl Entry {
    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    User,
    Assistant,
    Note,
    Error,
    Command,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::User => "you",
            Self::Assistant => "jani",
            Self::Note => "note",
            Self::Error => "error",
            Self::Command => "command",
        }
    }

    fn label_color(self) -> Color {
        match self {
            Self::User => USER,
            Self::Assistant => ACCENT,
            Self::Note => DIM,
            Self::Error => ERROR,
            Self::Command => FAINT,
        }
    }

    fn body_style(self) -> Style {
        match self {
            Self::User | Self::Assistant => Style::default().fg(TEXT),
            Self::Note => Style::default().fg(DIM),
            Self::Error => Style::default().fg(ERROR),
            Self::Command => Style::default().fg(DIM),
        }
    }
}

#[derive(Default)]
struct Editor {
    text: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
}

impl Editor {
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
    }

    fn insert_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn insert_str(&mut self, text: &str) {
        let clean: String = text
            .chars()
            .map(|ch| match ch {
                '\r' | '\n' | '\t' => ' ',
                other => other,
            })
            .filter(|ch| !ch.is_control())
            .collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(index, _)| index)
                .unwrap_or(0);
        }
    }

    fn right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += self.text[self.cursor..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
        }
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.len();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            let previous = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.text.replace_range(previous..self.cursor, "");
            self.cursor = previous;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
            self.text.replace_range(self.cursor..self.cursor + next, "");
        }
    }

    fn take(&mut self) -> String {
        let text = self.text.trim().to_string();
        if !text.is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.clear();
        text
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.text = self.history[index].clone();
        self.cursor = self.text.len();
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.text = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.text = std::mem::take(&mut self.draft);
        }
        self.cursor = self.text.len();
    }

    fn complete(&mut self, names: &[&str]) {
        let body = self.text.trim_start_matches('/');
        if !self.text.starts_with('/') || body.contains(char::is_whitespace) {
            return;
        }
        let prefix = body.to_ascii_lowercase();
        let matches: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| name.starts_with(&prefix))
            .collect();
        if matches.is_empty() {
            return;
        }
        let mut common = matches[0].to_string();
        for name in &matches[1..] {
            let keep = common
                .chars()
                .zip(name.chars())
                .take_while(|(a, b)| a == b)
                .count();
            common.truncate(keep);
        }
        if matches.len() == 1 {
            common.push(' ');
        }
        self.text = format!("/{common}");
        self.cursor = self.text.len();
    }

    fn visible(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        let before = &self.text[..self.cursor];
        let mut cursor_width = before.width();
        let mut start = 0;
        if cursor_width >= width {
            let mut used = 0;
            let mut start_at = self.cursor;
            for (index, ch) in before.char_indices().rev() {
                let ch_width = ch.width().unwrap_or(0);
                if used + ch_width >= width {
                    start_at = index + ch.len_utf8();
                    break;
                }
                used += ch_width;
            }
            start = start_at;
            cursor_width = self.text[start..self.cursor].width();
        }
        let mut visible = String::new();
        let mut used = 0;
        for ch in self.text[start..].chars() {
            let ch_width = ch.width().unwrap_or(0);
            if used + ch_width > width {
                break;
            }
            visible.push(ch);
            used += ch_width;
        }
        (visible, cursor_width)
    }
}

fn approval_summary(requests: &[Value]) -> String {
    let mut text = String::from("Approval required before the next protected action:");
    for request in requests {
        let id = request["approvalId"].as_str().unwrap_or("unknown");
        let capability = request["capability"].as_str().unwrap_or("unknown");
        let resource = request["resource"].as_str().unwrap_or("unknown");
        text.push_str(&format!("\n  {id}  {capability} {resource}"));
    }
    text.push_str(
        "\n\nA verified approver decides outside this session, then attach with /approval <id>.",
    );
    text
}

fn draw(frame: &mut Frame, state: &mut State) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area);
        return;
    }
    let margin = if area.width >= 96 { 3 } else { 2 };
    let content = Rect::new(
        area.x + margin,
        area.y,
        area.width.saturating_sub(margin * 2),
        area.height,
    );
    let welcome = state.entries.is_empty();
    let show_tips = area.height >= 20;
    let hero = if welcome { hero_height(area.height) } else { 2 };
    let chunks = Layout::vertical([
        Constraint::Length(hero),
        Constraint::Min(2),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(if show_tips { 1 } else { 0 }),
        Constraint::Length(1),
    ])
    .split(content);

    if welcome {
        draw_hero(frame, state, chunks[0]);
    } else {
        draw_compact_header(frame, state, chunks[0]);
    }
    draw_transcript(frame, state, chunks[1]);
    draw_input(frame, state, chunks[2]);
    draw_hints(frame, state, chunks[3]);
    if show_tips {
        draw_tip(frame, state, chunks[4]);
    }
    draw_status(
        frame,
        state,
        Rect::new(area.x, chunks[5].y, area.width, chunks[5].height),
    );

    if welcome {
        sprinkle_stars(frame, chunks[0]);
    }
}

fn hero_height(height: u16) -> u16 {
    if height >= 28 {
        10
    } else if height >= 22 {
        8
    } else {
        3
    }
}

fn draw_too_small(frame: &mut Frame, area: Rect) {
    let message = vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::default().fg(USER).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("resize to at least {MIN_WIDTH}x{MIN_HEIGHT}; ctrl+c exits"),
            Style::default().fg(DIM),
        )),
    ];
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(Style::default().bg(BG)),
        rows[1],
    );
}

fn draw_hero(frame: &mut Frame, state: &State, area: Rect) {
    if area.height == 0 {
        return;
    }
    let mut lines = Vec::new();
    if area.height >= 8 {
        lines.push(Line::from(Span::styled(
            format!("jani {}", state.meta.version),
            Style::default().fg(FAINT),
        )));
        let logo_width = LOGO.iter().map(|row| row.width()).max().unwrap_or(0);
        for (index, row) in LOGO.iter().enumerate() {
            let padded = format!("{row}{}", " ".repeat(logo_width - row.width()));
            lines.push(Line::from(Span::styled(
                padded,
                Style::default()
                    .fg(LOGO_COLORS[index])
                    .add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(Span::styled(
            "GOVERNED CODING AGENT",
            Style::default().fg(DIM),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "J A N I",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "governed coding agent",
            Style::default().fg(DIM),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("Type a message to begin.  ", Style::default().fg(DIM)),
        Span::styled("/help", Style::default().fg(ACCENT)),
        Span::styled(" lists commands.", Style::default().fg(DIM)),
    ]));
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(Style::default().bg(BG)),
        area,
    );
}

fn draw_compact_header(frame: &mut Frame, state: &State, area: Rect) {
    if area.height == 0 {
        return;
    }
    let title = Line::from(vec![
        Span::styled(
            "JANI",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  governed coding agent", Style::default().fg(DIM)),
    ]);
    frame.render_widget(
        Paragraph::new(title).style(Style::default().bg(BG)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height > 1 {
        let rule = "─".repeat(area.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(rule, Style::default().fg(FAINT))))
                .style(Style::default().bg(BG)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
    if area.width >= 64 {
        let task = format!(
            "{}  ·  {} turn{}",
            state.meta.task,
            state.turns,
            plural(state.turns as usize)
        );
        let width = (task.width() as u16).min(area.width);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(task, Style::default().fg(FAINT))))
                .alignment(Alignment::Right)
                .style(Style::default().bg(BG)),
            Rect::new(area.x + area.width - width, area.y, width, 1),
        );
    }
}

fn draw_transcript(frame: &mut Frame, state: &mut State, area: Rect) {
    if area.height == 0 {
        return;
    }
    let lines = transcript_lines(state, area.width as usize);
    let height = area.height as usize;
    let total = lines.len();
    let max_top = total.saturating_sub(height);
    if state.follow {
        state.scroll_top = max_top;
    } else {
        state.scroll_top = state.scroll_top.min(max_top);
        if state.scroll_top >= max_top {
            state.follow = true;
        }
    }
    let visible: Vec<Line<'_>> = lines
        .into_iter()
        .skip(state.scroll_top)
        .take(height)
        .collect();
    frame.render_widget(Paragraph::new(visible).style(Style::default().bg(BG)), area);
}

fn transcript_lines(state: &State, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, entry) in state.entries.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            entry.kind.label(),
            Style::default()
                .fg(entry.kind.label_color())
                .add_modifier(Modifier::BOLD),
        )));
        let body_width = width.saturating_sub(2);
        for part in wrap(&entry.text, body_width) {
            let mut spans = vec![Span::raw("  ")];
            if !part.is_empty() {
                spans.push(Span::styled(part, entry.kind.body_style()));
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn draw_input(frame: &mut Frame, state: &State, area: Rect) {
    if area.height == 0 || area.width < 8 {
        return;
    }
    let border = if state.busy { FAINT } else { ACCENT };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(PANEL));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width < 6 {
        return;
    }

    let prompt = Rect::new(inner.x + 1, inner.y, 2, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "› ",
            Style::default()
                .fg(if state.busy { DIM } else { ACCENT })
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(PANEL)),
        prompt,
    );

    let text_x = inner.x + 3;
    let text_width = inner.width.saturating_sub(4) as usize;
    if text_width > 0 {
        if state.editor.is_empty() {
            let placeholder = if text_width >= 31 {
                "Type a message… (/ for commands)"
            } else {
                "Type a message…"
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    placeholder,
                    Style::default().fg(FAINT),
                )))
                .style(Style::default().bg(PANEL)),
                Rect::new(text_x, inner.y, text_width as u16, 1),
            );
            if !state.busy {
                frame.set_cursor_position(Position::new(text_x, inner.y));
            }
        } else {
            let (visible, cursor_column) = state.editor.visible(text_width.saturating_sub(1));
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(visible, Style::default().fg(TEXT))))
                    .style(Style::default().bg(PANEL)),
                Rect::new(text_x, inner.y, text_width as u16, 1),
            );
            if !state.busy {
                let column = (cursor_column as u16).min(text_width.saturating_sub(1) as u16);
                frame.set_cursor_position(Position::new(text_x + column, inner.y));
            }
        }
    }

    if inner.height > 1 {
        let meta = meta_line(state, inner.width.saturating_sub(2) as usize);
        frame.render_widget(
            Paragraph::new(meta).style(Style::default().bg(PANEL)),
            Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1),
        );
    }
}

fn meta_line(state: &State, width: usize) -> Line<'static> {
    if state.busy {
        let spinner = SPINNER[state.spinner % SPINNER.len()];
        return Line::from(vec![
            Span::styled(
                format!("{spinner} "),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "working",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ctrl+c exits after this turn", Style::default().fg(DIM)),
        ]);
    }

    let mut spans = Vec::new();
    let mut used = 0usize;
    push_group(
        &mut spans,
        &mut used,
        vec![Span::styled(
            state.meta.mode.to_ascii_uppercase(),
            Style::default().fg(USER).add_modifier(Modifier::BOLD),
        )],
        width,
    );
    if state.pending > 0 {
        push_group(
            &mut spans,
            &mut used,
            vec![Span::styled(
                format!(
                    "{} approval{} pending",
                    state.pending,
                    plural(state.pending)
                ),
                Style::default().fg(ERROR).add_modifier(Modifier::BOLD),
            )],
            width,
        );
    }
    let mut model = vec![Span::styled(
        state.meta.model.clone(),
        Style::default().fg(DIM),
    )];
    if let Some(profile) = &state.meta.profile {
        model.push(Span::styled(
            format!(" ({profile})"),
            Style::default().fg(FAINT),
        ));
    }
    push_group(&mut spans, &mut used, model, width);
    push_group(
        &mut spans,
        &mut used,
        vec![Span::styled(
            state.meta.writes.clone(),
            Style::default().fg(DIM),
        )],
        width,
    );
    push_group(
        &mut spans,
        &mut used,
        vec![Span::styled(
            format!("sandbox {}", state.meta.sandbox),
            Style::default().fg(DIM),
        )],
        width,
    );
    push_group(
        &mut spans,
        &mut used,
        vec![Span::styled(
            format!("net {}", state.meta.network),
            Style::default().fg(DIM),
        )],
        width,
    );
    if state.meta.exec {
        push_group(
            &mut spans,
            &mut used,
            vec![Span::styled("exec on", Style::default().fg(DIM))],
            width,
        );
    }
    Line::from(spans)
}

fn push_group(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    group: Vec<Span<'static>>,
    width: usize,
) {
    let group_width: usize = group.iter().map(|span| span.content.width()).sum();
    let separator = if spans.is_empty() { 0 } else { 5 };
    if !spans.is_empty() && *used + separator + group_width > width {
        return;
    }
    if !spans.is_empty() {
        spans.push(Span::styled("  ·  ", Style::default().fg(FAINT)));
        *used += separator;
    }
    *used += group_width;
    spans.extend(group);
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn draw_hints(frame: &mut Frame, state: &State, area: Rect) {
    if area.height == 0 {
        return;
    }
    let line = if state.busy {
        Line::from(vec![
            Span::styled("working", Style::default().fg(ACCENT)),
            Span::styled(
                "  ctrl+c exits after the current turn",
                Style::default().fg(DIM),
            ),
        ])
    } else if area.width < 64 {
        let mut spans = Vec::new();
        spans.extend(key("enter"));
        spans.push(Span::styled(" send   ", Style::default().fg(DIM)));
        spans.extend(key("/"));
        spans.push(Span::styled(" commands   ", Style::default().fg(DIM)));
        spans.extend(key("ctrl+c"));
        spans.push(Span::styled(" exit", Style::default().fg(DIM)));
        Line::from(spans)
    } else {
        let mut spans = Vec::new();
        spans.extend(key("enter"));
        spans.push(Span::styled(" send   ", Style::default().fg(DIM)));
        spans.extend(key("/"));
        spans.push(Span::styled(" commands   ", Style::default().fg(DIM)));
        spans.extend(key("↑↓"));
        spans.push(Span::styled(" history   ", Style::default().fg(DIM)));
        spans.extend(key("pgup/pgdn"));
        spans.push(Span::styled(" scroll   ", Style::default().fg(DIM)));
        if !state.follow {
            spans.extend(key("end"));
            spans.push(Span::styled(" resume   ", Style::default().fg(DIM)));
        }
        spans.extend(key("ctrl+c"));
        spans.push(Span::styled(" exit", Style::default().fg(DIM)));
        Line::from(spans)
    };
    frame.render_widget(
        Paragraph::new(line)
            .alignment(Alignment::Center)
            .style(Style::default().bg(BG)),
        area,
    );
}

fn key(name: &'static str) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!(" {name} "),
        Style::default()
            .fg(TEXT)
            .bg(PANEL)
            .add_modifier(Modifier::BOLD),
    )]
}

fn draw_tip(frame: &mut Frame, state: &State, area: Rect) {
    if area.height == 0 {
        return;
    }
    let tip = tip_text(state);
    let line = Line::from(vec![
        Span::styled("● ", Style::default().fg(USER)),
        Span::styled(
            "Tip ",
            Style::default().fg(USER).add_modifier(Modifier::BOLD),
        ),
        Span::styled(tip, Style::default().fg(DIM)),
    ]);
    frame.render_widget(
        Paragraph::new(line)
            .alignment(Alignment::Center)
            .style(Style::default().bg(BG)),
        area,
    );
}

fn tip_text(state: &State) -> &'static str {
    if state.pending > 0 {
        return "Approvals are pending. Decide outside this session, then attach with /approval <id>.";
    }
    match state.turns % 4 {
        0 => "Type a message to begin. /help lists every command.",
        1 => "/status shows the authority frozen at startup.",
        2 => "Write scopes are fixed at startup; a session cannot widen them.",
        _ => "Approvals are decided outside this session and attached explicitly.",
    }
}

fn draw_status(frame: &mut Frame, state: &State, area: Rect) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(Block::default().style(Style::default().bg(PANEL)), area);
    let chunks =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", state.meta.worktree),
                Style::default().fg(DIM),
            ),
            Span::styled("·", Style::default().fg(FAINT)),
            Span::styled(format!(" {} ", state.meta.task), Style::default().fg(FAINT)),
        ]))
        .style(Style::default().bg(PANEL)),
        chunks[0],
    );
    let mut right = vec![Span::styled(
        format!(
            "v{}  ·  {} turn{} ",
            state.meta.version,
            state.turns,
            plural(state.turns as usize)
        ),
        Style::default().fg(DIM),
    )];
    if state.pending > 0 {
        right.push(Span::styled(
            format!(" ·  ! {} pending ", state.pending),
            Style::default().fg(USER).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(right))
            .alignment(Alignment::Right)
            .style(Style::default().bg(PANEL)),
        chunks[1],
    );
}

fn sprinkle_stars(frame: &mut Frame, area: Rect) {
    let buffer = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        let mut text_start = area.right();
        let mut text_end = area.left();
        for x in area.left()..area.right() {
            if buffer[(x, y)].symbol() != " " {
                text_start = text_start.min(x);
                text_end = text_end.max(x);
            }
        }
        let has_text = text_end >= text_start;
        let text_start = text_start.saturating_sub(1);
        let text_end = text_end.saturating_add(1);
        for x in area.left()..area.right() {
            if has_text && x >= text_start && x <= text_end {
                continue;
            }
            if buffer[(x, y)].symbol() != " " {
                continue;
            }
            let hash = star_hash(x, y);
            if !hash.is_multiple_of(29) {
                continue;
            }
            let symbol = match hash % 4 {
                0 => "✦",
                1 => "✧",
                2 => "·",
                _ => "⋅",
            };
            let cell = &mut buffer[(x, y)];
            cell.set_symbol(symbol);
            cell.set_fg(if hash.is_multiple_of(5) { DIM } else { FAINT });
        }
    }
}

fn star_hash(x: u16, y: u16) -> u32 {
    let mut hash = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(1_274_126_177);
    hash ^ (hash >> 16)
}

fn short_path(path: &std::path::Path, max: usize) -> String {
    let mut text = path.display().to_string();
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        text = stripped.to_string();
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        if let Some(rest) = text.strip_prefix(&home) {
            text = format!("~{rest}");
        }
    }
    if text.chars().count() <= max {
        return text;
    }
    let parts: Vec<&str> = text
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() >= 2 {
        return format!("…/{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]);
    }
    text.chars().skip(text.chars().count() - max).collect()
}

fn short_task(task: &str) -> String {
    if task.chars().count() <= 13 {
        return task.to_string();
    }
    task.chars().take(13).collect()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.replace('\r', "").split('\n') {
        let expanded = raw.replace('\t', "    ");
        if expanded.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut line_width = 0usize;
        let mut break_at: Option<usize> = None;
        for ch in expanded.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if line_width + ch_width > width && !line.is_empty() {
                if let Some(index) = break_at.take() {
                    let rest = line[index + 1..].to_string();
                    line.truncate(index);
                    out.push(line.trim_end().to_string());
                    line = rest;
                    line_width = line.width();
                    if let Some(position) = line.rfind(' ') {
                        break_at = Some(position);
                    }
                } else {
                    out.push(std::mem::take(&mut line));
                    line_width = 0;
                }
            }
            line.push(ch);
            line_width += ch_width;
            if ch == ' ' {
                break_at = Some(line.len() - 1);
            }
        }
        out.push(line.trim_end().to_string());
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn io_error(error: io::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn meta() -> Meta {
        Meta {
            worktree: "~/CascadeProjects/jani".into(),
            task: "task-1234567890ab".into(),
            mode: "local",
            model: "test-model".into(),
            profile: None,
            writes: "writes off".into(),
            exec: false,
            sandbox: "trusted-dev",
            network: "deny",
            version: "0.1.0",
        }
    }

    fn render(state: &mut State, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let mut text = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn editor_handles_unicode_and_history() {
        let mut editor = Editor::default();
        editor.insert_str("héllo");
        editor.left();
        editor.backspace();
        assert_eq!(editor.text, "hélo");
        assert_eq!(editor.take(), "hélo");
        assert_eq!(editor.take(), "");
    }

    #[test]
    fn editor_history_restores_the_draft() {
        let mut editor = Editor::default();
        editor.insert_str("first");
        assert_eq!(editor.take(), "first");
        editor.insert_str("second");
        assert_eq!(editor.take(), "second");
        editor.history_previous();
        assert_eq!(editor.text, "second");
        editor.history_previous();
        assert_eq!(editor.text, "first");
        editor.history_next();
        assert_eq!(editor.text, "second");
        editor.history_next();
        assert_eq!(editor.text, "");
    }

    #[test]
    fn editor_completes_commands() {
        let mut editor = Editor::default();
        editor.insert_str("/sta");
        editor.complete(&COMMAND_NAMES);
        assert_eq!(editor.text, "/status ");
        assert_eq!(editor.cursor, editor.text.len());
    }

    #[test]
    fn wrapping_respects_width_and_words() {
        let lines = wrap("the quick brown fox", 10);
        assert_eq!(lines, vec!["the quick", "brown fox"]);
        assert!(lines.iter().all(|line| line.width() <= 10));
    }

    #[test]
    fn wrapping_splits_long_words_and_keeps_blank_lines() {
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("one\n\ntwo", 10), vec!["one", "", "two"]);
    }

    #[test]
    fn transcript_marks_roles_and_indents_bodies() {
        let mut state = State::new(meta(), 0, 0);
        state.push(Entry::new(Kind::User, "hello there"));
        state.push(Entry::new(Kind::Assistant, "general kenobi"));
        let lines = transcript_lines(&state, 60);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(text.iter().any(|line| line == "you"));
        assert!(text.iter().any(|line| line == "jani"));
        assert!(text.iter().any(|line| line == "  hello there"));
        assert!(text.iter().any(|line| line == "  general kenobi"));
    }

    #[test]
    fn welcome_render_shows_brand_prompt_and_keys() {
        let mut state = State::new(meta(), 3, 0);
        let text = render(&mut state, 100, 32);
        assert!(text.contains("Type a message"), "{text}");
        assert!(text.contains("/help"), "{text}");
        assert!(text.contains("GOVERNED CODING AGENT"), "{text}");
        assert!(text.contains("ctrl+c"), "{text}");
        assert!(text.contains("~/CascadeProjects/jani"), "{text}");
        assert!(text.contains("v0.1.0"), "{text}");
    }

    #[test]
    fn conversation_render_shows_turns_and_mode() {
        let mut state = State::new(meta(), 3, 1);
        state.push(Entry::new(Kind::User, "write a parser"));
        state.push(Entry::new(Kind::Assistant, "Here is the parser."));
        let text = render(&mut state, 100, 30);
        assert!(text.contains("write a parser"), "{text}");
        assert!(text.contains("Here is the parser."), "{text}");
        assert!(text.contains("LOCAL"), "{text}");
        assert!(text.contains("1 approval pending"), "{text}");
        assert!(text.contains("JANI"), "{text}");
    }

    #[test]
    fn busy_render_replaces_the_meta_line() {
        let mut state = State::new(meta(), 0, 0);
        state.busy = true;
        let text = render(&mut state, 100, 30);
        assert!(text.contains("working"), "{text}");
        assert!(text.contains("ctrl+c exits after this turn"), "{text}");
    }

    #[test]
    fn tiny_terminals_get_a_resize_message() {
        let mut state = State::new(meta(), 0, 0);
        let text = render(&mut state, 30, 8);
        assert!(text.contains("Terminal too small"), "{text}");
        assert!(text.contains("40x10"), "{text}");
    }

    #[test]
    fn long_paths_and_tasks_are_shortened() {
        assert!(short_path(
            std::path::Path::new(r"\\?\C:\Users\user\CascadeProjects\jani"),
            20
        )
        .contains('/'));
        assert_eq!(short_task("task-1234567890abcdef"), "task-12345678");
    }
}
