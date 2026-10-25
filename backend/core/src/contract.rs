//! Generated states and transitions. Contract files are the source of truth.
//! Consumers never hand-copy state names or transition logic.

include!(concat!(env!("OUT_DIR"), "/contract.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_transition_gates() {
        assert!(AgentPhase::Ready.can_transition(AgentPhase::Sleeping));
        assert!(AgentPhase::Sleeping.can_transition(AgentPhase::Ready));
        assert!(!AgentPhase::Quarantined.can_transition(AgentPhase::Ready));
        assert!(!AgentPhase::Running.can_transition(AgentPhase::Sleeping));
    }

    #[test]
    fn alert_receipt_transition_gates() {
        assert!(AlertPhase::Pending.can_transition(AlertPhase::Leased));
        assert!(AlertPhase::Leased.can_transition(AlertPhase::Accepted));
        assert!(AlertPhase::Leased.can_transition(AlertPhase::Pending));
        assert!(AlertPhase::Accepted.can_transition(AlertPhase::Resolved));
        assert!(!AlertPhase::Resolved.can_transition(AlertPhase::Pending));
    }
}
