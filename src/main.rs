mod agent;
mod app;
mod history;
mod tools;

use anyhow::{Context as _, Result};
use app::App;
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use std::io::Write;
use tokio::sync::mpsc;

const DEFAULT_MODEL_ID: &str = "openai.gpt-oss-120b-1:0";

#[derive(Parser)]
#[command(name = "asobi", version, about = "A minimal coding agent powered by AWS Bedrock")]
struct Cli {
    /// Bedrock model ID to use
    #[arg(short, long, env = "ASOBI_MODEL", default_value = DEFAULT_MODEL_ID)]
    model: String,

    /// System prompt to use
    #[arg(long, env = "ASOBI_SYSTEM_PROMPT")]
    system_prompt: Option<String>,

    /// Run non-interactively with the given prompt
    #[arg(short, long)]
    prompt: Option<String>,

    /// Run non-interactively with the prompt read from a file ("-" for stdin)
    #[arg(short = 'f', long)]
    prompt_file: Option<String>,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let prompt = cli.resolve_prompt()?;

    if let Some(prompt) = prompt {
        run_non_interactive(cli, &prompt).await
    } else {
        let mut terminal = ratatui::init();
        let result = run_interactive(&mut terminal, cli).await;
        ratatui::restore();
        result
    }
}

async fn run_non_interactive(cli: Cli, prompt: &str) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<agent::AgentEvent>();

    let model_id = cli.model;
    let system_prompt = cli.system_prompt;
    let prompt = prompt.to_string();

    let agent_handle = tokio::spawn(async move {
        let mut agent = agent::Agent::new(model_id, system_prompt).await?;
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

    if has_error {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_interactive(terminal: &mut ratatui::DefaultTerminal, cli: Cli) -> Result<()> {
    let mut app = App::new();
    let mut event_stream = EventStream::new();

    let (user_tx, mut user_rx) = mpsc::unbounded_channel::<String>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<agent::AgentEvent>();

    let model_id = cli.model;
    let system_prompt = cli.system_prompt;
    tokio::spawn(async move {
        let mut agent = match agent::Agent::new(model_id, system_prompt).await {
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
                        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                            app.should_quit = true;
                        } else if app.is_streaming {
                        } else {
                            match key.code {
                                KeyCode::Enter => {
                                    let input = app.take_input();
                                    if input.trim() == "/quit" {
                                        app.should_quit = true;
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
                                KeyCode::Up => app.scroll_up(),
                                KeyCode::Down => app.scroll_down(),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(event) = agent_rx.recv() => {
                app.handle_agent_event(event);
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_with(prompt: Option<&str>, prompt_file: Option<&str>) -> Cli {
        Cli {
            model: DEFAULT_MODEL_ID.to_string(),
            system_prompt: None,
            prompt: prompt.map(|s| s.to_string()),
            prompt_file: prompt_file.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_resolve_prompt_none() {
        let cli = cli_with(None, None);
        assert!(cli.resolve_prompt().unwrap().is_none());
    }

    #[test]
    fn test_resolve_prompt_inline() {
        let cli = cli_with(Some("hello"), None);
        assert_eq!(cli.resolve_prompt().unwrap().unwrap(), "hello");
    }

    #[test]
    fn test_resolve_prompt_file() {
        let dir = std::env::temp_dir().join("asobi_test_prompt");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("prompt.txt");
        std::fs::write(&file, "from file").unwrap();

        let cli = cli_with(None, Some(file.to_str().unwrap()));
        assert_eq!(cli.resolve_prompt().unwrap().unwrap(), "from file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_prompt_file_not_found() {
        let cli = cli_with(None, Some("/nonexistent/path/prompt.txt"));
        assert!(cli.resolve_prompt().is_err());
    }

    #[test]
    fn test_resolve_prompt_inline_takes_precedence() {
        let cli = cli_with(Some("inline"), Some("/some/file"));
        assert_eq!(cli.resolve_prompt().unwrap().unwrap(), "inline");
    }
}
