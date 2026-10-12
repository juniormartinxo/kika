use std::io;
use std::time::Duration;

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::{DefaultTerminal, Frame};

use crate::cleanup::{CleanupCandidate, format_bytes};
use crate::groups::{Group, GroupKind};

const PAGE: usize = 10;
const HELP: &str = "↑↓/jk move · space mark · a all · tab buttons · enter activate · c clean selected · C clean all · q quit";

/// What the user decided in the interactive screen.
pub enum Choice {
    Quit,
    Clean(Vec<CleanupCandidate>),
}

/// Shows the groups as a checklist with "Clean selected" and "Clean all" buttons.
pub fn select(groups: Vec<Group>, warnings: usize) -> io::Result<Choice> {
    let mut terminal = ratatui::try_init()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnableMouseCapture)?;

    // Keys typed while the scan was running must not act on the list (a stray
    // `C` then `y` would clean everything).
    while event::poll(Duration::ZERO)? {
        event::read()?;
    }

    App::new(groups, warnings).run(&mut terminal)
}

/// Leaves raw mode, the alternate screen and mouse capture on every exit path.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    CleanSelected,
    CleanAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Selected,
    All,
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    None,
    Quit,
    Clean(Scope),
}

/// A table row: either a group or a section title that cannot be selected.
enum Slot {
    Group(usize),
    Section(&'static str),
}

/// Where widgets were last drawn, for mouse hit-testing.
#[derive(Default)]
struct HitAreas {
    table_body: Rect,
    clean_selected: Rect,
    clean_all: Rect,
}

struct App {
    groups: Vec<Group>,
    checked: Vec<bool>,
    slots: Vec<Slot>,
    cursor: usize,
    focus: Focus,
    confirm: Option<Scope>,
    status: Option<String>,
    warnings: usize,
    table_state: TableState,
    hit: HitAreas,
}

impl App {
    fn new(groups: Vec<Group>, warnings: usize) -> Self {
        let mut slots = Vec::with_capacity(groups.len() + 1);
        let mut in_caches = false;

        for (index, group) in groups.iter().enumerate() {
            if group.kind == GroupKind::GlobalCache && !in_caches {
                slots.push(Slot::Section("Global caches"));
                in_caches = true;
            }
            slots.push(Slot::Group(index));
        }

        Self {
            checked: vec![false; groups.len()],
            groups,
            slots,
            cursor: 0,
            focus: Focus::List,
            confirm: None,
            status: None,
            warnings,
            table_state: TableState::default(),
            hit: HitAreas::default(),
        }
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<Choice> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            let action = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
                Event::Mouse(mouse) => self.handle_mouse(mouse),
                _ => Action::None,
            };

            match action {
                Action::None => {}
                Action::Quit => return Ok(Choice::Quit),
                Action::Clean(scope) => return Ok(Choice::Clean(self.take(scope))),
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if self.confirm.is_some() {
            return match key.code {
                KeyCode::Char('y' | 'Y') => self.confirm.take().map_or(Action::None, Action::Clean),
                _ => {
                    self.confirm = None;
                    Action::None
                }
            };
        }

        self.status = None;

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-(PAGE as isize)),
            KeyCode::PageDown => self.move_cursor(PAGE as isize),
            KeyCode::Home | KeyCode::Char('g') => self.move_cursor(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.move_cursor(isize::MAX),
            KeyCode::Char(' ') | KeyCode::Enter => self.activate(),
            KeyCode::Char('a') => {
                self.toggle_all();
                Action::None
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::List => Focus::CleanSelected,
                    Focus::CleanSelected => Focus::CleanAll,
                    Focus::CleanAll => Focus::List,
                };
                Action::None
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::List => Focus::CleanAll,
                    Focus::CleanSelected => Focus::List,
                    Focus::CleanAll => Focus::CleanSelected,
                };
                Action::None
            }
            KeyCode::Char('c') => self.request(Scope::Selected),
            KeyCode::Char('C') => self.request(Scope::All),
            _ => Action::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Action {
        if self.confirm.is_some() {
            return Action::None;
        }

        let position = Position::new(mouse.column, mouse.row);

        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_cursor(-1),
            MouseEventKind::ScrollDown => self.move_cursor(1),
            MouseEventKind::Down(MouseButton::Left) => {
                self.status = None;

                if self.hit.clean_selected.contains(position) {
                    self.focus = Focus::CleanSelected;
                    return self.request(Scope::Selected);
                }

                if self.hit.clean_all.contains(position) {
                    self.focus = Focus::CleanAll;
                    return self.request(Scope::All);
                }

                if self.hit.table_body.contains(position) {
                    let row =
                        usize::from(position.y - self.hit.table_body.y) + self.table_state.offset();
                    if let Some(Slot::Group(index)) = self.slots.get(row) {
                        self.cursor = *index;
                        self.focus = Focus::List;
                        self.toggle(*index);
                    }
                }

                Action::None
            }
            _ => Action::None,
        }
    }

    fn move_cursor(&mut self, delta: isize) -> Action {
        if self.groups.is_empty() {
            return Action::None;
        }

        let last = self.groups.len() - 1;
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.focus = Focus::List;
        Action::None
    }

    fn activate(&mut self) -> Action {
        match self.focus {
            Focus::List => {
                self.toggle(self.cursor);
                Action::None
            }
            Focus::CleanSelected => self.request(Scope::Selected),
            Focus::CleanAll => self.request(Scope::All),
        }
    }

    fn toggle(&mut self, index: usize) {
        if let Some(checked) = self.checked.get_mut(index) {
            *checked = !*checked;
        }
    }

    fn toggle_all(&mut self) {
        let mark = !self.checked.iter().all(|checked| *checked);
        self.checked.fill(mark);
    }

    fn request(&mut self, scope: Scope) -> Action {
        if self.selection(scope).is_empty() {
            self.status = Some("Nothing selected: mark groups with space or the mouse".to_string());
        } else {
            self.confirm = Some(scope);
        }
        Action::None
    }

    fn selection(&self, scope: Scope) -> Vec<usize> {
        (0..self.groups.len())
            .filter(|index| scope == Scope::All || self.checked[*index])
            .collect()
    }

    /// (groups, items, bytes) for the given scope.
    fn totals(&self, scope: Scope) -> (usize, usize, u64) {
        let selection = self.selection(scope);
        let items = selection
            .iter()
            .map(|index| self.groups[*index].candidates.len())
            .sum();
        let bytes = selection
            .iter()
            .map(|index| self.groups[*index].size_bytes())
            .sum();
        (selection.len(), items, bytes)
    }

    fn take(self, scope: Scope) -> Vec<CleanupCandidate> {
        let selection = self.selection(scope);
        self.groups
            .into_iter()
            .enumerate()
            .filter(|(index, _)| selection.contains(index))
            .flat_map(|(_, group)| group.candidates)
            .collect()
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [table_area, buttons_area, help_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        self.draw_table(frame, table_area);
        self.draw_buttons(frame, buttons_area);

        let help = match &self.status {
            Some(status) => Line::from(status.as_str()).yellow(),
            None => Line::from(HELP).dim(),
        };
        frame.render_widget(Paragraph::new(help), help_area);

        if let Some(scope) = self.confirm {
            self.draw_confirm(frame, scope);
        }
    }

    fn draw_table(&mut self, frame: &mut Frame, area: Rect) {
        let (groups, _, bytes) = self.totals(Scope::All);
        let mut block = Block::bordered().title(
            Line::from(format!(
                " kika · {} · {} reclaimable ",
                plural(groups, "group"),
                format_bytes(bytes)
            ))
            .bold(),
        );
        if self.warnings > 0 {
            block = block.title_bottom(
                Line::from(format!(" {} warnings, shown on exit ", self.warnings)).yellow(),
            );
        }

        let rows = self.slots.iter().map(|slot| match slot {
            Slot::Section(title) => Row::new([Cell::from(""), Cell::from(*title)]).bold().cyan(),
            Slot::Group(index) => self.group_row(*index),
        });
        let header = Row::new(["", "Name", "Items", "Size", "Contents"])
            .bold()
            .underlined();
        let highlight = if self.focus == Focus::List {
            Style::new().bg(Color::DarkGray).bold()
        } else {
            Style::new().bg(Color::DarkGray)
        };

        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(5),
                Constraint::Length(9),
                Constraint::Fill(1),
            ],
        )
        .header(header)
        .block(block)
        .row_highlight_style(highlight)
        .highlight_symbol("› ");

        let selected_row = self
            .slots
            .iter()
            .position(|slot| matches!(slot, Slot::Group(index) if *index == self.cursor));
        self.table_state.select(selected_row);
        frame.render_stateful_widget(table, area, &mut self.table_state);

        let inner = area.inner(Margin::new(1, 1));
        self.hit.table_body = Rect {
            y: inner.y.saturating_add(1),
            height: inner.height.saturating_sub(1),
            ..inner
        };
    }

    fn group_row(&self, index: usize) -> Row<'static> {
        let group = &self.groups[index];
        let checkbox = if self.checked[index] {
            Span::from("[x]").green().bold()
        } else {
            Span::from("[ ]")
        };
        let kinds = group
            .kinds()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");

        Row::new([
            Cell::from(checkbox),
            Cell::from(group.label.clone()),
            Cell::from(Line::from(group.candidates.len().to_string()).right_aligned()),
            Cell::from(Line::from(format_bytes(group.size_bytes())).right_aligned()),
            Cell::from(Line::from(kinds).dim()),
        ])
    }

    fn draw_buttons(&mut self, frame: &mut Frame, area: Rect) {
        let (selected, _, selected_bytes) = self.totals(Scope::Selected);
        let (_, _, all_bytes) = self.totals(Scope::All);

        let selected_label = format!(
            " Clean selected · {selected} · {} ",
            format_bytes(selected_bytes)
        );
        let all_label = format!(" Clean all · {} ", format_bytes(all_bytes));

        let [selected_area, all_area] = Layout::horizontal([
            Constraint::Length(text_width(&selected_label)),
            Constraint::Length(text_width(&all_label)),
        ])
        .spacing(2)
        .areas(area);

        let style = |focus: Focus, enabled: bool| {
            if self.focus == focus {
                Style::new().bg(Color::Cyan).fg(Color::Black).bold()
            } else if enabled {
                Style::new().bg(Color::DarkGray).bold()
            } else {
                Style::new().bg(Color::DarkGray).dim()
            }
        };

        frame.render_widget(
            Paragraph::new(selected_label).style(style(Focus::CleanSelected, selected > 0)),
            selected_area,
        );
        frame.render_widget(
            Paragraph::new(all_label).style(style(Focus::CleanAll, true)),
            all_area,
        );

        self.hit.clean_selected = selected_area;
        self.hit.clean_all = all_area;
    }

    fn draw_confirm(&self, frame: &mut Frame, scope: Scope) {
        let (groups, items, bytes) = self.totals(scope);
        let area = frame
            .area()
            .centered(Constraint::Length(56), Constraint::Length(6));

        let text = vec![
            Line::from(format!(
                "Remove {} ({}, {})?",
                plural(groups, "group"),
                plural(items, "item"),
                format_bytes(bytes)
            ))
            .bold(),
            Line::from(""),
            Line::from(vec![
                Span::from("[y]").green().bold(),
                Span::from(" yes    "),
                Span::from("[n]").bold(),
                Span::from(" no"),
            ]),
        ];

        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text).centered().block(
                Block::bordered()
                    .title(" Confirm ")
                    .border_style(Style::new().yellow()),
            ),
            area,
        );
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn text_width(text: &str) -> u16 {
    u16::try_from(text.chars().count()).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Action, App, Focus, Scope};
    use crate::cleanup::{CleanupCandidate, CleanupKind};
    use crate::groups::{Group, GroupKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn group(label: &str, kind: GroupKind, sizes: &[u64]) -> Group {
        Group {
            label: label.to_string(),
            kind,
            candidates: sizes
                .iter()
                .map(|size| CleanupCandidate {
                    path: PathBuf::from(format!("/work/{label}/node_modules")),
                    kind: CleanupKind::NodeModules,
                    size_bytes: *size,
                })
                .collect(),
        }
    }

    fn app() -> App {
        App::new(
            vec![
                group("web", GroupKind::Repository, &[300, 100]),
                group("api", GroupKind::Repository, &[200]),
                group("uv cache", GroupKind::GlobalCache, &[1_000]),
            ],
            0,
        )
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn marks_groups_and_cleans_only_the_selection() {
        let mut app = app();

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.totals(Scope::Selected), (1, 1, 200));

        assert_eq!(press(&mut app, KeyCode::Char('c')), Action::None);
        assert_eq!(app.confirm, Some(Scope::Selected));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            Action::Clean(Scope::Selected)
        );

        let candidates = app.take(Scope::Selected);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].size_bytes, 200);
    }

    #[test]
    fn cleaning_requires_confirmation() {
        let mut app = app();

        press(&mut app, KeyCode::Char('C'));
        assert_eq!(app.confirm, Some(Scope::All));
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.confirm, None);
        assert_eq!(app.totals(Scope::All), (3, 4, 1_600));
    }

    #[test]
    fn refuses_clean_selected_with_nothing_marked() {
        let mut app = app();

        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.confirm, None);
        assert!(app.status.is_some());
    }

    #[test]
    fn toggles_all_and_cycles_focus() {
        let mut app = app();

        press(&mut app, KeyCode::Char('a'));
        assert!(app.checked.iter().all(|checked| *checked));
        press(&mut app, KeyCode::Char('a'));
        assert!(app.checked.iter().all(|checked| !*checked));

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::CleanSelected);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::CleanAll);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.confirm, Some(Scope::All));
    }

    #[test]
    fn cursor_stays_in_bounds() {
        let mut app = app();

        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor, 0);
        press(&mut app, KeyCode::End);
        assert_eq!(app.cursor, 2);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn renders_groups_sections_and_buttons() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();

        terminal.draw(|frame| app.draw(frame)).unwrap();

        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        for expected in [
            "web",
            "api",
            "Global caches",
            "uv cache",
            "Clean selected",
            "Clean all",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
    }
}
