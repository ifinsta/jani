use crate::agent::{Agent, TurnEvent};
use crate::config::{Config, JResult, ModelChoice};
use crate::repl::{execute_command, Command, Flow, COMMANDS, COMMAND_NAMES};
use crate::skills::Catalog;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::{
    cursor,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::io::{self, IsTerminal, Stdout};
use std::panic;
use std::sync::mpsc;
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
const CODE: Color = Color::Rgb(150, 214, 170);
const SELECT: Color = Color::Rgb(30, 36, 44);

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];
const WHEEL_STEP: i32 = 3;
const PAGE_STEP: i32 = 10;

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
    state.models = cfg.model_choices.clone();
    let _guard = TerminalGuard::enter().map_err(io_error)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout())).map_err(io_error)?;
    terminal.clear().map_err(io_error)?;
    event_loop(&mut terminal, &mut state, agent, catalog)
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        ) {
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
        DisableMouseCapture,
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
                if state.palette.is_some() {
                    palette_run(state, agent, catalog);
                } else {
                    submit(terminal, state, agent, catalog)?;
                }
            } else if is_ctrl(&key, 'c') {
                if state.palette.is_some() {
                    state.palette = None;
                    state.editor.clear();
                } else if state.editor.is_empty() {
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
        Event::Paste(text) => {
            state.editor.insert_str(&text);
            state.reconcile_palette(true);
        }
        Event::Mouse(mouse) => handle_mouse(state, mouse),
        Event::Resize(_, _) => {}
        _ => {}
    }
    Ok(())
}

fn handle_mouse(state: &mut State, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => state.scroll_by(-WHEEL_STEP),
        MouseEventKind::ScrollDown => state.scroll_by(WHEEL_STEP),
        MouseEventKind::Down(_) => state.click(mouse.column, mouse.row),
        _ => {}
    }
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
        state.palette = None;
        run_command(state, agent, catalog, line);
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

fn run_command(state: &mut State, agent: &mut Agent<'_>, catalog: &Catalog, line: String) {
    let name = command_name(&line);
    let result = execute_command(agent, catalog, &line);
    if name == "new" {
        state.entries.clear();
        state.scroll_top = 0;
    }
    state.push(Entry::new(Kind::Command, line));
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
}

fn palette_run(state: &mut State, agent: &mut Agent<'_>, catalog: &Catalog) {
    let Some(action) = palette_action(state) else {
        return;
    };
    state.palette = None;
    match action {
        PaletteAction::Prefill(command) => {
            state.editor.text = format!("/{} ", command.name);
            state.editor.cursor = state.editor.text.len();
        }
        PaletteAction::Run(command) => {
            state.editor.text = format!("/{}", command.name);
            state.editor.cursor = state.editor.text.len();
            let line = state.editor.take();
            state.follow = true;
            run_command(state, agent, catalog, line);
        }
        PaletteAction::Models => {
            state.editor.text = "/model ".into();
            state.editor.cursor = state.editor.text.len();
            state.palette = Some(Palette::models());
        }
        PaletteAction::Select(target) => {
            state.editor.text = format!("/model {target}");
            state.editor.cursor = state.editor.text.len();
            let line = state.editor.take();
            state.follow = true;
            run_command(state, agent, catalog, line);
        }
    }
}

fn run_turn(
    terminal: &mut Tui,
    state: &mut State,
    agent: &mut Agent<'_>,
    prompt: String,
) -> JResult<()> {
    let mut outcome: Option<JResult<String>> = None;
    let (tx, rx) = mpsc::channel::<TurnEvent>();
    state.live = Some(LiveTurn::default());
    std::thread::scope(|scope| -> JResult<()> {
        let handle = scope.spawn(|| {
            let mut send = |event: TurnEvent| {
                let _ = tx.send(event);
            };
            agent.run_turn_events(&prompt, &mut send)
        });
        loop {
            while let Ok(event) = rx.try_recv() {
                state.apply(event);
            }
            draw_screen(terminal, state)?;
            set_cursor_visible(false);
            state.spinner = state.spinner.wrapping_add(1);
            if handle.is_finished() {
                while let Ok(event) = rx.try_recv() {
                    state.apply(event);
                }
                break;
            }
            if event::poll(Duration::from_millis(120)).map_err(io_error)? {
                let event = event::read().map_err(io_error)?;
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if is_ctrl(&key, 'c') {
                            state.quit_after_turn = true;
                        } else {
                            editor_key(state, key);
                        }
                    }
                    Event::Paste(text) => state.editor.insert_str(&text),
                    Event::Mouse(mouse) => handle_mouse(state, mouse),
                    _ => {}
                }
            }
        }
        let result = handle
            .join()
            .unwrap_or_else(|_| Err("the model turn panicked".into()));
        outcome = Some(result);
        Ok(())
    })?;

    let live = state.live.take().unwrap_or_default();
    match outcome.expect("turn outcome") {
        Ok(answer) => {
            if !live.reasoning.trim().is_empty() {
                let open = live.reasoning_open;
                state.push(Entry::new(Kind::Reasoning, live.reasoning).folded(open));
            }
            state.push(Entry::new(Kind::Assistant, answer.trim_end()));
        }
        Err(error) => {
            state.push(Entry::new(Kind::Error, format!("jani: {error}")));
            state.push(Entry::new(
                Kind::Note,
                "This turn was rolled back; the model does not retain it. The next message continues from the last successful turn.",
            ));
        }
    }
    if state.quit_after_turn {
        state.quit_after_turn = false;
        state.quit = true;
    }
    Ok(())
}

fn editor_key(state: &mut State, key: KeyEvent) {
    if is_ctrl(&key, 't') {
        if let Some(target) = state.latest_fold() {
            state.toggle_fold(target);
        }
        return;
    }
    if state.palette.is_some() {
        match key.code {
            KeyCode::Up => {
                state.palette_move(-1);
                return;
            }
            KeyCode::Down => {
                state.palette_move(1);
                return;
            }
            KeyCode::Tab => {
                palette_complete(state);
                return;
            }
            KeyCode::Esc => {
                state.palette = None;
                state.editor.clear();
                return;
            }
            _ => {}
        }
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::PageUp => state.scroll_by(-PAGE_STEP),
        KeyCode::PageDown => state.scroll_by(PAGE_STEP),
        KeyCode::Home if control || state.editor.is_empty() => state.scroll_to_top(),
        KeyCode::End if control || state.editor.is_empty() => state.scroll_to_bottom(),
        _ => editor_only_key(state, key),
    }
}

fn palette_complete(state: &mut State) {
    let Some(palette) = state.palette.as_ref() else {
        return;
    };
    match palette.mode {
        PaletteMode::Commands => {
            let Some(command) = palette.command_choice(&state.editor) else {
                return;
            };
            state.palette = None;
            state.editor.text = format!("/{}", command.name);
            if command.requires_argument {
                state.editor.text.push(' ');
            }
            state.editor.cursor = state.editor.text.len();
        }
        PaletteMode::Models => {
            let rows = palette.matches(&state.editor, &state.models);
            let target = match rows.get(palette.selected.min(rows.len().saturating_sub(1))) {
                Some(PaletteRow::Profile(choice)) => choice.label.clone(),
                Some(PaletteRow::Custom(model)) => model.clone(),
                _ => return,
            };
            state.palette = None;
            state.editor.text = format!("/model {target} ");
            state.editor.cursor = state.editor.text.len();
        }
    }
}

fn editor_only_key(state: &mut State, key: KeyEvent) {
    let before = state.editor.text.clone();
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
    let changed = state.editor.text != before;
    state.reconcile_palette(changed);
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
            model: agent.model().to_string(),
            profile: agent.model_profile().map(str::to_string),
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
    palette: Option<Palette>,
    models: Vec<ModelChoice>,
    live: Option<LiveTurn>,
    scroll_top: usize,
    viewport: usize,
    line_count: usize,
    folds: Vec<Option<Fold>>,
    transcript_area: Rect,
    scrollbar_col: Option<u16>,
    follow: bool,
    busy: bool,
    quit_after_turn: bool,
    spinner: usize,
    pending: usize,
    seen_approvals: Vec<String>,
    turns: u64,
    quit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fold {
    Entry(usize),
    Live,
}

#[derive(Default)]
struct LiveTurn {
    activity: String,
    answer: String,
    reasoning: String,
    reasoning_open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum PaletteMode {
    #[default]
    Commands,
    Models,
}

#[derive(Default)]
struct Palette {
    selected: usize,
    mode: PaletteMode,
}

enum PaletteRow<'a> {
    Command(&'static Command),
    Profile(&'a ModelChoice),
    Custom(String),
}

impl Palette {
    fn models() -> Self {
        Self {
            selected: 0,
            mode: PaletteMode::Models,
        }
    }

    fn query<'a>(&self, editor: &'a Editor) -> Option<&'a str> {
        match self.mode {
            PaletteMode::Commands => {
                let body = editor.text.strip_prefix('/')?;
                if body.contains(char::is_whitespace) {
                    return None;
                }
                Some(body)
            }
            PaletteMode::Models => {
                let body = editor.text.strip_prefix("/model ")?;
                if body.contains(char::is_whitespace) {
                    return None;
                }
                Some(body)
            }
        }
    }

    fn command_matches(&self, editor: &Editor) -> Vec<&'static Command> {
        let Some(query) = self.query(editor) else {
            return Vec::new();
        };
        let query = query.to_ascii_lowercase();
        COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(&query))
            .collect()
    }

    fn command_choice(&self, editor: &Editor) -> Option<&'static Command> {
        let matches = self.command_matches(editor);
        matches
            .get(self.selected.min(matches.len().saturating_sub(1)))
            .copied()
    }

    fn matches<'a>(&self, editor: &Editor, models: &'a [ModelChoice]) -> Vec<PaletteRow<'a>> {
        let Some(query) = self.query(editor) else {
            return Vec::new();
        };
        match self.mode {
            PaletteMode::Commands => self
                .command_matches(editor)
                .into_iter()
                .map(PaletteRow::Command)
                .collect(),
            PaletteMode::Models => {
                let folded = query.to_ascii_lowercase();
                let mut rows: Vec<PaletteRow<'a>> = models
                    .iter()
                    .filter(|choice| {
                        choice.label.to_ascii_lowercase().starts_with(&folded)
                            || choice.model.to_ascii_lowercase().starts_with(&folded)
                    })
                    .map(PaletteRow::Profile)
                    .collect();
                if !query.is_empty() && rows.is_empty() {
                    rows.push(PaletteRow::Custom(query.to_string()));
                }
                rows
            }
        }
    }

    fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            return;
        }
        let selected = self.selected.min(len - 1) as isize + delta;
        self.selected = selected.rem_euclid(len as isize) as usize;
    }
}

enum PaletteAction {
    Run(&'static Command),
    Prefill(&'static Command),
    Models,
    Select(String),
}

fn palette_action(state: &State) -> Option<PaletteAction> {
    let palette = state.palette.as_ref()?;
    match palette.mode {
        PaletteMode::Commands => {
            let command = palette.command_choice(&state.editor)?;
            Some(if command.name == "model" {
                PaletteAction::Models
            } else if command.requires_argument {
                PaletteAction::Prefill(command)
            } else {
                PaletteAction::Run(command)
            })
        }
        PaletteMode::Models => {
            let rows = palette.matches(&state.editor, &state.models);
            match rows.get(palette.selected.min(rows.len().saturating_sub(1)))? {
                PaletteRow::Profile(choice) => Some(PaletteAction::Select(choice.label.clone())),
                PaletteRow::Custom(model) => Some(PaletteAction::Select(model.clone())),
                PaletteRow::Command(_) => None,
            }
        }
    }
}

impl State {
    fn new(meta: Meta, turns: u64, pending: usize) -> Self {
        Self {
            meta,
            entries: Vec::new(),
            editor: Editor::default(),
            palette: None,
            models: Vec::new(),
            live: None,
            scroll_top: 0,
            viewport: 0,
            line_count: 0,
            folds: Vec::new(),
            transcript_area: Rect::default(),
            scrollbar_col: None,
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

    fn palette_move(&mut self, delta: isize) {
        let len = self
            .palette
            .as_ref()
            .map(|palette| palette.matches(&self.editor, &self.models).len())
            .unwrap_or(0);
        if let Some(palette) = self.palette.as_mut() {
            palette.move_selection(delta, len);
        }
    }

    fn palette_candidate(&self) -> Option<Palette> {
        if self.busy {
            return None;
        }
        if self.editor.text == "/" {
            return Some(Palette::default());
        }
        let rest = self.editor.text.strip_prefix("/model ")?;
        if rest.contains(char::is_whitespace) {
            return None;
        }
        Some(Palette::models())
    }

    fn reconcile_palette(&mut self, text_changed: bool) {
        let Some(palette) = self.palette.as_ref() else {
            self.palette = self.palette_candidate();
            return;
        };
        if palette.query(&self.editor).is_none() {
            self.palette = self.palette_candidate();
        } else if text_changed {
            if let Some(palette) = self.palette.as_mut() {
                palette.selected = 0;
            }
        }
    }

    fn max_top(&self) -> usize {
        self.line_count.saturating_sub(self.viewport)
    }

    fn lines_below(&self) -> usize {
        if self.follow {
            0
        } else {
            self.max_top().saturating_sub(self.scroll_top)
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let max_top = self.max_top();
        let moved = self.scroll_top as i64 + i64::from(delta);
        self.scroll_top = moved.clamp(0, max_top as i64) as usize;
        self.follow = self.scroll_top >= max_top;
    }

    fn scroll_to_top(&mut self) {
        self.scroll_top = 0;
        self.follow = self.max_top() == 0;
    }

    fn scroll_to_bottom(&mut self) {
        self.follow = true;
    }

    fn latest_fold(&self) -> Option<Fold> {
        if self
            .live
            .as_ref()
            .is_some_and(|live| !live.reasoning.trim().is_empty())
        {
            return Some(Fold::Live);
        }
        self.entries
            .iter()
            .rposition(|entry| entry.kind == Kind::Reasoning)
            .map(Fold::Entry)
    }

    fn toggle_fold(&mut self, target: Fold) {
        match target {
            Fold::Entry(index) => {
                if let Some(entry) = self.entries.get_mut(index) {
                    entry.open = !entry.open;
                }
            }
            Fold::Live => {
                if let Some(live) = self.live.as_mut() {
                    live.reasoning_open = !live.reasoning_open;
                }
            }
        }
    }

    fn click(&mut self, column: u16, row: u16) {
        let area = self.transcript_area;
        if row < area.y || row >= area.bottom() {
            return;
        }
        if column == self.scrollbar_col.unwrap_or(u16::MAX) {
            let height = usize::from(area.height.max(1));
            let offset = (row - area.y) as usize;
            let max_top = self.max_top();
            self.scroll_top = max_top.min(offset * max_top / height.max(1));
            self.follow = self.scroll_top >= max_top;
            return;
        }
        if column < area.x || column >= area.right() {
            return;
        }
        let line = self.scroll_top + (row - area.y) as usize;
        if let Some(Some(target)) = self.folds.get(line).copied() {
            self.toggle_fold(target);
        }
    }

    fn apply(&mut self, event: TurnEvent) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match event {
            TurnEvent::Step { step, max_steps } => {
                live.activity = format!("step {step}/{max_steps}");
            }
            TurnEvent::ToolStarted { activity, .. } => live.activity = activity,
            TurnEvent::ToolFinished { activity, ok, .. } => {
                live.activity = "thinking".into();
                let text = if ok {
                    activity
                } else {
                    format!("{activity} (failed)")
                };
                self.entries.push(Entry::new(Kind::Tool, text));
            }
            TurnEvent::Text(chunk) => {
                live.activity = "writing answer".into();
                live.answer.push_str(&chunk);
            }
            TurnEvent::Reasoning(chunk) => {
                if live.activity != "writing answer" {
                    live.activity = "thinking".into();
                }
                live.reasoning.push_str(&chunk);
            }
        }
    }

    fn sync(&mut self, agent: &mut Agent<'_>) {
        self.meta.model = agent.model().to_string();
        self.meta.profile = agent.model_profile().map(str::to_string);
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
    open: bool,
}

impl Entry {
    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            open: false,
        }
    }

    fn folded(mut self, open: bool) -> Self {
        self.open = open;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Assistant,
    Note,
    Error,
    Command,
    Tool,
    Reasoning,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::User => "you",
            Self::Assistant => "jani",
            Self::Note => "note",
            Self::Error => "error",
            Self::Command => "command",
            Self::Tool => "tool",
            Self::Reasoning => "think",
        }
    }

    fn label_color(self) -> Color {
        match self {
            Self::User => USER,
            Self::Assistant => ACCENT,
            Self::Note => DIM,
            Self::Error => ERROR,
            Self::Command => FAINT,
            Self::Tool => CODE,
            Self::Reasoning => FAINT,
        }
    }

    fn body_style(self) -> Style {
        match self {
            Self::User | Self::Assistant => Style::default().fg(TEXT),
            Self::Note | Self::Tool | Self::Reasoning => Style::default().fg(DIM),
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
    draw_palette(frame, state, content, chunks[2]);
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
    let height = area.height as usize;
    let mut view = transcript(state, area.width as usize);
    let mut scrollbar_col = None;
    if view.lines.len() > height && area.width > MIN_WIDTH {
        view = transcript(state, area.width.saturating_sub(2) as usize);
        scrollbar_col = Some(area.x + area.width - 1);
    }
    let total = view.lines.len();
    let max_top = total.saturating_sub(height);
    if state.follow {
        state.scroll_top = max_top;
    } else {
        state.scroll_top = state.scroll_top.min(max_top);
        if state.scroll_top >= max_top {
            state.follow = true;
        }
    }
    state.line_count = total;
    state.viewport = height;
    state.folds = view.folds;
    state.transcript_area = area;
    state.scrollbar_col = scrollbar_col;

    let body = Rect::new(
        area.x,
        area.y,
        area.width - u16::from(scrollbar_col.is_some()),
        area.height,
    );
    let visible: Vec<Line<'_>> = view
        .lines
        .into_iter()
        .skip(state.scroll_top)
        .take(height)
        .collect();
    frame.render_widget(Paragraph::new(visible).style(Style::default().bg(BG)), body);
    if scrollbar_col.is_some() {
        let mut position = ScrollbarState::new(total)
            .position(state.scroll_top)
            .viewport_content_length(height);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(DIM))
                .track_style(Style::default().fg(FAINT))
                .begin_symbol(None)
                .end_symbol(None),
            area,
            &mut position,
        );
    }
}

struct Transcript {
    lines: Vec<Line<'static>>,
    folds: Vec<Option<Fold>>,
}

impl Transcript {
    fn push(&mut self, line: Line<'static>, fold: Option<Fold>) {
        self.lines.push(line);
        self.folds.push(fold);
    }

    fn extend(&mut self, block: Vec<Line<'static>>, fold: Option<Fold>) {
        for line in block {
            self.push(line, fold);
        }
    }

    fn blank(&mut self) {
        self.push(Line::default(), None);
    }
}

fn transcript(state: &State, width: usize) -> Transcript {
    let mut out = Transcript {
        lines: Vec::new(),
        folds: Vec::new(),
    };
    let body_width = width.saturating_sub(2);
    for (index, entry) in state.entries.iter().enumerate() {
        if !out.lines.is_empty() {
            out.blank();
        }
        match entry.kind {
            Kind::Reasoning => {
                let fold = Some(Fold::Entry(index));
                out.push(fold_label(entry.open), fold);
                out.extend(reasoning_block(&entry.text, body_width, entry.open), fold);
            }
            Kind::Assistant => {
                out.push(kind_label(entry.kind), None);
                out.extend(markdown_block(&entry.text, body_width), None);
            }
            other => {
                out.push(kind_label(other), None);
                for part in wrap(&entry.text, body_width) {
                    let mut spans = vec![Span::raw("  ")];
                    if !part.is_empty() {
                        spans.push(Span::styled(part, other.body_style()));
                    }
                    out.push(Line::from(spans), None);
                }
            }
        }
    }
    if let Some(live) = state.live.as_ref() {
        let has_reasoning = !live.reasoning.trim().is_empty();
        let has_answer = !live.answer.trim().is_empty();
        if has_reasoning || has_answer {
            if !out.lines.is_empty() {
                out.blank();
            }
            if has_reasoning {
                let fold = Some(Fold::Live);
                out.push(fold_label(live.reasoning_open), fold);
                out.extend(
                    reasoning_block(&live.reasoning, body_width, live.reasoning_open),
                    fold,
                );
            }
            if has_answer {
                if has_reasoning {
                    out.blank();
                }
                out.push(kind_label(Kind::Assistant), None);
                out.extend(markdown_block(&live.answer, body_width), None);
            }
        }
    }
    out
}

fn kind_label(kind: Kind) -> Line<'static> {
    Line::from(Span::styled(
        kind.label(),
        Style::default()
            .fg(kind.label_color())
            .add_modifier(Modifier::BOLD),
    ))
}

fn fold_label(open: bool) -> Line<'static> {
    let marker = if open { "▾" } else { "▸" };
    Line::from(Span::styled(
        format!("{marker} {}", Kind::Reasoning.label()),
        Style::default()
            .fg(Kind::Reasoning.label_color())
            .add_modifier(Modifier::BOLD),
    ))
}

fn markdown_block(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for line in markdown_lines(text, width) {
        if line.spans.is_empty() {
            lines.push(Line::default());
        } else {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn reasoning_block(text: &str, width: usize, open: bool) -> Vec<Line<'static>> {
    let text = text.trim_end();
    if open {
        let mut lines = Vec::new();
        for part in wrap(text, width) {
            let mut spans = vec![Span::raw("  ")];
            if !part.is_empty() {
                spans.push(Span::styled(
                    part,
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                ));
            }
            lines.push(Line::from(spans));
        }
        return lines;
    }
    let count = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        .max(1);
    vec![Line::from(Span::styled(
        format!(
            "  {count} reasoning line{} · click or ctrl+t",
            plural(count)
        ),
        Style::default().fg(FAINT),
    ))]
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

fn draw_palette(frame: &mut Frame, state: &State, content: Rect, input: Rect) {
    let Some(palette) = state.palette.as_ref() else {
        return;
    };
    let Some(query) = palette.query(&state.editor) else {
        return;
    };
    let available = input.y.saturating_sub(content.y);
    let width = content.width.min(80);
    if available < 4 || width < 20 {
        return;
    }
    let rows = palette.matches(&state.editor, &state.models);
    let max_rows = usize::from(available.saturating_sub(3)).max(1);
    let visible = rows.len().clamp(1, max_rows);
    let height = (visible as u16 + 3).min(available);
    let area = Rect::new(
        content.x + (content.width - width) / 2,
        input.y - height,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let title = match (palette.mode, query.is_empty()) {
        (PaletteMode::Commands, true) => " commands ".to_string(),
        (PaletteMode::Commands, false) => format!(" commands · {query} "),
        (PaletteMode::Models, true) => " model ".to_string(),
        (PaletteMode::Models, false) => format!(" model · {query} "),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(title, Style::default().fg(DIM)))
        .style(Style::default().bg(PANEL));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 || inner.width < 6 {
        return;
    }
    let list_height = usize::from(inner.height) - 1;
    let selected = palette.selected.min(rows.len().saturating_sub(1));
    let first = if rows.len() > list_height {
        selected
            .saturating_sub(list_height / 2)
            .min(rows.len() - list_height)
    } else {
        0
    };
    let name_width = rows
        .iter()
        .map(|row| match row {
            PaletteRow::Command(command) => command_label(command).width(),
            PaletteRow::Profile(choice) => choice.label.width(),
            PaletteRow::Custom(model) => model.width(),
        })
        .max()
        .unwrap_or(0);
    let description_width = usize::from(inner.width).saturating_sub(name_width + 4);
    let mut lines = Vec::new();
    if rows.is_empty() {
        let message = match palette.mode {
            PaletteMode::Commands => format!("  no command matches \"/{query}\""),
            PaletteMode::Models => "  no model profiles in settings; type a model ID".to_string(),
        };
        lines.push(Line::from(Span::styled(message, Style::default().fg(DIM))));
    } else {
        for (offset, row) in rows[first..first + visible].iter().enumerate() {
            let active = first + offset == selected;
            let marker = if active { "› " } else { "  " };
            let (name, summary, current, unavailable) = match row {
                PaletteRow::Command(command) => (
                    command_label(command),
                    command.summary.to_string(),
                    false,
                    false,
                ),
                PaletteRow::Profile(choice) => (
                    choice.label.clone(),
                    choice.model.clone(),
                    state.meta.profile.as_deref() == Some(choice.label.as_str()),
                    choice.api_key().is_none(),
                ),
                PaletteRow::Custom(model) => (
                    model.clone(),
                    "use as a model ID with the current API and key".to_string(),
                    false,
                    false,
                ),
            };
            let name = if name.width() > name_width {
                clip(&name, name_width)
            } else {
                name
            };
            let mut line = Line::from(vec![
                Span::styled(
                    marker,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{name:<name_width$}"),
                    Style::default().fg(if active { ACCENT } else { TEXT }),
                ),
                Span::raw("  "),
                Span::styled(clip(&summary, description_width), Style::default().fg(DIM)),
            ]);
            if current {
                line.spans
                    .push(Span::styled(" · current", Style::default().fg(CODE)));
            }
            if unavailable {
                line.spans
                    .push(Span::styled(" · no credential", Style::default().fg(ERROR)));
            }
            if active {
                line = line.style(Style::default().bg(SELECT));
            }
            lines.push(line);
        }
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(PANEL)),
        Rect::new(inner.x, inner.y, inner.width, list_height as u16),
    );
    let footer = match palette.mode {
        PaletteMode::Commands => "enter run · tab complete · esc close",
        PaletteMode::Models => "enter switch · tab complete · esc close",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            clip(footer, usize::from(inner.width)),
            Style::default().fg(FAINT),
        )))
        .style(Style::default().bg(PANEL)),
        Rect::new(inner.x, inner.y + list_height as u16, inner.width, 1),
    );
}

fn command_label(command: &Command) -> String {
    if command.usage.is_empty() {
        format!("/{}", command.name)
    } else {
        format!("/{} {}", command.name, command.usage)
    }
}

fn clip(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".into();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > width - 1 {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

fn meta_line(state: &State, width: usize) -> Line<'static> {
    if state.busy {
        let spinner = SPINNER[state.spinner % SPINNER.len()];
        let activity = state
            .live
            .as_ref()
            .map(|live| live.activity.as_str())
            .filter(|activity| !activity.is_empty())
            .unwrap_or("working");
        let below = state.lines_below();
        let suffix = if state.follow || below == 0 {
            "  ctrl+c exits after this turn".to_string()
        } else {
            format!(
                "  {below} line{} below · end resumes  ctrl+c exits after this turn",
                plural(below)
            )
        };
        return Line::from(vec![
            Span::styled(
                format!("{spinner} "),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                activity.to_string(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(suffix, Style::default().fg(DIM)),
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
    let line = if let Some(palette) = state.palette.as_ref() {
        let action = match palette.mode {
            PaletteMode::Commands => " run  ",
            PaletteMode::Models => " switch  ",
        };
        hints_line(&[
            ("↑↓", " select  "),
            ("enter", action),
            ("tab", " complete  "),
            ("esc", " close"),
        ])
    } else if state.busy {
        hints_line(&[
            ("pgup/wheel", " scroll  "),
            ("ctrl+t", " thinking  "),
            ("ctrl+c", " exits after the current turn"),
        ])
    } else if area.width < 64 {
        hints_line(&[
            ("enter", " send  "),
            ("/", " commands  "),
            ("ctrl+c", " exit"),
        ])
    } else if area.width < 96 {
        hints_line(&[
            ("enter", " send  "),
            ("pgup/wheel", " scroll  "),
            ("ctrl+t", " thinking  "),
            ("ctrl+c", " exit"),
        ])
    } else {
        let jump = if state.follow {
            ("home/end", " jump  ")
        } else {
            ("end", " resume  ")
        };
        hints_line(&[
            ("enter", " send  "),
            ("/", " commands  "),
            ("pgup/wheel", " scroll  "),
            jump,
            ("ctrl+t", " thinking  "),
            ("ctrl+c", " exit"),
        ])
    };
    frame.render_widget(
        Paragraph::new(line)
            .alignment(Alignment::Center)
            .style(Style::default().bg(BG)),
        area,
    );
}

fn hints_line(groups: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, (name, label)) in groups.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.extend(key(name));
        let text = if index + 1 == groups.len() {
            label.trim_end()
        } else {
            label
        };
        spans.push(Span::styled(text.to_string(), Style::default().fg(DIM)));
    }
    Line::from(spans)
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

fn tip_text(state: &State) -> String {
    if !state.follow && state.lines_below() > 0 {
        let below = state.lines_below();
        return format!(
            "{below} line{} below the bottom · end or wheel down resumes following",
            plural(below)
        );
    }
    if state.pending > 0 {
        return "Approvals are pending. Decide outside this session, then attach with /approval <id>."
            .into();
    }
    match state.turns % 4 {
        0 => "Type a message to begin. /help lists every command.",
        1 => "/status shows the authority frozen at startup.",
        2 => "Write scopes are fixed at startup; a session cannot widen them.",
        _ => "Approvals are decided outside this session and attached explicitly.",
    }
    .into()
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

fn markdown_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let base = Style::default().fg(TEXT);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let source: Vec<&str> = normalized.split('\n').collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let line = source[index];
        if line.trim().is_empty() {
            out.push(Line::default());
            index += 1;
            continue;
        }
        if let Some((fence, indent)) = fence_open(line) {
            index += 1;
            let mut code = Vec::new();
            while index < source.len() && !fence_close(source[index], fence) {
                code.push(source[index]);
                index += 1;
            }
            if index < source.len() {
                index += 1;
            }
            push_code(&mut out, &code, width, indent);
            continue;
        }
        if is_rule(line) {
            out.push(Line::from(Span::styled(
                "─".repeat(width.min(32)),
                Style::default().fg(FAINT),
            )));
            index += 1;
            continue;
        }
        if let Some((level, content)) = heading(line) {
            let style = if level <= 2 {
                base.fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                base.add_modifier(Modifier::BOLD)
            };
            push_inline(&mut out, content, width, style);
            index += 1;
            continue;
        }
        if let Some(content) = blockquote(line) {
            push_prefixed(
                &mut out,
                content,
                width,
                ("│ ", Style::default().fg(FAINT)),
                ("│ ", Style::default().fg(FAINT)),
                base.fg(DIM),
            );
            index += 1;
            continue;
        }
        if let Some((prefix, content)) = list_item(line) {
            let continuation = " ".repeat(prefix.width());
            push_prefixed(
                &mut out,
                content,
                width,
                (&prefix, base.fg(ACCENT)),
                (&continuation, Style::default()),
                base,
            );
            index += 1;
            continue;
        }
        if is_table_header(&source, index) {
            index = push_table(&mut out, &source, index, width, base);
            continue;
        }
        let mut parts = vec![line.trim()];
        index += 1;
        while index < source.len() && !starts_block(&source, index) {
            parts.push(source[index].trim());
            index += 1;
        }
        push_inline(&mut out, &parts.join(" "), width, base);
    }
    if out.is_empty() {
        out.push(Line::default());
    }
    out
}

fn starts_block(lines: &[&str], index: usize) -> bool {
    let line = lines[index];
    line.trim().is_empty()
        || fence_open(line).is_some()
        || heading(line).is_some()
        || blockquote(line).is_some()
        || list_item(line).is_some()
        || is_rule(line)
        || is_table_header(lines, index)
}

fn fence_open(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let fence = trimmed.chars().next()?;
    if fence != '`' && fence != '~' {
        return None;
    }
    if trimmed.chars().take_while(|ch| *ch == fence).count() < 3 {
        return None;
    }
    let indent = line.len() - trimmed.len();
    Some((fence, indent.min(8)))
}

fn fence_close(line: &str, fence: char) -> bool {
    let trimmed = line.trim();
    trimmed.chars().take_while(|ch| *ch == fence).count() >= 3
        && trimmed.chars().all(|ch| ch == fence)
}

fn is_rule(line: &str) -> bool {
    let markers: Vec<char> = line
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    markers.len() >= 3
        && matches!(markers[0], '-' | '*' | '_')
        && markers.iter().all(|ch| *ch == markers[0])
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim().trim_end_matches('#').trim_end()))
}

fn blockquote(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn list_item(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    let indent = (line.len() - trimmed.len()).min(8);
    let pad = " ".repeat(indent);
    for marker in ["- ", "* ", "+ "] {
        if let Some(content) = trimmed.strip_prefix(marker) {
            return Some((format!("{pad}• "), content.trim_start()));
        }
    }
    let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &trimmed[digits..];
    if let Some(content) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
        return Some((
            format!("{pad}{}. ", &trimmed[..digits]),
            content.trim_start(),
        ));
    }
    None
}

fn is_table_header(lines: &[&str], index: usize) -> bool {
    index + 1 < lines.len() && lines[index].contains('|') && is_table_separator(lines[index + 1])
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return false;
    }
    let mut dash = false;
    for ch in trimmed.chars() {
        match ch {
            '|' | ':' | ' ' | '\t' => {}
            '-' => dash = true,
            _ => return false,
        }
    }
    dash
}

fn push_table(
    out: &mut Vec<Line<'static>>,
    lines: &[&str],
    index: usize,
    width: usize,
    base: Style,
) -> usize {
    let header = table_cells(lines[index]);
    let header_style = base.add_modifier(Modifier::BOLD);
    push_inline(out, &header.join("  │  "), width, header_style);
    let mut next = index + 2;
    while next < lines.len() && !lines[next].trim().is_empty() && lines[next].contains('|') {
        push_inline(out, &table_cells(lines[next]).join("  │  "), width, base);
        next += 1;
    }
    next
}

fn table_cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn push_inline(out: &mut Vec<Line<'static>>, text: &str, width: usize, base: Style) {
    let spans = inline_spans(text, base);
    for segment in wrap_spans(&spans, width) {
        if segment.is_empty() {
            out.push(Line::default());
            continue;
        }
        out.push(Line::from(
            segment
                .into_iter()
                .map(|(text, style)| Span::styled(text, style))
                .collect::<Vec<_>>(),
        ));
    }
}

fn push_prefixed(
    out: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    first: (&str, Style),
    next: (&str, Style),
    base: Style,
) {
    let available = width
        .saturating_sub(first.0.width().max(next.0.width()))
        .max(1);
    let spans = inline_spans(text, base);
    let mut wrapped = wrap_spans(&spans, available);
    if wrapped.is_empty() {
        wrapped.push(Vec::new());
    }
    for (index, segment) in wrapped.into_iter().enumerate() {
        let (prefix, style) = if index == 0 { first } else { next };
        let mut spans = vec![Span::styled(prefix.to_string(), style)];
        spans.extend(
            segment
                .into_iter()
                .map(|(text, style)| Span::styled(text, style)),
        );
        out.push(Line::from(spans));
    }
}

fn push_code(out: &mut Vec<Line<'static>>, code: &[&str], width: usize, indent: usize) {
    let indent = indent.min(width.saturating_sub(2) / 2);
    let gutter = format!("{}│ ", " ".repeat(indent));
    let available = width.saturating_sub(gutter.width()).max(1);
    for raw in code {
        let expanded = raw.replace('\t', "    ");
        if expanded.trim().is_empty() {
            out.push(Line::default());
            continue;
        }
        for segment in split_width(&expanded, available) {
            out.push(Line::from(vec![
                Span::styled(gutter.clone(), Style::default().fg(FAINT)),
                Span::styled(segment, Style::default().fg(CODE)),
            ]));
        }
    }
}

fn split_width(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut line_width = 0usize;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if line_width + ch_width > width && !line.is_empty() {
            out.push(std::mem::take(&mut line));
            line_width = 0;
        }
        line.push(ch);
        line_width += ch_width;
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

fn inline_spans(text: &str, base: Style) -> Vec<(String, Style)> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' && index + 1 < chars.len() {
            plain.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if ch == '`' {
            let start = index;
            while index < chars.len() && chars[index] == '`' {
                index += 1;
            }
            let ticks = index - start;
            if let Some(close) = find_run(&chars, index, '`', ticks) {
                flush_plain(&mut spans, &mut plain, base);
                spans.push((chars[index..close].iter().collect(), base.fg(CODE)));
                index = close + ticks;
            } else {
                plain.push('`');
                index = start + 1;
            }
            continue;
        }
        if ch == '*' && chars.get(index + 1) == Some(&'*') {
            if let Some(close) = find_text(&chars, index + 2, "**") {
                let inner: String = chars[index + 2..close].iter().collect();
                if !inner.trim().is_empty() {
                    flush_plain(&mut spans, &mut plain, base);
                    spans.extend(inline_spans(&inner, base.add_modifier(Modifier::BOLD)));
                    index = close + 2;
                    continue;
                }
            }
            plain.push('*');
            index += 1;
            continue;
        }
        if ch == '*' {
            if let Some(close) = find_char(&chars, index + 1, '*') {
                let inner: String = chars[index + 1..close].iter().collect();
                if !inner.trim().is_empty() {
                    flush_plain(&mut spans, &mut plain, base);
                    spans.extend(inline_spans(&inner, base.add_modifier(Modifier::ITALIC)));
                    index = close + 1;
                    continue;
                }
            }
            plain.push('*');
            index += 1;
            continue;
        }
        if ch == '~' && chars.get(index + 1) == Some(&'~') {
            if let Some(close) = find_text(&chars, index + 2, "~~") {
                let inner: String = chars[index + 2..close].iter().collect();
                if !inner.trim().is_empty() {
                    flush_plain(&mut spans, &mut plain, base);
                    spans.extend(inline_spans(
                        &inner,
                        base.add_modifier(Modifier::CROSSED_OUT),
                    ));
                    index = close + 2;
                    continue;
                }
            }
            plain.push('~');
            index += 1;
            continue;
        }
        if ch == '_' && underscore_boundary(&chars, index) {
            let double = chars.get(index + 1) == Some(&'_');
            let width = if double { 2 } else { 1 };
            if let Some(close) = find_underscore(&chars, index + width, double) {
                let inner: String = chars[index + width..close].iter().collect();
                if !inner.trim().is_empty() {
                    flush_plain(&mut spans, &mut plain, base);
                    let style = if double {
                        base.add_modifier(Modifier::BOLD)
                    } else {
                        base.add_modifier(Modifier::ITALIC)
                    };
                    spans.extend(inline_spans(&inner, style));
                    index = close + width;
                    continue;
                }
            }
            plain.push('_');
            index += 1;
            continue;
        }
        if ch == '[' {
            if let Some((label, url, next)) = parse_link(&chars, index) {
                flush_plain(&mut spans, &mut plain, base);
                spans.extend(inline_spans(
                    &label,
                    base.fg(ACCENT).add_modifier(Modifier::UNDERLINED),
                ));
                if !url.is_empty() && url != label {
                    spans.push((format!(" ({url})"), base.fg(DIM)));
                }
                index = next;
                continue;
            }
        }
        plain.push(ch);
        index += 1;
    }
    flush_plain(&mut spans, &mut plain, base);
    merge_spans(spans, base)
}

fn flush_plain(spans: &mut Vec<(String, Style)>, plain: &mut String, style: Style) {
    if !plain.is_empty() {
        spans.push((std::mem::take(plain), style));
    }
}

fn merge_spans(spans: Vec<(String, Style)>, base: Style) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    for (text, style) in spans {
        if text.is_empty() {
            continue;
        }
        if let Some((last, last_style)) = out.last_mut() {
            if *last_style == style {
                last.push_str(&text);
                continue;
            }
        }
        out.push((text, style));
    }
    if out.is_empty() {
        out.push((String::new(), base));
    }
    out
}

fn underscore_boundary(chars: &[char], index: usize) -> bool {
    index == 0 && chars.get(index + 1).is_some_and(|ch| !ch.is_whitespace())
        || index > 0
            && !chars[index - 1].is_alphanumeric()
            && chars.get(index + 1).is_some_and(|ch| !ch.is_whitespace())
}

fn find_underscore(chars: &[char], from: usize, double: bool) -> Option<usize> {
    let width = if double { 2 } else { 1 };
    let mut index = from;
    while index < chars.len() {
        if chars[index] == '_' {
            let mut end = index;
            while end < chars.len() && chars[end] == '_' {
                end += 1;
            }
            let run = end - index;
            if run >= width
                && index > from
                && !chars[index - 1].is_whitespace()
                && (index + width == chars.len() || !chars[index + width].is_alphanumeric())
            {
                return Some(index);
            }
            index = end;
            continue;
        }
        index += 1;
    }
    None
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|index| chars[*index] == target)
}

fn find_text(chars: &[char], from: usize, target: &str) -> Option<usize> {
    let needle: Vec<char> = target.chars().collect();
    if needle.is_empty() || chars.len() < needle.len() {
        return None;
    }
    (from..=chars.len() - needle.len())
        .find(|index| chars[*index..*index + needle.len()] == needle[..])
}

fn find_run(chars: &[char], from: usize, target: char, count: usize) -> Option<usize> {
    let mut index = from;
    while index + count <= chars.len() {
        if (index..index + count).all(|offset| chars[offset] == target)
            && (index == 0 || chars[index - 1] != target)
            && (index + count == chars.len() || chars[index + count] != target)
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = find_char(chars, start + 1, ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = find_char(chars, close + 2, ')')?;
    let label: String = chars[start + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    Some((label, url.trim().to_string(), end + 1))
}

fn wrap_spans(spans: &[(String, Style)], width: usize) -> Vec<Vec<(String, Style)>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current: Vec<(char, Style)> = Vec::new();
    let mut current_width = 0usize;
    let mut break_at: Option<usize> = None;
    for (text, style) in spans {
        for ch in text.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if current_width + ch_width > width && !current.is_empty() {
                if let Some(index) = break_at.take() {
                    let tail = current.split_off(index + 1);
                    trim_trailing_space(&mut current);
                    lines.push(coalesce_line(std::mem::take(&mut current)));
                    current = tail;
                    let lead = current.iter().take_while(|(ch, _)| *ch == ' ').count();
                    if lead > 0 {
                        current.drain(0..lead);
                    }
                    current_width = current.iter().map(|(ch, _)| ch.width().unwrap_or(0)).sum();
                    break_at = current.iter().rposition(|(ch, _)| *ch == ' ');
                } else {
                    trim_trailing_space(&mut current);
                    lines.push(coalesce_line(std::mem::take(&mut current)));
                    current_width = 0;
                }
            }
            if ch == ' ' {
                break_at = Some(current.len());
            }
            current.push((ch, *style));
            current_width += ch_width;
        }
    }
    trim_trailing_space(&mut current);
    if !current.is_empty() || lines.is_empty() {
        lines.push(coalesce_line(current));
    }
    lines
}

fn trim_trailing_space(line: &mut Vec<(char, Style)>) {
    while line.last().is_some_and(|(ch, _)| ch.is_whitespace()) {
        line.pop();
    }
}

fn coalesce_line(line: Vec<(char, Style)>) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    for (ch, style) in line {
        if let Some((text, last_style)) = out.last_mut() {
            if *last_style == style {
                text.push(ch);
                continue;
            }
        }
        out.push((ch.to_string(), style));
    }
    out
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

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn markdown_renders_emphasis_code_and_lists() {
        let lines = markdown_lines("**Bold** and `code` and *soft*\n\n1. first\n2. second", 60);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert!(
            text.iter()
                .any(|line| line.contains("Bold and code and soft")),
            "{text:?}"
        );
        assert!(
            !text
                .iter()
                .any(|line| line.contains("**") || line.contains('`')),
            "{text:?}"
        );
        assert!(
            text.iter().any(|line| line.contains("1. first")),
            "{text:?}"
        );
        let bold = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "Bold")
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let code = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "code")
            .expect("code span");
        assert_eq!(code.style.fg, Some(CODE));
    }

    #[test]
    fn markdown_renders_headings_quotes_and_fenced_code() {
        let lines = markdown_lines("# Title\n> quoted\n```rust\nfn main() {}\n```", 40);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert!(text.iter().any(|line| line == "Title"), "{text:?}");
        assert!(
            text.iter().any(|line| line.contains("│ quoted")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|line| line.contains("│ fn main() {}")),
            "{text:?}"
        );
        assert!(!text.iter().any(|line| line.contains("```")), "{text:?}");
    }

    #[test]
    fn markdown_renders_links_and_keeps_snake_case() {
        let lines = markdown_lines("[docs](https://example.test/x) and snake_case_name", 80);
        let text = lines.iter().map(plain).collect::<Vec<_>>().join(" ");
        assert!(text.contains("docs (https://example.test/x)"), "{text}");
        assert!(text.contains("snake_case_name"), "{text}");
    }

    #[test]
    fn markdown_renders_tables_without_separator_rows() {
        let lines = markdown_lines("| a | b |\n|---|---|\n| 1 | 2 |", 40);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert!(text.iter().any(|line| line.contains("a  │  b")), "{text:?}");
        assert!(text.iter().any(|line| line.contains("1  │  2")), "{text:?}");
        assert!(!text.iter().any(|line| line.contains("---")), "{text:?}");
    }

    #[test]
    fn markdown_keeps_styled_lines_within_width() {
        let lines = markdown_lines(
            "**The quick brown fox jumps over the lazy dog** and `code_span` continues",
            16,
        );
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.width() <= 16, "{}: {line:?}", line.width());
        }
    }

    #[test]
    fn transcript_marks_roles_and_indents_bodies() {
        let mut state = State::new(meta(), 0, 0);
        state.push(Entry::new(Kind::User, "hello there"));
        state.push(Entry::new(Kind::Assistant, "general kenobi"));
        let lines = transcript(&state, 60).lines;
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

    #[test]
    fn live_turn_shows_tools_answer_and_folded_reasoning() {
        let mut state = State::new(meta(), 0, 0);
        state.busy = true;
        state.live = Some(LiveTurn::default());
        state.apply(TurnEvent::Step {
            step: 1,
            max_steps: 4,
        });
        state.apply(TurnEvent::ToolStarted {
            name: "workspace_read".into(),
            activity: "reading notes.txt".into(),
        });
        state.apply(TurnEvent::ToolFinished {
            name: "workspace_read".into(),
            activity: "reading notes.txt".into(),
            ok: true,
        });
        state.apply(TurnEvent::Reasoning(
            "compare both options\npick the safer one\n".into(),
        ));
        state.apply(TurnEvent::Text("**Answer**\n".into()));

        let text = render(&mut state, 80, 30);
        assert!(text.contains("reading notes.txt"), "{text}");
        assert!(text.contains("Answer"), "{text}");
        assert!(text.contains("reasoning line"), "{text}");
        assert!(!text.contains("compare both options"), "{text}");
        assert!(!text.contains("**"), "{text}");

        state.toggle_fold(Fold::Live);
        let text = render(&mut state, 80, 30);
        assert!(text.contains("compare both options"), "{text}");
        assert!(text.contains("pick the safer one"), "{text}");
    }

    #[test]
    fn failed_tools_are_marked_in_the_transcript() {
        let mut state = State::new(meta(), 0, 0);
        state.live = Some(LiveTurn::default());
        state.apply(TurnEvent::ToolFinished {
            name: "workspace_read".into(),
            activity: "reading missing.txt".into(),
            ok: false,
        });
        assert!(state
            .entries
            .iter()
            .any(|entry| entry.text == "reading missing.txt (failed)"));
    }

    #[test]
    fn ctrl_t_toggles_the_latest_thinking_block() {
        let mut state = State::new(meta(), 0, 0);
        state.push(Entry::new(Kind::Reasoning, "earlier thought"));
        state.push(Entry::new(Kind::Reasoning, "later thought"));
        assert_eq!(state.latest_fold(), Some(Fold::Entry(1)));
        editor_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert!(state.entries[1].open);
        assert!(!state.entries[0].open);
        editor_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert!(!state.entries[1].open);
    }

    #[test]
    fn thinking_blocks_fold_independently() {
        let mut state = State::new(meta(), 0, 0);
        state.push(Entry::new(Kind::Reasoning, "first thought"));
        state.push(Entry::new(Kind::Reasoning, "second thought"));
        let text = render(&mut state, 80, 30);
        let rows: Vec<&str> = text.lines().collect();
        let first = rows
            .iter()
            .position(|row| row.contains("▸ think"))
            .expect("first folded block");
        let second = first
            + 1
            + rows[first + 1..]
                .iter()
                .position(|row| row.contains("▸ think"))
                .expect("second folded block");
        state.click(4, second as u16);
        assert!(state.entries[1].open);
        assert!(!state.entries[0].open);
        let text = render(&mut state, 80, 30);
        assert!(text.contains("second thought"), "{text}");
        assert!(!text.contains("first thought"), "{text}");
    }

    #[test]
    fn an_expanded_live_block_stays_expanded_when_committed() {
        let mut state = State::new(meta(), 0, 0);
        state.live = Some(LiveTurn::default());
        state.apply(TurnEvent::Reasoning("a thought".into()));
        state.toggle_fold(Fold::Live);
        let open = state.live.as_ref().expect("live turn").reasoning_open;
        state.push(Entry::new(Kind::Reasoning, "a thought").folded(open));
        assert!(state.entries[0].open);
    }

    #[test]
    fn wheel_scrolling_leaves_and_returns_to_the_bottom() {
        let mut state = State::new(meta(), 0, 0);
        for index in 0..20 {
            state.push(Entry::new(Kind::User, format!("question {index}")));
        }
        render(&mut state, 80, 24);
        assert!(state.follow);
        assert_eq!(state.lines_below(), 0);

        handle_mouse(
            &mut state,
            MouseEvent {
                column: 10,
                row: 10,
                kind: MouseEventKind::ScrollUp,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!state.follow);
        assert!(state.scroll_top > 0);
        assert!(state.lines_below() > 0);

        for _ in 0..20 {
            handle_mouse(
                &mut state,
                MouseEvent {
                    column: 10,
                    row: 10,
                    kind: MouseEventKind::ScrollDown,
                    modifiers: KeyModifiers::NONE,
                },
            );
        }
        assert!(state.follow);
        assert_eq!(state.lines_below(), 0);
    }

    #[test]
    fn a_long_transcript_gets_a_scrollbar_and_a_below_tip() {
        let mut state = State::new(meta(), 0, 0);
        for index in 0..20 {
            state.push(Entry::new(Kind::User, format!("question {index}")));
        }
        let text = render(&mut state, 80, 24);
        assert!(text.contains('║'), "{text}");
        assert!(text.contains('█'), "{text}");
        assert!(state.scrollbar_col.is_some());

        state.scroll_to_top();
        let text = render(&mut state, 80, 24);
        assert!(text.contains("below the bottom"), "{text}");
        assert!(text.contains("question 0"), "{text}");
        assert!(!text.contains("question 19"), "{text}");
    }

    #[test]
    fn home_and_end_jump_the_transcript_when_the_editor_is_empty() {
        let mut state = State::new(meta(), 0, 0);
        for index in 0..20 {
            state.push(Entry::new(Kind::User, format!("question {index}")));
        }
        render(&mut state, 80, 24);
        editor_key(&mut state, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(state.scroll_top, 0);
        assert!(!state.follow);
        editor_key(&mut state, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert!(state.follow);
    }

    #[test]
    fn home_and_end_still_edit_the_input_when_it_is_not_empty() {
        let mut state = State::new(meta(), 0, 0);
        state.editor.insert_str("draft");
        editor_key(&mut state, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(state.editor.cursor, 0);
        assert!(state.follow);
        editor_key(&mut state, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(state.editor.cursor, 5);
    }

    fn palette_choice(state: &State) -> Option<&'static Command> {
        let palette = state.palette.as_ref()?;
        if palette.mode != PaletteMode::Commands {
            return None;
        }
        palette.command_choice(&state.editor)
    }

    fn open_palette(state: &mut State, query: &str) {
        state.editor.clear();
        editor_only_key(state, KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for ch in query.chars() {
            editor_only_key(state, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    fn workspace(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "jani-tui-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn typing_slash_opens_a_filterable_command_palette() {
        let mut state = State::new(meta(), 0, 0);
        open_palette(&mut state, "");
        assert!(state.palette.is_some());
        let text = render(&mut state, 100, 30);
        assert!(text.contains("commands"), "{text}");
        assert!(text.contains("/status"), "{text}");
        assert!(text.contains("Show frozen authority"), "{text}");
        assert!(text.contains("enter run"), "{text}");
        assert!(text.contains('›'), "{text}");

        for ch in "st".chars() {
            editor_only_key(
                &mut state,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        let text = render(&mut state, 100, 30);
        assert!(text.contains("commands · st"), "{text}");
        assert!(text.contains("/status"), "{text}");
        assert!(!text.contains("/budget"), "{text}");
        assert_eq!(palette_choice(&state).unwrap().name, "status");
    }

    #[test]
    fn palette_navigation_cycles_and_resets_on_filter_edits() {
        let mut state = State::new(meta(), 0, 0);
        open_palette(&mut state, "s");
        assert_eq!(palette_choice(&state).unwrap().name, "status");
        editor_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(palette_choice(&state).unwrap().name, "skills");
        editor_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(palette_choice(&state).unwrap().name, "status");
        editor_key(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(palette_choice(&state).unwrap().name, "skills");

        open_palette(&mut state, "");
        editor_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(palette_choice(&state).unwrap().name, "status");
        editor_only_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert_eq!(palette_choice(&state).unwrap().name, "new");
    }

    #[test]
    fn palette_tab_fills_the_name_and_escape_clears() {
        let mut state = State::new(meta(), 0, 0);
        open_palette(&mut state, "sta");
        editor_key(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(state.palette.is_none());
        assert_eq!(state.editor.text, "/status");

        open_palette(&mut state, "appr");
        editor_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        editor_key(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(state.palette.is_none());
        assert_eq!(state.editor.text, "/approval ");

        open_palette(&mut state, "sta");
        editor_key(&mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.palette.is_none());
        assert!(state.editor.is_empty());
    }

    #[test]
    fn deleting_the_slash_closes_the_palette() {
        let mut state = State::new(meta(), 0, 0);
        open_palette(&mut state, "s");
        editor_only_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert!(state.palette.is_some());
        editor_only_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert!(state.palette.is_none());
        assert!(state.editor.is_empty());
    }

    #[test]
    fn the_palette_does_not_open_while_a_turn_runs() {
        let mut state = State::new(meta(), 0, 0);
        state.busy = true;
        editor_only_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        assert!(state.palette.is_none());
        assert_eq!(state.editor.text, "/");
    }

    #[test]
    fn palette_marks_commands_that_still_need_an_argument() {
        let mut state = State::new(meta(), 0, 0);
        open_palette(&mut state, "appr");
        editor_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        match palette_action(&state) {
            Some(PaletteAction::Prefill(command)) => assert_eq!(command.name, "approval"),
            _ => panic!("expected an argument prompt"),
        }
        open_palette(&mut state, "sta");
        match palette_action(&state) {
            Some(PaletteAction::Run(command)) => assert_eq!(command.name, "status"),
            _ => panic!("expected an immediate action"),
        }
        open_palette(&mut state, "model");
        match palette_action(&state) {
            Some(PaletteAction::Models) => {}
            _ => panic!("expected the model picker"),
        }
    }

    #[test]
    fn palette_enter_runs_the_selected_command_end_to_end() {
        let root = workspace("palette");
        let mut cfg = Config::baseline();
        cfg.root = root.clone();
        cfg.state_dir = root.join("state");
        cfg.tenant = "t1".into();
        cfg.principal = "p1".into();
        cfg.task_id = Some("task-1".into());
        let catalog = Catalog::default();
        let mut agent = Agent::new(&cfg, &catalog).unwrap();
        let mut state = State::new(meta(), 0, 0);
        state.push(Entry::new(Kind::User, "previous"));

        open_palette(&mut state, "ne");
        assert_eq!(palette_choice(&state).unwrap().name, "new");
        palette_run(&mut state, &mut agent, &catalog);

        assert!(state.palette.is_none());
        assert!(state.editor.is_empty());
        assert!(!state.entries.iter().any(|entry| entry.text == "previous"));
        assert!(state
            .entries
            .iter()
            .any(|entry| entry.kind == Kind::Command && entry.text == "/new"));
    }

    fn model_config(root: &std::path::Path) -> Config {
        std::fs::create_dir_all(root.join(".jani")).unwrap();
        let path = root.join(".jani").join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "version": 1,
                "model": "alpha",
                "models": {
                    "alpha": {
                        "model": "alpha-model",
                        "apiBase": "https://alpha.test/v1",
                        "apiKey": "unit-test-alpha-key"
                    },
                    "beta": {
                        "model": "beta-model",
                        "apiBase": "https://beta.test/v1",
                        "apiKey": "unit-test-beta-key"
                    }
                }
            }"#,
        )
        .unwrap();
        Config::from_args(&[
            "repl".to_string(),
            "--repo".into(),
            root.to_string_lossy().into_owned(),
            "--settings".into(),
            path.to_string_lossy().into_owned(),
        ])
        .unwrap()
    }

    fn type_text(state: &mut State, text: &str) {
        for ch in text.chars() {
            editor_only_key(state, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    #[test]
    fn the_model_picker_switches_between_settings_profiles() {
        let root = workspace("model-picker");
        let cfg = model_config(&root);
        let catalog = Catalog::default();
        let mut agent = Agent::new(&cfg, &catalog).unwrap();
        let mut state = State::new(Meta::new(&cfg, &agent), 0, 0);
        state.models = cfg.model_choices.clone();

        open_palette(&mut state, "model");
        assert_eq!(palette_choice(&state).unwrap().name, "model");
        palette_run(&mut state, &mut agent, &catalog);
        assert_eq!(state.palette.as_ref().unwrap().mode, PaletteMode::Models);
        assert_eq!(state.editor.text, "/model ");
        let text = render(&mut state, 100, 30);
        assert!(text.contains("alpha"), "{text}");
        assert!(text.contains("beta-model"), "{text}");
        assert!(text.contains("current"), "{text}");
        assert!(text.contains("enter switch"), "{text}");

        type_text(&mut state, "be");
        let text = render(&mut state, 100, 30);
        assert!(text.contains("model · be"), "{text}");
        assert!(text.contains("beta-model"), "{text}");
        let rows = state
            .palette
            .as_ref()
            .unwrap()
            .matches(&state.editor, &state.models);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], PaletteRow::Profile(choice) if choice.label == "beta"));

        palette_run(&mut state, &mut agent, &catalog);
        assert!(state.palette.is_none());
        assert_eq!(agent.model(), "beta-model");
        assert_eq!(state.meta.model, "beta-model");
        assert_eq!(state.meta.profile.as_deref(), Some("beta"));
        assert!(state
            .entries
            .iter()
            .any(|entry| entry.kind == Kind::Command && entry.text == "/model beta"));
        assert!(state
            .entries
            .iter()
            .any(|entry| entry.text.contains("Model switched to beta-model")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_model_picker_accepts_a_custom_model_id_without_profiles() {
        let root = workspace("model-custom");
        let mut cfg = Config::baseline();
        cfg.root = root.clone();
        cfg.state_dir = root.join("state");
        cfg.model = "test-model".into();
        cfg.api_key = Some("unit-test-key".into());
        cfg.api_key_source = "test".into();
        let catalog = Catalog::default();
        let mut agent = Agent::new(&cfg, &catalog).unwrap();
        let mut state = State::new(meta(), 0, 0);
        state.models = cfg.model_choices.clone();

        type_text(&mut state, "/model ");
        assert_eq!(state.palette.as_ref().unwrap().mode, PaletteMode::Models);
        let text = render(&mut state, 100, 30);
        assert!(text.contains("no model profiles"), "{text}");

        type_text(&mut state, "custom-123");
        let text = render(&mut state, 100, 30);
        assert!(text.contains("custom-123"), "{text}");
        assert!(text.contains("use as a model ID"), "{text}");

        palette_run(&mut state, &mut agent, &catalog);
        assert_eq!(agent.model(), "custom-123");
        assert_eq!(agent.model_profile(), None);
        assert_eq!(state.meta.model, "custom-123");
        assert_eq!(state.meta.profile, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_model_picker_tab_completes_and_escape_clears() {
        let root = workspace("model-tab");
        let cfg = model_config(&root);
        let catalog = Catalog::default();
        let agent = Agent::new(&cfg, &catalog).unwrap();
        let mut state = State::new(Meta::new(&cfg, &agent), 0, 0);
        state.models = cfg.model_choices.clone();

        type_text(&mut state, "/model be");
        editor_key(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(state.palette.is_none());
        assert_eq!(state.editor.text, "/model beta ");

        state.editor.clear();
        type_text(&mut state, "/model ");
        assert!(state.palette.is_some());
        editor_key(&mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.palette.is_none());
        assert!(state.editor.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_model_picker_does_not_open_while_a_turn_runs() {
        let mut state = State::new(meta(), 0, 0);
        state.busy = true;
        type_text(&mut state, "/model ");
        assert!(state.palette.is_none());
        assert_eq!(state.editor.text, "/model ");
    }
}
