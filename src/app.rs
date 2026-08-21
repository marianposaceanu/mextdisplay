use std::io;

use anyhow::Result;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
};

use crate::display::{Display, DisplayManager, DisplayStatus};

const ACCENT: Color = Color::Rgb(108, 182, 255);
const ENABLED: Color = Color::Rgb(105, 219, 154);
const DISABLED: Color = Color::Rgb(255, 190, 92);
const MUTED: Color = Color::Rgb(130, 141, 158);

pub fn run(manager: DisplayManager) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = App::new(manager).and_then(|mut app| app.run(&mut terminal));
    ratatui::restore();
    result
}

struct App {
    manager: DisplayManager,
    displays: Vec<Display>,
    selected: usize,
    confirmation: Option<String>,
    notice: Notice,
}

struct Notice {
    text: String,
    error: bool,
}

impl App {
    fn new(manager: DisplayManager) -> Result<Self> {
        let displays = manager.refresh()?;
        Ok(Self {
            manager,
            displays,
            selected: 0,
            confirmation: None,
            notice: Notice {
                text: "Select an external display and press Enter to toggle its video output."
                    .to_owned(),
                error: false,
            },
        })
    }

    fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Press && self.handle_key(key) {
                return Ok(());
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.confirmation.is_some() {
            match key.code {
                KeyCode::Char('y' | 'Y') => self.confirm_disable(),
                KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.confirmation = None;
                    self.info("Disable cancelled.");
                }
                _ => {}
            }
            return false;
        }

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Up | KeyCode::Char('k') => self.select_previous(),
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_selected(),
            _ => {}
        }
        false
    }

    fn select_previous(&mut self) {
        if self.displays.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.displays.len() - 1
        } else {
            self.selected - 1
        };
    }

    fn select_next(&mut self) {
        if !self.displays.is_empty() {
            self.selected = (self.selected + 1) % self.displays.len();
        }
    }

    fn refresh(&mut self) {
        let selected_uuid = self.selected_display().map(|display| display.uuid.clone());
        match self.manager.refresh() {
            Ok(displays) => {
                self.displays = displays;
                self.selected = selected_uuid
                    .and_then(|uuid| {
                        self.displays
                            .iter()
                            .position(|display| display.uuid == uuid)
                    })
                    .unwrap_or(0);
                self.info("Display list refreshed.");
            }
            Err(error) => self.error(format!("Refresh failed: {error:#}")),
        }
    }

    fn toggle_selected(&mut self) {
        let Some(display) = self.selected_display().cloned() else {
            self.error("No display selected.");
            return;
        };

        match display.status {
            DisplayStatus::Disabled => match self.manager.enable(&display.uuid) {
                Ok(message) => {
                    if self.refresh_after_action(&display.uuid) {
                        self.info(message);
                    }
                }
                Err(error) => self.error(format!("Enable failed: {error:#}")),
            },
            DisplayStatus::Enabled => {
                if display.builtin {
                    self.error("The built-in display is protected and cannot be disabled.");
                } else if display.main {
                    self.error(
                        "This is the main display. Make another display main in System Settings first.",
                    );
                } else {
                    self.confirmation = Some(display.uuid);
                }
            }
        }
    }

    fn confirm_disable(&mut self) {
        let Some(uuid) = self.confirmation.take() else {
            return;
        };
        match self.manager.disable(&uuid) {
            Ok(message) => {
                if self.refresh_after_action(&uuid) {
                    self.info(message);
                }
            }
            Err(error) => self.error(format!("Disable failed: {error:#}")),
        }
    }

    fn refresh_after_action(&mut self, uuid: &str) -> bool {
        match self.manager.refresh() {
            Ok(displays) => {
                self.displays = displays;
                self.selected = self
                    .displays
                    .iter()
                    .position(|display| display.uuid == uuid)
                    .unwrap_or(0);
                true
            }
            Err(error) => {
                self.error(format!("Display changed, but refresh failed: {error:#}"));
                false
            }
        }
    }

    fn selected_display(&self) -> Option<&Display> {
        self.displays.get(self.selected)
    }

    fn info(&mut self, message: impl Into<String>) {
        self.notice = Notice {
            text: message.into(),
            error: false,
        };
    }

    fn error(&mut self, message: impl Into<String>) {
        self.notice = Notice {
            text: message.into(),
            error: true,
        };
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let [header, stats, table, details, notice, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .areas(area);

        self.draw_header(frame, header);
        self.draw_stats(frame, stats);
        self.draw_table(frame, table);
        self.draw_details(frame, details);
        self.draw_notice(frame, notice);
        self.draw_help(frame, help);

        if let Some(uuid) = &self.confirmation {
            self.draw_confirmation(frame, area, uuid);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let title = Line::from(vec![
            Span::styled(" mext", Style::default().fg(ACCENT).bold()),
            Span::styled("display ", Style::default().fg(Color::White).bold()),
            Span::styled(
                "external display manager for Apple Silicon",
                Style::default().fg(MUTED),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(title)
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_style(Style::default().fg(Color::DarkGray)),
                )
                .alignment(Alignment::Left),
            area,
        );
    }

    fn draw_stats(&self, frame: &mut Frame, area: Rect) {
        let external = self
            .displays
            .iter()
            .filter(|display| display.is_external())
            .count();
        let enabled = self
            .displays
            .iter()
            .filter(|display| display.is_external() && display.status == DisplayStatus::Enabled)
            .count();
        let disabled = self
            .displays
            .iter()
            .filter(|display| display.status == DisplayStatus::Disabled)
            .count();
        let columns = Layout::horizontal([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);

        stat_card(frame, columns[0], "EXTERNAL", external, ACCENT);
        stat_card(frame, columns[1], "ENABLED", enabled, ENABLED);
        stat_card(frame, columns[2], "DISABLED", disabled, DISABLED);
    }

    fn draw_table(&self, frame: &mut Frame, area: Rect) {
        let rows = self.displays.iter().map(|display| {
            let (status, color) = match display.status {
                DisplayStatus::Enabled => ("● Enabled", ENABLED),
                DisplayStatus::Disabled => ("○ Disabled", DISABLED),
            };
            let role = match (display.builtin, display.main) {
                (true, true) => "Built-in · Main",
                (true, false) => "Built-in",
                (false, true) => "External · Main",
                (false, false) => "External",
            };
            Row::new(vec![
                Cell::from(display.name.clone()),
                Cell::from(status).style(Style::default().fg(color).bold()),
                Cell::from(role),
                Cell::from(display.id.to_string()),
                Cell::from(display.uuid.chars().take(8).collect::<String>()),
            ])
            .style(Style::default().fg(Color::Gray))
            .height(1)
            .bottom_margin(1)
        });
        let header = Row::new(["DISPLAY", "STATUS", "ROLE", "ID", "UUID"])
            .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD))
            .bottom_margin(1);
        let table = Table::new(
            rows,
            [
                Constraint::Min(20),
                Constraint::Length(12),
                Constraint::Length(18),
                Constraint::Length(6),
                Constraint::Length(10),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .title(" Displays ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(34, 48, 65))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("  › ");
        let mut state = TableState::default()
            .with_selected((!self.displays.is_empty()).then_some(self.selected));
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn draw_details(&self, frame: &mut Frame, area: Rect) {
        let content = if let Some(display) = self.selected_display() {
            let resolution = if display.width == 0 {
                "offline".to_owned()
            } else {
                format!("{}×{} px", display.width, display.height)
            };
            vec![
                Line::from(vec![
                    Span::styled("UUID  ", Style::default().fg(MUTED)),
                    Span::raw(&display.uuid),
                ]),
                Line::from(vec![
                    Span::styled("Mode  ", Style::default().fg(MUTED)),
                    Span::raw(resolution),
                    Span::styled("    Hardware  ", Style::default().fg(MUTED)),
                    Span::raw(format!(
                        "vendor {:04X} · model {:04X} · serial {}",
                        display.vendor, display.model, display.serial
                    )),
                ]),
            ]
        } else {
            vec![Line::from("No displays found.")]
        };
        frame.render_widget(
            Paragraph::new(content).block(
                Block::default()
                    .title(" Selected ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
            area,
        );
    }

    fn draw_notice(&self, frame: &mut Frame, area: Rect) {
        let color = if self.notice.error {
            Color::LightRed
        } else {
            ACCENT
        };
        let label = if self.notice.error {
            " ERROR "
        } else {
            " INFO "
        };
        frame.render_widget(
            Paragraph::new(self.notice.text.as_str())
                .style(Style::default().fg(color))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .title(label)
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(color)),
                ),
            area,
        );
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let help = Line::from(vec![
            key("↑/k"),
            Span::raw(" up  "),
            key("↓/j"),
            Span::raw(" down  "),
            key("Enter/Space"),
            Span::raw(" toggle  "),
            key("r"),
            Span::raw(" refresh  "),
            key("q"),
            Span::raw(" quit"),
        ]);
        frame.render_widget(
            Paragraph::new(help)
                .alignment(Alignment::Center)
                .style(Style::default().fg(MUTED)),
            area,
        );
    }

    fn draw_confirmation(&self, frame: &mut Frame, area: Rect, uuid: &str) {
        let display_name = self
            .displays
            .iter()
            .find(|display| display.uuid == uuid)
            .map(|display| display.name.as_str())
            .unwrap_or("this display");
        let popup = centered_rect(62, 9, area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("Disable {display_name}?"),
                    Style::default().fg(Color::White).bold(),
                )),
                Line::from(""),
                Line::from("macOS will stop rendering this display for the current login session."),
                Line::from("The monitor should continue providing power and USB hub access."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("y", Style::default().fg(ENABLED).bold()),
                    Span::raw(" confirm    "),
                    Span::styled("n / Esc", Style::default().fg(DISABLED).bold()),
                    Span::raw(" cancel"),
                ]),
            ])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" Confirm ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Double)
                    .border_style(Style::default().fg(DISABLED)),
            ),
            popup,
        );
    }
}

fn stat_card(frame: &mut Frame, area: Rect, label: &str, value: usize, color: Color) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {value} "), Style::default().fg(color).bold()),
            Span::styled(label, Style::default().fg(MUTED)),
        ]))
        .alignment(Alignment::Center),
        area,
    );
}

fn key(value: &str) -> Span<'_> {
    Span::styled(
        format!(" {value} "),
        Style::default().fg(Color::White).bg(Color::Rgb(47, 53, 64)),
    )
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let [vertical] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [horizontal] = Layout::horizontal([Constraint::Percentage(width)])
        .flex(Flex::Center)
        .areas(vertical);
    horizontal
}
