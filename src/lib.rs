mod actor_context;
mod actor_id;
mod actor_ref;
mod actor_system;
mod config;

pub use actor_context::ActorContext;
pub use actor_id::ActorId;
pub use actor_ref::ActorRef;
pub use actor_system::ActorSystem;
pub use config::{Config, CONFIG};

use async_trait::async_trait;

/// A stateful handler for messages or signals received by an actor.
#[async_trait]
pub trait Handler {
    /// The type of the received messages.
    type Msg;

    /// The type of the state. For stateless handlers use `()` or similar.
    type State;

    /// Receive a message or signal, apply it to the current state and return the new state.
    async fn receive(
        &mut self,
        ctx: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State>;
}

/// Handlers can receive messages or signals. Messages are represented via the `Msg` variant and
/// signals as the other variants.
pub enum MsgOrSignal<M> {
    Msg(M),
    Terminated(ActorId),
}

/// Type which cannot be instantiated to  be used for actors which don't react to messages.
#[derive(Clone, Copy)]
pub enum NotUsed {}

/// Handlers either return the new state or their intent to stop. State is represented via the
/// `State` variant, stopping via the `Stop` variant.
pub enum StateOrStop<S> {
    State(S),
    Stop,
}

#[cfg(test)]
mod tests {}
