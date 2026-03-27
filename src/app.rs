use aws_sdk_bedrockruntime::types::{ContentBlock, ConversationRole, Message};

use unicode_width::UnicodeWidthStr;

use crate::agent::AgentEvent;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Chat,
}

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
    autoscroll: bool,
    pub focus: Focus,
    input_history: Vec<String>,
    history_index: Option<usize>,
    saved_input: String,
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
            autoscroll: true,
            focus: Focus::Input,
            input_history: Vec::new(),
            history_index: None,
            saved_input: String::new(),
        }
    }

    pub fn load_history(&mut self, messages: &[Message]) {
        for msg in messages {
            let role = msg.role();
            for block in msg.content() {
                if let ContentBlock::Text(text) = block {
                    match role {
                        ConversationRole::User => {
                            self.chat.push(ChatEntry::User(text.clone()));
                        }
                        ConversationRole::Assistant => {
                            self.chat.push(ChatEntry::AssistantText(text.clone()));
                        }
                        _ => {}
                    }
                }
            }
        }
        self.autoscroll = true;
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
            }
            AgentEvent::ToolCall { name, input } => {
                self.flush_streaming();
                self.chat.push(ChatEntry::ToolCall { name, input });
            }
            AgentEvent::ToolResult { name, output } => {
                self.chat.push(ChatEntry::ToolResult { name, output });
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
        self.history_index = None;
        self.saved_input.clear();
        let input = std::mem::take(&mut self.input);
        if !input.trim().is_empty() {
            self.input_history.push(input.clone());
        }
        input
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
    }

    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => {
                self.saved_input = self.input.clone();
                self.input_history.len() - 1
            }
            Some(0) => return,
            Some(i) => i - 1,
        };
        self.history_index = Some(idx);
        self.input = self.input_history[idx].clone();
        self.cursor_pos = self.input.len();
    }

    pub fn history_next(&mut self) {
        let idx = match self.history_index {
            None => return,
            Some(i) => i,
        };
        if idx + 1 >= self.input_history.len() {
            self.history_index = None;
            self.input = std::mem::take(&mut self.saved_input);
        } else {
            self.history_index = Some(idx + 1);
            self.input = self.input_history[idx + 1].clone();
        }
        self.cursor_pos = self.input.len();
    }

    pub fn on_resize(&mut self) {
        // autoscrollがtrueなら次のrender時に追従する
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Input => Focus::Chat,
            Focus::Chat => Focus::Input,
        };
    }

    pub fn scroll_up(&mut self) {
        self.autoscroll = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(3);
        self.clamp_scroll();
        let max = self.content_height.saturating_sub(self.viewport_height);
        if self.scroll_offset >= max {
            self.autoscroll = true;
        }
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

    if app.autoscroll {
        app.scroll_offset = app.content_height.saturating_sub(app.viewport_height);
    }

    let chat_border = if app.focus == Focus::Chat {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let chat_widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(chat_border)
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
    let input_border = if app.focus == Focus::Input {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let input_widget = Paragraph::new(app.input.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(input_border)
            .title(input_title),
    );
    frame.render_widget(input_widget, input_area);

    if app.focus == Focus::Input {
        let cursor_x = input_area.x + 1 + app.input[..app.cursor_pos].width() as u16;
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

    #[test]
    fn test_input_history_navigation() {
        let mut app = App::new();

        app.input = "first".into();
        app.cursor_pos = 5;
        app.take_input();

        app.input = "second".into();
        app.cursor_pos = 6;
        app.take_input();

        app.input = "third".into();
        app.cursor_pos = 5;
        app.take_input();

        assert_eq!(app.input, "");

        app.history_prev();
        assert_eq!(app.input, "third");
        app.history_prev();
        assert_eq!(app.input, "second");
        app.history_prev();
        assert_eq!(app.input, "first");
        app.history_prev();
        assert_eq!(app.input, "first");

        app.history_next();
        assert_eq!(app.input, "second");
        app.history_next();
        assert_eq!(app.input, "third");
        app.history_next();
        assert_eq!(app.input, "");
        app.history_next();
        assert_eq!(app.input, "");
    }

    #[test]
    fn test_input_history_preserves_draft() {
        let mut app = App::new();

        app.input = "old".into();
        app.cursor_pos = 3;
        app.take_input();

        app.input = "drafting".into();
        app.cursor_pos = 8;

        app.history_prev();
        assert_eq!(app.input, "old");

        app.history_next();
        assert_eq!(app.input, "drafting");
    }

    #[test]
    fn test_input_history_empty() {
        let mut app = App::new();
        app.history_prev();
        assert_eq!(app.input, "");
        app.history_next();
        assert_eq!(app.input, "");
    }

    #[test]
    fn test_take_input_adds_to_history() {
        let mut app = App::new();
        app.input = "hello".into();
        app.cursor_pos = 5;
        app.take_input();
        assert_eq!(app.input_history, vec!["hello"]);
    }

    #[test]
    fn test_take_input_skips_empty() {
        let mut app = App::new();
        app.input = "   ".into();
        app.cursor_pos = 3;
        app.take_input();
        assert!(app.input_history.is_empty());
    }
}
