use crate::agent::Agent;
use crate::config::{Config, JResult};
use crate::skills::Catalog;
use serde_json::Value;
use std::io::{self, BufRead, IsTerminal, Write};

const BANNER: &str = "Jani interactive terminal. Conversation is retained for this task.\n\
Commands: /help /status /skills /inspect /budget /approvals /approval /new /exit\n\
Authority is fixed at startup; restart to change write, command or policy scopes.";

const HELP: &str = "\
/help              Show this help
/status            Show frozen authority, task, model and history size
/skills            List discovered skills
/inspect [RUN_ID]  Show journal events (default: latest turn)
/budget            Show the persisted task budget
/approvals         List approval requests
/approval ID       Attach an approval already decided by a verified approver
/new               Clear conversation history; task and budget continue
/exit, /quit       End the session

Any other input is sent to the model as a task.";

pub(crate) const COMMAND_NAMES: [&str; 10] = [
    "help",
    "status",
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
        "help" | "?" => HELP.to_string(),
        "exit" | "quit" => return Ok((Flow::Exit, String::new())),
        "new" => {
            agent.reset_conversation();
            format!(
                "Conversation cleared. Task {} and its budget continue.",
                agent.task_id()
            )
        }
        "status" => agent.status(),
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
