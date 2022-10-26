use std::fmt::{self, Display};
use uuid::Uuid;

/// A unique actor ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorId(Uuid);

impl ActorId {
    pub(crate) fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub(crate) const fn nil() -> Self {
        Self(Uuid::nil())
    }
}

impl Display for ActorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
