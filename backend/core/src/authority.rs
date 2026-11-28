//! Validity of the authority already verified for an operation.
use crate::types::now_unix;
use std::fmt;

#[derive(Clone, Copy, Debug, Default)]
pub struct AuthorityWindow {
    identity_expires_at: Option<u64>,
    policy_expires_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityExpired {
    Identity,
    Policy,
}

impl AuthorityExpired {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Identity => "identity_expired",
            Self::Policy => "policy_expired",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Identity => "identity assertion has expired",
            Self::Policy => "policy bundle has expired",
        }
    }
}

impl fmt::Display for AuthorityExpired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl AuthorityWindow {
    pub fn new(identity_expires_at: Option<u64>, policy_expires_at: Option<u64>) -> Self {
        Self {
            identity_expires_at,
            policy_expires_at,
        }
    }

    pub fn require_current(self) -> Result<(), AuthorityExpired> {
        self.require_at(now_unix())
    }

    fn require_at(self, now: u64) -> Result<(), AuthorityExpired> {
        if self.identity_expires_at.is_some_and(|expiry| now >= expiry) {
            return Err(AuthorityExpired::Identity);
        }
        if self.policy_expires_at.is_some_and(|expiry| now >= expiry) {
            return Err(AuthorityExpired::Policy);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_authority_is_exclusive_at_expiration() {
        assert_eq!(AuthorityWindow::new(Some(10), None).require_at(9), Ok(()));
        assert_eq!(
            AuthorityWindow::new(Some(10), None).require_at(10),
            Err(AuthorityExpired::Identity)
        );
        assert_eq!(
            AuthorityWindow::new(Some(20), Some(10)).require_at(10),
            Err(AuthorityExpired::Policy)
        );
        assert_eq!(
            AuthorityWindow::new(None, None).require_at(u64::MAX),
            Ok(())
        );
    }
}
