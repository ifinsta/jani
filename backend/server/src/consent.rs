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

/// A question the agent puts to the operator instead of guessing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    /// Suggested answers. The operator may always answer in their own words.
    pub options: Vec<String>,
}

pub trait Consent: Send + Sync {
    /// Block until the operator decides. Returning `Deny` on timeout or
    /// cancellation is the safe default.
    fn ask(&self, request: &ConsentRequest) -> ConsentAnswer;

    /// Block until the operator answers a question. `None` means nobody can
    /// answer now (declined, cancelled, timed out): the turn then ends with the
    /// question as its result, to be answered later.
    fn ask_question(&self, _question: &Question) -> Option<String> {
        None
    }
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
    question: Option<(u64, Question)>,
    /// `Some(None)` is "declined"; `None` is "not answered yet".
    reply: Option<Option<String>>,
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

    pub fn pending_question(&self) -> Option<(u64, Question)> {
        self.state.lock().ok()?.question.clone()
    }

    /// Record the operator's reply to question `id`: their words, or `None` to decline.
    /// False when that question is not the one waiting.
    pub fn answer_question(&self, id: u64, reply: Option<String>) -> bool {
        let Ok(mut gate) = self.state.lock() else {
            return false;
        };
        if gate.question.as_ref().map(|(n, _)| *n) != Some(id) || gate.reply.is_some() {
            return false;
        }
        gate.reply = Some(reply.filter(|r| !r.trim().is_empty()));
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
    fn ask_question(&self, question: &Question) -> Option<String> {
        let Ok(mut gate) = self.state.lock() else {
            return None;
        };
        gate.next += 1;
        let id = gate.next;
        gate.question = Some((id, question.clone()));
        gate.reply = None;
        let started = Instant::now();
        let result = loop {
            if let Some(reply) = gate.reply.take() {
                break reply;
            }
            if self.cancelled.load(Ordering::SeqCst) || started.elapsed() >= WAIT_LIMIT {
                break None;
            }
            gate = match self.changed.wait_timeout(gate, Duration::from_millis(250)) {
                Ok((gate, _)) => gate,
                Err(_) => return None,
            };
        };
        gate.question = None;
        result
    }

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
/// The question goes to stderr and the answer is read from the terminal itself,
/// the way `sudo` and `ssh` ask, so stdout stays clean for the result and a line
/// the REPL or a script was about to read is never taken as an answer:
///
/// - Unix: `/dev/tty`, or stdin reopened when there is no controlling terminal.
/// - Windows: the console input buffer, `CONIN$`. A terminal that is not a
///   console (mintty and other pty-over-pipe terminals, which Rust still reports
///   as terminals) has no `CONIN$`; there the answer is read from stdin, which is
///   safe because nothing holds the stdin lock during a turn.
///
/// With no terminal to ask (CI, pipes) it is not offered and commands are refused.
pub struct TerminalConsent;

impl TerminalConsent {
    /// A person is at an interactive terminal who can be asked.
    pub fn available() -> bool {
        io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    fn open_tty() -> io::Result<std::fs::File> {
        #[cfg(windows)]
        {
            // The console input buffer wants read and write access, as with any
            // CONIN$ open (rpassword and friends do the same).
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("CONIN$")
        }
        #[cfg(not(windows))]
        {
            std::fs::File::open("/dev/tty").or_else(|_| std::fs::File::open("/dev/stdin"))
        }
    }
}

impl Consent for TerminalConsent {
    fn ask_question(&self, question: &Question) -> Option<String> {
        let mut err = io::stderr();
        match Self::open_tty() {
            Ok(tty) => {
                let mut reader = io::BufReader::new(tty);
                question_with(question, &mut |line| reader.read_line(line), &mut err)
            }
            Err(_) if io::stdin().is_terminal() => {
                question_with(question, &mut |line| io::stdin().read_line(line), &mut err)
            }
            Err(_) => None,
        }
    }

    fn ask(&self, request: &ConsentRequest) -> ConsentAnswer {
        let mut err = io::stderr();
        match Self::open_tty() {
            Ok(tty) => {
                let mut reader = io::BufReader::new(tty);
                ask_with(request, &mut |line| reader.read_line(line), &mut err)
            }
            Err(_) if io::stdin().is_terminal() => {
                ask_with(request, &mut |line| io::stdin().read_line(line), &mut err)
            }
            Err(error) => {
                let _ = writeln!(
                    err,
                    "jani: cannot read an answer from the terminal ({error}); refusing the command"
                );
                ConsentAnswer::Deny
            }
        }
    }
}

/// Put a question and read one answer: a number picks that option, anything else
/// is the operator's own words, and an empty line or end of input leaves it
/// unanswered (the turn then ends with the question).
fn question_with(
    question: &Question,
    read_line: &mut dyn FnMut(&mut String) -> io::Result<usize>,
    out: &mut dyn Write,
) -> Option<String> {
    let _ = writeln!(out, "\njani: the agent has a question\n  {}", question.text);
    for (i, option) in question.options.iter().enumerate() {
        let _ = writeln!(out, "  [{}] {option}", i + 1);
    }
    let _ = write!(
        out,
        "{}",
        if question.options.is_empty() {
            "answer (Enter to leave it for later): "
        } else {
            "number or your own words (Enter to leave it for later): "
        }
    );
    let _ = out.flush();
    let mut line = String::new();
    match read_line(&mut line) {
        Ok(0) | Err(_) => return None,
        Ok(_) => {}
    }
    let text = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{1a}');
    if text.is_empty() {
        return None;
    }
    if let Ok(n) = text.parse::<usize>() {
        if (1..=question.options.len()).contains(&n) {
            return Some(question.options[n - 1].clone());
        }
    }
    Some(text.to_string())
}

/// The question and the parsing of the answer, apart from where they travel.
/// Enter, end of input and anything unrecognised after three tries all mean no.
fn ask_with(
    request: &ConsentRequest,
    read_line: &mut dyn FnMut(&mut String) -> io::Result<usize>,
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
        match read_line(&mut line) {
            Ok(0) | Err(_) => return ConsentAnswer::Deny,
            Ok(_) => {}
        }
        // Console input ends lines with \r\n, and may carry a stray Ctrl+Z (\x1a) at end of input.
        match line
            .trim_matches(|c: char| c.is_whitespace() || c == '\u{1a}')
            .to_ascii_lowercase()
            .as_str()
        {
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
        let mut reader = input.as_bytes();
        let answer = super::ask_with(&request(), &mut |line| reader.read_line(line), &mut out);
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
    fn windows_console_line_endings_and_end_of_input_markers_are_understood() {
        assert_eq!(ask_with("y\r\n").0, ConsentAnswer::Once);
        assert_eq!(ask_with("a\r\n").0, ConsentAnswer::Always);
        assert_eq!(ask_with("\r\n").0, ConsentAnswer::Deny);
        assert_eq!(
            ask_with("\u{1a}\r\n").0,
            ConsentAnswer::Deny,
            "a stray Ctrl+Z is no, not an unrecognised answer to retry"
        );
        assert_eq!(ask_with(" s \r\n").0, ConsentAnswer::Session);
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

    fn question() -> Question {
        Question {
            text: "Which database?".into(),
            options: vec!["sqlite".into(), "postgres".into()],
        }
    }

    fn answer_line(input: &str) -> (Option<String>, String) {
        let mut reader = input.as_bytes();
        let mut out = Vec::new();
        let reply = super::question_with(&question(), &mut |line| reader.read_line(line), &mut out);
        (reply, String::from_utf8(out).unwrap())
    }

    #[test]
    fn the_line_question_takes_a_number_or_free_words_and_leaves_blank_for_later() {
        let (reply, shown) = answer_line("2\n");
        assert_eq!(reply.as_deref(), Some("postgres"));
        assert!(
            shown.contains("Which database?")
                && shown.contains("[1] sqlite")
                && shown.contains("[2] postgres"),
            "{shown}"
        );
        assert_eq!(
            answer_line("sqlite, in memory\n").0.as_deref(),
            Some("sqlite, in memory")
        );
        assert_eq!(
            answer_line("7\n").0.as_deref(),
            Some("7"),
            "a number outside the options is the operator's own words"
        );
        assert_eq!(
            answer_line("1\r\n").0.as_deref(),
            Some("sqlite"),
            "console line endings"
        );
        assert_eq!(answer_line("\n").0, None);
        assert_eq!(answer_line("").0, None, "end of input leaves it for later");
    }

    #[test]
    fn the_gate_carries_a_question_to_the_interface_and_the_reply_back() {
        let gate = Arc::new(ConsentGate::default());
        let asker = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.ask_question(&question()))
        };
        let (id, shown) = loop {
            if let Some(pending) = gate.pending_question() {
                break pending;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(shown, question());
        assert!(!gate.answer_question(id + 1, Some("x".into())));
        assert!(gate.answer_question(id, Some("  sqlite ".into())));
        assert!(
            !gate.answer_question(id, Some("again".into())),
            "answered once"
        );
        assert_eq!(asker.join().unwrap().as_deref(), Some("  sqlite "));
        assert!(gate.pending_question().is_none());

        // Declining, and a blank reply, both come back as "not answered".
        for reply in [None, Some("   ".to_string())] {
            let asker = {
                let gate = gate.clone();
                std::thread::spawn(move || gate.ask_question(&question()))
            };
            let (id, _) = loop {
                if let Some(pending) = gate.pending_question() {
                    break pending;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(gate.answer_question(id, reply));
            assert_eq!(asker.join().unwrap(), None);
        }
    }

    #[test]
    fn cancelling_leaves_a_waiting_question_unanswered() {
        let gate = Arc::new(ConsentGate::default());
        let asker = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.ask_question(&question()))
        };
        while gate.pending_question().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        gate.cancel();
        assert_eq!(asker.join().unwrap(), None);
        gate.rearm();
    }
}
