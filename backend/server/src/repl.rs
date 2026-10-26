use crate::agent::Agent;
use crate::config::{Config, JResult};
use crate::skills::Catalog;
use serde_json::Value;
use std::io::{self, BufRead, IsTerminal, Write};

const BANNER: &str = "Jani interactive terminal. Conversation is retained for this task.\n\
Commands: /help /status /model /skills /inspect /budget /approvals /approval /new /exit\n\
Authority is fixed at startup; model profiles can be switched with /model. Restart to change write, command or policy scopes.";

#[derive(Clone, Copy)]
pub(crate) struct Command {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    pub requires_argument: bool,
}

pub(crate) static COMMANDS: [Command; 11] = [
    Command {
        name: "help",
        usage: "",
        summary: "Show this help",
        requires_argument: false,
    },
    Command {
        name: "status",
        usage: "",
        summary: "Show frozen authority, task, model and history size",
        requires_argument: false,
    },
    Command {
        name: "model",
        usage: "[NAME]",
        summary: "Switch model or settings profile for later turns",
        requires_argument: false,
    },
    Command {
        name: "skills",
        usage: "",
        summary: "List discovered skills",
        requires_argument: false,
    },
    Command {
        name: "inspect",
        usage: "[RUN_ID]",
        summary: "Show journal events (default: latest turn)",
        requires_argument: false,
    },
    Command {
        name: "budget",
        usage: "",
        summary: "Show the persisted task budget",
        requires_argument: false,
    },
    Command {
        name: "approvals",
        usage: "",
        summary: "List approval requests",
        requires_argument: false,
    },
    Command {
        name: "approval",
        usage: "ID",
        summary: "Attach an approval a verified approver already decided",
        requires_argument: true,
    },
    Command {
        name: "new",
        usage: "",
        summary: "Clear conversation history; task and budget continue",
        requires_argument: false,
    },
    Command {
        name: "exit",
        usage: "",
        summary: "End the session",
        requires_argument: false,
    },
    Command {
        name: "quit",
        usage: "",
        summary: "End the session (alias of /exit)",
        requires_argument: false,
    },
];

pub(crate) fn help_text() -> String {
    let mut text = String::new();
    for command in &COMMANDS {
        let usage = if command.usage.is_empty() {
            String::new()
        } else {
            format!(" {}", command.usage)
        };
        let entry = format!("/{}{}", command.name, usage);
        text.push_str(&format!("{entry:<19}{}\n", command.summary));
    }
    text.push_str("\nAny other input is sent to the model as a task.");
    text
}

pub(crate) const COMMAND_NAMES: [&str; 11] = [
    "help",
    "status",
    "model",
    "skills",
    "inspect",
    "budget",
    "approvals",
    "approval",
    "new",
    "exit",
    "quit",
];

pub(crate) enum Flow {
    Continue,
    Exit,
}

pub fn run(cfg: &Config, catalog: &Catalog) -> JResult<()> {
    let stdin = io::stdin();
    let mut agent = Agent::new(cfg, catalog)?;
    if stdin.is_terminal() && io::stdout().is_terminal() {
        return crate::tui::run(&mut agent, cfg, catalog);
    }
    if !stdin.is_terminal() {
        eprintln!(
            "jani: stdin is not a terminal; reading commands until EOF (use --print for a single task)"
        );
    } else {
        eprintln!("jani: stdout is not a terminal; using line mode");
    }
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    run_loop(&mut agent, catalog, stdin.lock(), &mut out, &mut err)
}

fn run_loop<R: BufRead, O: Write, E: Write>(
    agent: &mut Agent<'_>,
    catalog: &Catalog,
    mut input: R,
    out: &mut O,
    err: &mut E,
) -> JResult<()> {
    writeln!(err, "{BANNER}").map_err(io_error)?;
    loop {
        write!(err, "jani> ").map_err(io_error)?;
        err.flush().map_err(io_error)?;
        let mut line = String::new();
        let read = input
            .read_line(&mut line)
            .map_err(|error| format!("cannot read input: {error}"))?;
        if read == 0 {
            writeln!(err).map_err(io_error)?;
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            match execute_command(agent, catalog, line) {
                Ok((Flow::Exit, _)) => break,
                Ok((Flow::Continue, text)) => {
                    if !text.is_empty() {
                        writeln!(out, "{text}").map_err(io_error)?;
                        out.flush().map_err(io_error)?;
                    }
                }
                Err(error) => writeln!(err, "jani: {error}").map_err(io_error)?,
            }
            continue;
        }
        match agent.run_turn(line) {
            Ok(answer) => {
                writeln!(out, "{}", answer.trim_end()).map_err(io_error)?;
                out.flush().map_err(io_error)?;
            }
            Err(error) => {
                writeln!(err, "jani: {error}").map_err(io_error)?;
                writeln!(
                    err,
                    "note: this turn was rolled back and is not in model context"
                )
                .map_err(io_error)?;
            }
        }
        report_pending_approvals(agent, err)?;
    }
    Ok(())
}

pub(crate) fn execute_command(
    agent: &mut Agent<'_>,
    catalog: &Catalog,
    line: &str,
) -> Result<(Flow, String), String> {
    let body = line.trim_start_matches('/');
    let mut parts = body.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_ascii_lowercase();
    let argument = parts.next().unwrap_or("").trim();
    let text = match name.as_str() {
        "help" | "?" => help_text(),
        "exit" | "quit" => return Ok((Flow::Exit, String::new())),
        "new" => {
            agent.reset_conversation();
            format!(
                "Conversation cleared. Task {} and its budget continue.",
                agent.task_id()
            )
        }
        "status" => agent.status(),
        "model" => {
            if argument.is_empty() {
                let mut text = format!("Model: {}", agent.model());
                if let Some(profile) = agent.model_profile() {
                    text.push_str(&format!(" (profile {profile})"));
                }
                if agent.model_choices().is_empty() {
                    text.push_str(
                        "\nNo model profiles in settings; /model MODEL_ID uses the current API and key.",
                    );
                } else {
                    text.push_str("\nAvailable profiles:");
                    for choice in agent.model_choices() {
                        text.push_str(&format!("\n  {:<20} {}", choice.label, choice.model));
                    }
                    text.push_str("\nusage: /model PROFILE|MODEL");
                }
                text
            } else {
                agent.select_model(argument)?
            }
        }
        "skills" => catalog.listing(),
        "budget" => write_json(&agent.budget()?)?,
        "inspect" => {
            let run_id = if argument.is_empty() {
                None
            } else {
                Some(argument)
            };
            write_json(&agent.inspect(run_id)?)?
        }
        "approvals" => write_json(&agent.approvals())?,
        "approval" => {
            if argument.is_empty() {
                return Err("usage: /approval APPROVAL_ID".into());
            }
            agent.attach_approval(argument)?
        }
        other => {
            return Err(format!("unknown command '/{other}'; /help lists commands"));
        }
    };
    Ok((Flow::Continue, text))
}

fn write_json(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| error.to_string())
}

fn report_pending_approvals(agent: &Agent<'_>, err: &mut impl Write) -> Result<(), String> {
    let pending = agent.pending_approvals();
    if pending.is_empty() {
        return Ok(());
    }
    for request in &pending {
        let id = request["approvalId"].as_str().unwrap_or("unknown");
        let capability = request["capability"].as_str().unwrap_or("unknown");
        let resource = request["resource"].as_str().unwrap_or("unknown");
        writeln!(err, "approval required: {id} ({capability} {resource})").map_err(io_error)?;
    }
    writeln!(
        err,
        "A verified approver must decide each request outside this session, then attach it with /approval <id>."
    )
    .map_err(io_error)
}

fn io_error(error: io::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::io::Cursor;

    fn test_agent<'a>(cfg: &'a Config, catalog: &'a Catalog) -> Agent<'a> {
        Agent::new(cfg, catalog).unwrap()
    }

    fn workspace() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "jani-repl-{}-{}",
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
    fn the_command_index_matches_the_names_list() {
        let names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        assert_eq!(names, COMMAND_NAMES.to_vec());
        assert!(COMMANDS
            .iter()
            .any(|command| command.requires_argument && command.name == "approval"));
    }

    #[test]
    fn help_lists_every_command_and_the_task_fallback() {
        let help = help_text();
        for name in COMMAND_NAMES {
            assert!(help.contains(&format!("/{name}")), "{help}");
        }
        assert!(help.contains("/inspect [RUN_ID]"), "{help}");
        assert!(help.contains("/approval ID"), "{help}");
        assert!(
            help.contains("Any other input is sent to the model"),
            "{help}"
        );
    }

    #[test]
    fn slash_commands_do_not_call_the_model() {
        let root = workspace();
        let mut cfg = Config::baseline();
        cfg.root = root.clone();
        cfg.state_dir = root.join("state");
        cfg.tenant = "t1".into();
        cfg.principal = "p1".into();
        cfg.task_id = Some("task-1".into());
        let catalog = Catalog::default();
        let mut agent = test_agent(&cfg, &catalog);

        let mut input = Cursor::new(b"/status\n/new\n/approval missing\n/budget\n/exit\n".to_vec());
        let mut out = Vec::new();
        let mut err = Vec::new();
        run_loop(&mut agent, &catalog, &mut input, &mut out, &mut err).unwrap();

        let out = String::from_utf8(out).unwrap();
        let err = String::from_utf8(err).unwrap();
        assert!(out.contains("Task: task-1"), "{out}");
        assert!(out.contains("Conversation cleared"), "{out}");
        assert!(out.contains("\"taskId\": \"task-1\""), "{out}");
        assert!(err.contains("unknown approval 'missing'"), "{err}");
        assert!(err.contains("jani> "), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_lines_are_ignored_and_eof_exits_cleanly() {
        let root = workspace();
        let mut cfg = Config::baseline();
        cfg.root = root.clone();
        cfg.state_dir = root.join("state");
        cfg.tenant = "t1".into();
        cfg.principal = "p1".into();
        let catalog = Catalog::default();
        let mut agent = test_agent(&cfg, &catalog);

        let mut input = Cursor::new(b"\n\n".to_vec());
        let mut out = Vec::new();
        let mut err = Vec::new();
        run_loop(&mut agent, &catalog, &mut input, &mut out, &mut err).unwrap();
        assert!(out.is_empty());
        assert!(String::from_utf8(err).unwrap().contains("jani> "));
        std::fs::remove_dir_all(root).unwrap();
    }
}
