use crate::diff::{DiffResult, SyncAction};
use crate::journal::format_bytes;
use crate::scan::Entry as ScanEntry;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction as LayoutDir, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};
use std::io::{self, Stdout};

struct AppState {
    conflicts: Vec<DiffResult>,
    choices: Vec<Option<SyncAction>>,
    selected_index: usize,
}

impl AppState {
    fn new(conflicts: Vec<DiffResult>) -> Self {
        let len = conflicts.len();
        Self {
            conflicts,
            choices: vec![None; len],
            selected_index: 0,
        }
    }

    fn next(&mut self) {
        if self.selected_index < self.conflicts.len() - 1 {
            self.selected_index += 1;
        }
    }

    fn previous(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    fn resolve_current(&mut self, action: SyncAction) {
        if self.selected_index < self.choices.len() {
            self.choices[self.selected_index] = Some(action);
            self.next();
        }
    }
}

pub struct Ui;

impl Ui {
    pub fn resolve_conflicts(conflicts: Vec<DiffResult>) -> Result<Vec<ConflictDecision>> {
        if conflicts.is_empty() {
            return Ok(Vec::new());
        }

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut app = AppState::new(conflicts);
        let res = Self::run_app(&mut terminal, &mut app);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        res?;

        Ok(Self::collect_choices(app))
    }

    fn run_app(
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        app: &mut AppState,
    ) -> Result<()> {
        loop {
            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(LayoutDir::Vertical)
                    .constraints([Constraint::Percentage(80), Constraint::Percentage(20)].as_ref())
                    .split(f.area());

                let items: Vec<ListItem> = app.conflicts.iter().enumerate().map(|(i, c)| {
                    let status = match &app.choices[i] {
                        Some(SyncAction::CopyAtoB) => "[Use A]",
                        Some(SyncAction::CopyBtoA) => "[Use B]",
                        Some(SyncAction::NoOp) => "[Skip]",
                        Some(_) => "[Other]",
                        None => "[UNRESOLVED]",
                    };

                    let conflict_desc = match &c.action {
                        SyncAction::Conflict(reason) => format!("{:?}", reason),
                        _ => "Unknown".to_string(),
                    };

                    let content = format!("{} {} - {}", status, c.path, conflict_desc);
                    let style = if i == app.selected_index {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else if app.choices[i].is_none() {
                         Style::default().fg(Color::Red)
                    } else {
                         Style::default().fg(Color::Green)
                    };

                    ListItem::new(content).style(style)
                }).collect();

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("Conflicts"));

                let mut state = ListState::default();
                state.select(Some(app.selected_index));

                f.render_stateful_widget(list, chunks[0], &mut state);

                let current_conflict = &app.conflicts[app.selected_index];
                let instructions = format!(
                    "Path: {}\nA: {:?}\n{}\n\nB: {:?}\n{}\n\n[A] Use Local (A->B) | [B] Use Remote (B->A) | [S] Skip | [Enter] Done | [q] Abort",
                    current_conflict.path,
                    current_conflict.change_a.change,
                    entry_summary(current_conflict.change_a.entry_now.as_ref()),
                    current_conflict.change_b.change,
                    entry_summary(current_conflict.change_b.entry_now.as_ref())
                );
                let p = Paragraph::new(instructions).block(Block::default().borders(Borders::ALL).title("Actions"));
                f.render_widget(p, chunks[1]);

            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') => {
                                anyhow::bail!("Sync aborted by user");
                            }
                            KeyCode::Up => app.previous(),
                            KeyCode::Down => app.next(),
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                app.resolve_current(SyncAction::CopyAtoB);
                            }
                            KeyCode::Char('b') | KeyCode::Char('B') => {
                                app.resolve_current(SyncAction::CopyBtoA);
                            }
                            KeyCode::Char('s') | KeyCode::Char('S') => {
                                app.resolve_current(SyncAction::NoOp);
                            }
                            KeyCode::Enter => {
                                if app.choices.iter().all(|c| c.is_some()) {
                                    return Ok(());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    fn collect_choices(app: AppState) -> Vec<ConflictDecision> {
        let mut decisions = Vec::new();
        for (i, conflict) in app.conflicts.into_iter().enumerate() {
            let choice = match app.choices.get(i).and_then(|c| c.clone()) {
                Some(action) => action,
                None => continue,
            };
            let action = match choice {
                SyncAction::CopyAtoB => {
                    if conflict.change_a.entry_now.is_some() {
                        SyncAction::CopyAtoB
                    } else {
                        SyncAction::DeleteB
                    }
                }
                SyncAction::CopyBtoA => {
                    if conflict.change_b.entry_now.is_some() {
                        SyncAction::CopyBtoA
                    } else {
                        SyncAction::DeleteA
                    }
                }
                SyncAction::NoOp => SyncAction::NoOp,
                _ => SyncAction::NoOp,
            };
            decisions.push(ConflictDecision {
                path: conflict.path,
                action,
            });
        }
        decisions
    }
}

#[derive(Debug, Clone)]
pub struct ConflictDecision {
    pub path: String,
    pub action: SyncAction,
}

fn entry_summary(entry: Option<&ScanEntry>) -> String {
    match entry {
        Some(e) => {
            let hash_str = e
                .hash
                .as_ref()
                .map(|h| {
                    let s = hex::encode(h);
                    s[..s.len().min(12)].to_string()
                })
                .unwrap_or_else(|| "-".to_string());
            let mode_perms = e.mode & 0o7777;
            format!(
                "   size={}  mtime={}  mode={:o}  hash={}",
                format_bytes(e.size),
                format_mtime(e.mtime),
                mode_perms,
                hash_str
            )
        }
        None => "   (absent)".to_string(),
    }
}

fn format_mtime(epoch: i64) -> String {
    if epoch <= 0 {
        return "unknown".to_string();
    }
    let secs = epoch as u64;
    let sec_of_day = secs % 86400;
    let hours = sec_of_day / 3600;
    let minutes = (sec_of_day % 3600) / 60;
    let seconds = sec_of_day % 60;

    let mut days = (secs / 86400) as i64;
    let mut year = 1970i32;

    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if days < md as i64 {
            month = i;
            break;
        }
        days -= md as i64;
    }

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year,
        month + 1,
        days + 1,
        hours,
        minutes,
        seconds
    )
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
