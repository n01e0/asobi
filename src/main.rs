mod agent;
mod app;
mod config;
mod history;
mod tools;

use anyhow::{Context as _, Result};
use app::App;
use clap::Parser;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::StreamExt;
use std::io::Write;
use tokio::sync::mpsc;

const DEFAULT_MODEL_ID: &str = "openai.gpt-oss-120b-1:0";

#[derive(Parser)]
#[command(name = "asobi", version, about = "A minimal coding agent powered by AWS Bedrock")]
struct Cli {
    /// Bedrock model ID or ARN to use
    #[arg(short, long, env = "ASOBI_MODEL")]
    model: Option<String>,

    /// AWS region for ARN resolution
    #[arg(long, env = "AWS_REGION")]
    region: Option<String>,

    /// System prompt to use
    #[arg(long, env = "ASOBI_SYSTEM_PROMPT")]
    system_prompt: Option<String>,

    /// Run non-interactively with the given prompt
    #[arg(short, long)]
    prompt: Option<String>,

    /// Run non-interactively with the prompt read from a file ("-" for stdin)
    #[arg(short = 'f', long)]
    prompt_file: Option<String>,

    /// Continue the most recent session
    #[arg(short = 'c', long)]
    r#continue: bool,

    /// Restore a specific session by ID
    #[arg(long)]
    restore: Option<String>,
}

struct ResolvedConfig {
    model: String,
    region: Option<String>,
    system_prompt: Option<String>,
}

impl ResolvedConfig {
    fn from_cli_and_config(cli: &Cli, cfg: &config::Config) -> Self {
        Self {
            model: cli
                .model
                .clone()
                .or_else(|| cfg.model.clone())
                .unwrap_or_else(|| DEFAULT_MODEL_ID.to_string()),
            region: cli.region.clone().or_else(|| cfg.region.clone()),
            system_prompt: cli
                .system_prompt
                .clone()
                .or_else(|| cfg.system_prompt.clone()),
        }
    }
}

impl Cli {
    fn resolve_prompt(&self) -> Result<Option<String>> {
        if let Some(ref prompt) = self.prompt {
            return Ok(Some(prompt.clone()));
        }
        if let Some(ref path) = self.prompt_file {
            let content = if path == "-" {
                std::io::read_to_string(std::io::stdin())
                    .context("failed to read prompt from stdin")?
            } else {
                std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read prompt file: {path}"))?
            };
            return Ok(Some(content));
        }
        Ok(None)
    }

    fn resolve_session(&self) -> Result<(String, bool)> {
        if let Some(ref id) = self.restore {
            return Ok((id.clone(), true));
        }
        if self.r#continue {
            let id = history::latest_session_id()?
                .context("no previous session found")?;
            return Ok((id, true));
        }
        Ok((history::new_session_id(), false))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load();
    let resolved = ResolvedConfig::from_cli_and_config(&cli, &cfg);
    let prompt = cli.resolve_prompt()?;
    let (session_id, restore) = cli.resolve_session()?;

    if let Some(prompt) = prompt {
        run_non_interactive(resolved, &prompt, session_id, restore).await
    } else {
        let mut terminal = ratatui::init();
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
        let result = run_interactive(&mut terminal, resolved, &session_id, restore).await;
        crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture)?;
        ratatui::restore();
        eprintln!("\nTo resume this session:\n  asobi --restore {session_id}");
        result
    }
}

async fn run_non_interactive(
    resolved: ResolvedConfig,
    prompt: &str,
    session_id: String,
    restore: bool,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<agent::AgentEvent>();

    let model_id = resolved.model;
    let region = resolved.region;
    let system_prompt = resolved.system_prompt;
    let prompt = prompt.to_string();
    let sid = session_id.clone();

    let agent_handle = tokio::spawn(async move {
        let mut agent = agent::Agent::new(model_id, region, system_prompt, sid, restore).await?;
        agent.send(&prompt, tx).await;
        Ok::<_, anyhow::Error>(())
    });

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut has_error = false;

    while let Some(event) = rx.recv().await {
        match event {
            agent::AgentEvent::Text(text) => {
                write!(stdout, "{text}")?;
                stdout.flush()?;
            }
            agent::AgentEvent::ToolCall { name, input } => {
                writeln!(stderr, "[tool] {name}: {input}")?;
            }
            agent::AgentEvent::ToolResult { name, output } => {
                let preview: String = output.chars().take(200).collect();
                writeln!(stderr, "[result] {name}: {preview}")?;
            }
            agent::AgentEvent::Error(msg) => {
                writeln!(stderr, "[error] {msg}")?;
                has_error = true;
            }
            agent::AgentEvent::TurnEnd => break,
        }
    }
    writeln!(stdout)?;

    agent_handle.await??;

    eprintln!("\nTo resume this session:\n  asobi --restore {session_id}");

    if has_error {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_interactive(
    terminal: &mut ratatui::DefaultTerminal,
    resolved: ResolvedConfig,
    session_id: &str,
    restore: bool,
) -> Result<()> {
    let mut app = App::new();
    let mut event_stream = EventStream::new();

    if restore
        && let Ok(messages) = history::load(session_id).await
    {
        app.load_history(&messages);
    }

    let (user_tx, mut user_rx) = mpsc::unbounded_channel::<String>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<agent::AgentEvent>();

    let model_id = resolved.model;
    let region = resolved.region;
    let system_prompt = resolved.system_prompt;
    let sid = session_id.to_string();
    tokio::spawn(async move {
        let mut agent = match agent::Agent::new(model_id, region, system_prompt, sid, restore).await {
            Ok(a) => a,
            Err(e) => {
                let _ = agent_tx.send(agent::AgentEvent::Error(format!("{e:#}")));
                return;
            }
        };

        while let Some(input) = user_rx.recv().await {
            agent.send(&input, agent_tx.clone()).await;
        }
    });

    loop {
        terminal.draw(|frame| app::render(&mut app, frame))?;

        tokio::select! {
            Some(Ok(event)) = event_stream.next() => {
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                        if ctrl && (key.code == KeyCode::Char('c') || key.code == KeyCode::Char('d')) {
                            if app.request_quit() {
                                break;
                            }
                        } else {
                            app.reset_quit_pending();

                            match key.code {
                                KeyCode::Tab => app.toggle_focus(),
                                KeyCode::Esc => app.focus = app::Focus::Input,
                                _ if app.focus == app::Focus::Chat => {
                                    match key.code {
                                        KeyCode::Up => app.scroll_up(),
                                        KeyCode::Down => app.scroll_down(),
                                        KeyCode::PageUp => app.scroll_up(),
                                        KeyCode::PageDown => app.scroll_down(),
                                        _ => {}
                                    }
                                }
                                _ if ctrl => {
                                    match key.code {
                                        KeyCode::Char('h') => app.delete_char(),
                                        KeyCode::Char('u') => app.clear_input(),
                                        _ => {}
                                    }
                                }
                                _ => {
                                    match key.code {
                                        KeyCode::Enter => {
                                            let input = app.take_input();
                                            if input.trim() == "/quit" {
                                                break;
                                            } else if !input.trim().is_empty() {
                                                app.chat.push(app::ChatEntry::User(input.clone()));
                                                app.is_streaming = true;
                                                let _ = user_tx.send(input);
                                            }
                                        }
                                        KeyCode::Char(c) => app.insert_char(c),
                                        KeyCode::Backspace => app.delete_char(),
                                        KeyCode::Left => app.move_cursor_left(),
                                        KeyCode::Right => app.move_cursor_right(),
                                        KeyCode::Up => app.history_prev(),
                                        KeyCode::Down => app.history_next(),
                                        KeyCode::PageUp => app.scroll_up(),
                                        KeyCode::PageDown => app.scroll_down(),
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Event::Mouse(mouse) => {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => app.scroll_up(),
                            MouseEventKind::ScrollDown => app.scroll_down(),
                            _ => {}
                        }
                    }
                    Event::Resize(_, _) => {
                        app.on_resize();
                    }
                    _ => {}
                }
            }
            Some(event) = agent_rx.recv() => {
                app.handle_agent_event(event);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_base() -> Cli {
        Cli {
            model: None,
            region: None,
            system_prompt: None,
            prompt: None,
            prompt_file: None,
            r#continue: false,
            restore: None,
        }
    }

    #[test]
    fn test_resolve_prompt_none() {
        let cli = cli_base();
        assert!(cli.resolve_prompt().unwrap().is_none());
    }

    #[test]
    fn test_resolve_prompt_inline() {
        let mut cli = cli_base();
        cli.prompt = Some("hello".to_string());
        assert_eq!(cli.resolve_prompt().unwrap().unwrap(), "hello");
    }

    #[test]
    fn test_resolve_prompt_file() {
        let dir = std::env::temp_dir().join("asobi_test_prompt");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("prompt.txt");
        std::fs::write(&file, "from file").unwrap();

        let mut cli = cli_base();
        cli.prompt_file = Some(file.to_str().unwrap().to_string());
        assert_eq!(cli.resolve_prompt().unwrap().unwrap(), "from file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_prompt_file_not_found() {
        let mut cli = cli_base();
        cli.prompt_file = Some("/nonexistent/path/prompt.txt".to_string());
        assert!(cli.resolve_prompt().is_err());
    }

    #[test]
    fn test_resolve_prompt_inline_takes_precedence() {
        let mut cli = cli_base();
        cli.prompt = Some("inline".to_string());
        cli.prompt_file = Some("/some/file".to_string());
        assert_eq!(cli.resolve_prompt().unwrap().unwrap(), "inline");
    }

    #[test]
    fn test_resolve_session_new() {
        let cli = cli_base();
        let (id, restore) = cli.resolve_session().unwrap();
        assert!(!restore);
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_resolve_session_restore() {
        let mut cli = cli_base();
        cli.restore = Some("abc-123".to_string());
        let (id, restore) = cli.resolve_session().unwrap();
        assert!(restore);
        assert_eq!(id, "abc-123");
    }
}
