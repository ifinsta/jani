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
}
