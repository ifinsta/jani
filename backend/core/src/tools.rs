//! Tool-invocation and policy-action vocabulary shared across the workspace.
//!
//! The model catalog, broker, agent display and the policy/mirror matchers all
//! consume the same two vocabularies: the tool *names* sent over the wire and
//! the governed *action* identifiers recorded in decisions and the journal.
//! Keeping them here means adding a tool is a compile-time checklist instead
//! of a scavenger hunt through string literals in five files.

/// Governed action identifiers. `repo.write` is also matched by prefix for
/// scoped paths, so the prefix form lives beside the exact names.
pub mod action {
    pub const SKILL_READ: &str = "skill.read";
    pub const REPO_LIST: &str = "repo.list";
    pub const REPO_READ: &str = "repo.read";
    pub const REPO_WRITE: &str = "repo.write";
    pub const PLUGIN_WRITE: &str = "plugin.write";
    pub const NATIVE_READ: &str = "native.read";
    pub const NATIVE_CONTROL: &str = "native.control";
    pub const COMMAND_EXEC: &str = "command.exec";
    /// Prefix families and the internal model-call action.
    pub const EXTERNAL_PREFIX: &str = "external.";
    pub const CONNECTOR_PREFIX: &str = "connector.";
    pub const MODEL_INVOKE: &str = "model.invoke";

    /// Whole-turn lifecycle actions recorded in the journal: produced by the
    /// agent loop and matched by evidence, recovery and the mirror report.
    pub const AGENT_RUN: &str = "agent.run";
    /// One model invocation inside a turn.
    pub const AGENT_MODEL: &str = "agent.model";
    /// Mirror's second-run comparison grant.
    pub const MIRROR_COMPARE: &str = "mirror.compare";
    /// Recorded when a tool request fails argument validation.
    pub const TOOL_INVALID_ARGUMENTS: &str = "tool.invalid_arguments";
}

/// Tools the model may invoke. Matching on the enum is exhaustive, so a new
/// variant forces every dispatch site to take a position on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    SkillRead,
    List,
    Read,
    Write,
    Command,
    NativeTool,
}

impl Tool {
    pub const ALL: &'static [Self] = &[
        Self::SkillRead,
        Self::List,
        Self::Read,
        Self::Write,
        Self::Command,
        Self::NativeTool,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::SkillRead => "skill_read",
            Self::List => "list",
            Self::Read => "read",
            Self::Write => "write",
            Self::Command => "command",
            Self::NativeTool => "native_tool",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|tool| tool.name() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for tool in Tool::ALL {
            assert_eq!(Tool::from_name(tool.name()), Some(*tool));
        }
        assert_eq!(Tool::from_name("exec"), None);
    }
}
