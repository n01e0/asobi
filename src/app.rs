use crate::agent::AgentEvent;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

#[derive(Debug, Clone)]
pub enum ChatEntry {
    User(String),
    AssistantText(String),
    ToolCall { name: String, input: String },
    ToolResult { name: String, output: String },
    Error(String),
}

pub struct App {
    pub input: String,
    pub cursor_pos: usize,
    pub chat: Vec<ChatEntry>,
    pub streaming_text: String,
    pub is_streaming: bool,
    pub scroll_offset: u16,
    pub content_height: u16,
    pub viewport_height: u16,
    quit_pending: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
            chat: Vec::new(),
            streaming_text: String::new(),
            is_streaming: false,
            scroll_offset: 0,
            content_height: 0,
            viewport_height: 0,
            quit_pending: false,
        }
    }

    pub fn request_quit(&mut self) -> bool {
        if self.quit_pending {
            return true;
        }
        self.quit_pending = true;
        false
    }

    pub fn reset_quit_pending(&mut self) {
        self.quit_pending = false;
    }

    pub fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Text(text) => {
                self.streaming_text.push_str(&text);
                self.scroll_to_bottom();
            }
            AgentEvent::ToolCall { name, input } => {
                self.flush_streaming();
                self.chat.push(ChatEntry::ToolCall { name, input });
                self.scroll_to_bottom();
            }
            AgentEvent::ToolResult { name, output } => {
                self.chat.push(ChatEntry::ToolResult { name, output });
                self.scroll_to_bottom();
            }
            AgentEvent::TurnEnd => {
                self.flush_streaming();
                self.is_streaming = false;
            }
            AgentEvent::Error(msg) => {
                self.flush_streaming();
                self.chat.push(ChatEntry::Error(msg));
                self.is_streaming = false;
            }
        }
    }

    fn flush_streaming(&mut self) {
        if !self.streaming_text.is_empty() {
            let text = std::mem::take(&mut self.streaming_text);
            self.chat.push(ChatEntry::AssistantText(text));
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    pub fn delete_char(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.replace_range(prev..self.cursor_pos, "");
            self.cursor_pos = prev;
        }
    }

    pub fn take_input(&mut self) -> String {
        self.cursor_pos = 0;
        std::mem::take(&mut self.input)
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(3);
        self.clamp_scroll();
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.content_height.saturating_sub(self.viewport_height);
    }

    fn clamp_scroll(&mut self) {
        let max = self.content_height.saturating_sub(self.viewport_height);
        if self.scroll_offset > max {
            self.scroll_offset = max;
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.input[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos = self.input[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_pos + i)
                .unwrap_or(self.input.len());
        }
    }
}

pub fn render(app: &mut App, frame: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(frame.area());

    let chat_area = chunks[0];
    let input_area = chunks[1];
    app.viewport_height = chat_area.height.saturating_sub(2);

    let mut lines: Vec<Line> = Vec::new();

    for entry in &app.chat {
        match entry {
            ChatEntry::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("You: ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::raw(text),
                ]));
            }
            ChatEntry::AssistantText(text) => {
                for (i, line) in text.lines().enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "Agent: ",
                                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                            ),
                            Span::raw(line),
                        ]));
                    } else {
                        lines.push(Line::from(format!("       {line}")));
                    }
                }
            }
            ChatEntry::ToolCall { name, input } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  [{name}] "),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate(input, 80),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            ChatEntry::ToolResult { name, output } => {
                let preview = truncate(output, 120);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  [{name} result] "),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(preview, Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatEntry::Error(msg) => {
                lines.push(Line::from(Span::styled(
                    format!("Error: {msg}"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
    }

    if !app.streaming_text.is_empty() {
        for (i, line) in app.streaming_text.lines().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(
                        "Agent: ",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(line),
                ]));
            } else {
                lines.push(Line::from(format!("       {line}")));
            }
        }
        lines.push(Line::from(Span::styled("▌", Style::default().fg(Color::Cyan))));
    }

    app.content_height = lines.len() as u16;

    let chat_widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" asobi "),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));
    frame.render_widget(chat_widget, chat_area);

    let input_title = if app.quit_pending {
        " Press Ctrl+C/Ctrl+D again to quit "
    } else if app.is_streaming {
        " waiting... "
    } else {
        " > "
    };
    let input_widget = Paragraph::new(app.input.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(input_title),
    );
    frame.render_widget(input_widget, input_area);

    if !app.is_streaming {
        let cursor_x = input_area.x + 1 + app.input[..app.cursor_pos].chars().count() as u16;
        let cursor_y = input_area.y + 1;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn truncate(s: &str, max: usize) -> String {
    let oneline = s.replace('\n', " ");
    if oneline.len() <= max {
        oneline
    } else {
        format!("{}...", &oneline[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_delete_char() {
        let mut app = App::new();
        app.insert_char('a');
        app.insert_char('b');
        app.insert_char('c');
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor_pos, 3);

        app.delete_char();
        assert_eq!(app.input, "ab");
        assert_eq!(app.cursor_pos, 2);
    }

    #[test]
    fn test_insert_multibyte() {
        let mut app = App::new();
        app.insert_char('あ');
        app.insert_char('い');
        assert_eq!(app.input, "あい");
        app.delete_char();
        assert_eq!(app.input, "あ");
    }

    #[test]
    fn test_take_input() {
        let mut app = App::new();
        app.insert_char('x');
        let taken = app.take_input();
        assert_eq!(taken, "x");
        assert_eq!(app.input, "");
        assert_eq!(app.cursor_pos, 0);
    }

    #[test]
    fn test_cursor_movement() {
        let mut app = App::new();
        app.insert_char('a');
        app.insert_char('b');
        app.insert_char('c');
        assert_eq!(app.cursor_pos, 3);

        app.move_cursor_left();
        assert_eq!(app.cursor_pos, 2);
        app.move_cursor_left();
        assert_eq!(app.cursor_pos, 1);
        app.move_cursor_right();
        assert_eq!(app.cursor_pos, 2);
    }

    #[test]
    fn test_handle_agent_event_text() {
        let mut app = App::new();
        app.is_streaming = true;
        app.handle_agent_event(AgentEvent::Text("hello ".into()));
        app.handle_agent_event(AgentEvent::Text("world".into()));
        assert_eq!(app.streaming_text, "hello world");

        app.handle_agent_event(AgentEvent::TurnEnd);
        assert!(app.streaming_text.is_empty());
        assert_eq!(app.chat.len(), 1);
        assert!(matches!(&app.chat[0], ChatEntry::AssistantText(t) if t == "hello world"));
        assert!(!app.is_streaming);
    }

    #[test]
    fn test_handle_agent_event_tool() {
        let mut app = App::new();
        app.is_streaming = true;
        app.handle_agent_event(AgentEvent::ToolCall {
            name: "read_file".into(),
            input: r#"{"path":"/tmp"}"#.into(),
        });
        app.handle_agent_event(AgentEvent::ToolResult {
            name: "read_file".into(),
            output: "contents".into(),
        });
        assert_eq!(app.chat.len(), 2);
        assert!(matches!(&app.chat[0], ChatEntry::ToolCall { name, .. } if name == "read_file"));
    }

    #[test]
    fn test_handle_agent_event_error() {
        let mut app = App::new();
        app.is_streaming = true;
        app.handle_agent_event(AgentEvent::Error("something went wrong".into()));
        assert!(!app.is_streaming);
        assert!(matches!(&app.chat[0], ChatEntry::Error(msg) if msg.contains("something went wrong")));
    }

    #[test]
    fn test_scroll() {
        let mut app = App::new();
        app.content_height = 100;
        app.viewport_height = 20;
        app.scroll_offset = 50;

        app.scroll_up();
        assert_eq!(app.scroll_offset, 47);

        app.scroll_down();
        assert_eq!(app.scroll_offset, 50);
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let result = truncate("hello world this is long", 10);
        assert_eq!(result, "hello worl...");
    }

    #[test]
    fn test_truncate_newlines() {
        let result = truncate("line1\nline2\nline3", 100);
        assert_eq!(result, "line1 line2 line3");
    }

    #[test]
    fn test_request_quit_double_press() {
        let mut app = App::new();
        assert!(!app.request_quit());
        assert!(app.quit_pending);
        assert!(app.request_quit());
    }

    #[test]
    fn test_reset_quit_pending() {
        let mut app = App::new();
        app.request_quit();
        assert!(app.quit_pending);
        app.reset_quit_pending();
        assert!(!app.quit_pending);
        assert!(!app.request_quit());
    }

    #[test]
    fn test_clear_input() {
        let mut app = App::new();
        app.insert_char('a');
        app.insert_char('b');
        app.insert_char('c');
        app.clear_input();
        assert_eq!(app.input, "");
        assert_eq!(app.cursor_pos, 0);
    }
}
