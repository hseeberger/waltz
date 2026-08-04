use derive_more::{Display, Into};
use uuid::Uuid;

/// A unique actor ID: a UUID v7, so IDs are time-ordered by creation.
#[derive(Debug, Display, Clone, Copy, Into, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorId(Uuid);

impl ActorId {
    pub(crate) fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

#[cfg(test)]
mod tests {
    use crate::ActorId;
    use uuid::Uuid;

    /// An ID converts into the UUID it wraps, and that UUID is what its `Display` shows.
    #[test]
    fn an_id_converts_into_its_uuid() {
        let actor_id = ActorId::new();

        assert_eq!(Uuid::from(actor_id).to_string(), actor_id.to_string());
    }
}
