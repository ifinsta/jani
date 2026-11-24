//! The operator's answer to "may the agent run this?".
//!
//! Smart execution runs what it can classify as safe and asks about the rest.
//! The question goes to whichever interface the operator is using (the web
//! workspace today); with none attached, the command is refused and the model
//! is told to stop and report. The answer can only come from that interface:
//! not from the model, the worktree or the settings files.

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentRequest {
    pub argv: Vec<String>,
    pub reason: String,
    /// The prefix an "always" answer would remember.
    pub suggest: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentAnswer {
    Once,
    /// This command prefix, until the session ends.
    Session,
    /// This command prefix, saved as an operator rule in the state directory.
    Always,
    Deny,
}

pub trait Consent: Send + Sync {
    /// Block until the operator decides. Returning `Deny` on timeout or
    /// cancellation is the safe default.
    fn ask(&self, request: &ConsentRequest) -> ConsentAnswer;
}

pub type SharedConsent = Arc<dyn Consent>;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long a question waits for the operator before it counts as "no".
const WAIT_LIMIT: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct Gate {
    next: u64,
    pending: Option<(u64, ConsentRequest)>,
    answer: Option<ConsentAnswer>,
}

/// A one-question-at-a-time mailbox between the agent thread (which blocks in
/// `ask`) and an interface thread (which shows the question and records the
/// answer). The workspace server owns one.
#[derive(Default)]
pub struct ConsentGate {
    state: Mutex<Gate>,
    changed: Condvar,
    cancelled: AtomicBool,
}

impl ConsentGate {
    pub fn pending(&self) -> Option<(u64, ConsentRequest)> {
        self.state.lock().ok()?.pending.clone()
    }

    /// Record the operator's answer to question `id`. False when that question
    /// is not the one waiting (already answered, timed out, or never asked).
    pub fn answer(&self, id: u64, answer: ConsentAnswer) -> bool {
        let Ok(mut gate) = self.state.lock() else {
            return false;
        };
        if gate.pending.as_ref().map(|(n, _)| *n) != Some(id) || gate.answer.is_some() {
            return false;
        }
        gate.answer = Some(answer);
        self.changed.notify_all();
        true
    }

    /// Cancelling a turn answers any waiting question "no".
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.changed.notify_all();
    }

    pub fn rearm(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

impl Consent for ConsentGate {
    fn ask(&self, request: &ConsentRequest) -> ConsentAnswer {
        let Ok(mut gate) = self.state.lock() else {
            return ConsentAnswer::Deny;
        };
        gate.next += 1;
        let id = gate.next;
        gate.pending = Some((id, request.clone()));
        gate.answer = None;
        let started = Instant::now();
        let result = loop {
            if let Some(answer) = gate.answer.take() {
                break answer;
            }
            if self.cancelled.load(Ordering::SeqCst) || started.elapsed() >= WAIT_LIMIT {
                break ConsentAnswer::Deny;
            }
            gate = match self.changed.wait_timeout(gate, Duration::from_millis(250)) {
                Ok((gate, _)) => gate,
                Err(_) => return ConsentAnswer::Deny,
            };
        };
        gate.pending = None;
        result
    }
}

// ------------------------------------------------------------ line-mode prompt

use std::io::{self, BufRead, IsTerminal, Write};

/// Asks on the terminal, for `--print` and the line-based REPL.
///
/// The answer is read from the controlling terminal itself (`/dev/tty`, or
/// `CONIN$` on Windows) and the question goes to stderr, the way `sudo` and
/// `ssh` ask. That keeps stdout clean for the answer, and it never consumes a
/// line the REPL was about to read as a command or that a script piped in. With
/// no terminal to ask (CI, pipes) it is not offered and commands are refused.
pub struct TerminalConsent;

impl TerminalConsent {
    /// A person is at an interactive terminal who can be asked.
    pub fn available() -> bool {
        io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    fn open_tty() -> io::Result<std::fs::File> {
        #[cfg(windows)]
        {
            std::fs::File::open("CONIN$")
        }
        #[cfg(not(windows))]
        {
            // The controlling terminal, or, when there is none (a pty without a
            // session), stdin reopened: `available` already knows it is a terminal.
            std::fs::File::open("/dev/tty").or_else(|_| std::fs::File::open("/dev/stdin"))
        }
    }
}

impl Consent for TerminalConsent {
    fn ask(&self, request: &ConsentRequest) -> ConsentAnswer {
        let tty = match Self::open_tty() {
            Ok(tty) => tty,
            Err(error) => {
                eprintln!(
                    "jani: cannot read an answer from the terminal ({error}); refusing the command"
                );
                return ConsentAnswer::Deny;
            }
        };
        ask_on(request, &mut io::BufReader::new(tty), &mut io::stderr())
    }
}

/// The question and the parsing of the answer, apart from where they travel.
/// Enter, end of input and anything unrecognised after three tries all mean no.
pub fn ask_on(
    request: &ConsentRequest,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> ConsentAnswer {
    let prefix = request.suggest.join(" ");
    let _ = writeln!(
        out,
        "\njani: needs your OK to run a command\n  {}\n  {}.\n  [y] once   [s] this session (\"{prefix}\")   [a] always (\"{prefix}\"; saves a rule)   [n] deny",
        request.argv.join(" "),
        request.reason,
    );
    for _ in 0..3 {
        let _ = write!(out, "allow? [y/s/a/N] ");
        let _ = out.flush();
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => return ConsentAnswer::Deny,
            Ok(_) => {}
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" | "once" => return ConsentAnswer::Once,
            "s" | "session" => return ConsentAnswer::Session,
            "a" | "always" => return ConsentAnswer::Always,
            "" | "n" | "no" | "deny" => return ConsentAnswer::Deny,
            _ => {
                let _ = writeln!(out, "  answer y, s, a or n");
            }
        }
    }
    ConsentAnswer::Deny
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn request() -> ConsentRequest {
        ConsentRequest {
            argv: vec!["npm".into(), "install".into()],
            reason: "installs".into(),
            suggest: vec!["npm".into(), "install".into()],
        }
    }

    #[test]
    fn the_agent_blocks_until_the_interface_answers_the_question_that_is_waiting() {
        let gate = Arc::new(ConsentGate::default());
        let asker = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.ask(&request()))
        };
        let (id, shown) = loop {
            if let Some(pending) = gate.pending() {
                break pending;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(shown.argv, vec!["npm", "install"]);
        assert!(
            !gate.answer(id + 1, ConsentAnswer::Always),
            "a stale or wrong id answers nothing"
        );
        assert!(gate.answer(id, ConsentAnswer::Session));
        assert!(
            !gate.answer(id, ConsentAnswer::Always),
            "a question is answered once"
        );
        assert_eq!(asker.join().unwrap(), ConsentAnswer::Session);
        assert!(gate.pending().is_none());
    }

    #[test]
    fn cancelling_the_turn_answers_no() {
        let gate = Arc::new(ConsentGate::default());
        let asker = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.ask(&request()))
        };
        while gate.pending().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        gate.cancel();
        assert_eq!(asker.join().unwrap(), ConsentAnswer::Deny);
        gate.rearm();
    }

    fn ask_with(input: &str) -> (ConsentAnswer, String) {
        let mut out = Vec::new();
        let answer = ask_on(&request(), &mut input.as_bytes(), &mut out);
        (answer, String::from_utf8(out).unwrap())
    }

    #[test]
    fn the_line_prompt_shows_the_command_and_reads_one_answer() {
        let (answer, shown) = ask_with("y\n");
        assert_eq!(answer, ConsentAnswer::Once);
        for want in [
            "npm install",
            "installs",
            "[y] once",
            "[s] this session",
            "[a] always",
            "[n] deny",
        ] {
            assert!(shown.contains(want), "{want} missing in {shown}");
        }
        assert_eq!(ask_with("S\n").0, ConsentAnswer::Session);
        assert_eq!(ask_with("always\n").0, ConsentAnswer::Always);
        assert_eq!(ask_with("n\n").0, ConsentAnswer::Deny);
    }

    #[test]
    fn enter_end_of_input_and_noise_all_mean_no() {
        assert_eq!(
            ask_with("\n").0,
            ConsentAnswer::Deny,
            "Enter is the default, and it is no"
        );
        assert_eq!(ask_with("").0, ConsentAnswer::Deny, "end of input is no");
        let (answer, shown) = ask_with("maybe\nperhaps\nsure\ny\n");
        assert_eq!(
            answer,
            ConsentAnswer::Deny,
            "three unrecognised answers give up; the fourth line is never read"
        );
        assert!(shown.contains("answer y, s, a or n"));
        // A typo followed by an answer within three tries still counts.
        assert_eq!(ask_with("yy\ns\n").0, ConsentAnswer::Session);
    }
}
