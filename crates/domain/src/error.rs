use std::fmt;

#[derive(Debug)]
pub enum DomainError {
    InvalidInput(String),
    NotFound(String),
    Conflict(String),
    Invariant(String),
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(m) => write!(f, "invalid input: {m}"),
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::Conflict(m) => write!(f, "conflict: {m}"),
            Self::Invariant(m) => write!(f, "invariant violated: {m}"),
        }
    }
}

impl std::error::Error for DomainError {}
