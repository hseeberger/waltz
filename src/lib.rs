mod actor_context;
mod actor_id;
mod actor_ref;
pub mod terminated;

pub use actor_context::ActorContext;
pub use actor_id::ActorId;
pub use actor_ref::ActorRef;

use async_trait::async_trait;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use tokio::{
    sync::{mpsc, watch as wtch},
    task,
};
use tracing::error;

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

/// Spawn an actor with the given handler and initial state. The returned [ActorRef] can be used
/// to send messages to this actor.
pub fn spawn<M, H, S, I>(handler: H, init: I) -> ActorRef<M>
where
    M: Send + 'static,
    H: Handler<Msg = M, State = S> + Send + 'static,
    S: Send + 'static,
    I: FnOnce(&ActorContext<M>) -> S,
{
    let (terminated_in, terminated_out) = wtch::channel::<ActorId>(ActorId::nil());
    let (mailbox_in, mut mailbox_out) = mpsc::channel::<MsgOrSignal<M>>(42);
    let actor_ref = ActorRef::new(mailbox_in, terminated_out);
    let id = actor_ref.id();
    let ctx = ActorContext::new(actor_ref.clone());
    let initial_state = init(&ctx);
    let mut actor = Actor {
        state: initial_state,
        handler,
    };

    task::spawn(async move {
        while let Some(msg) = mailbox_out.recv().await {
            let receive = actor.handler.receive(&ctx, msg, actor.state);
            let receive = AssertUnwindSafe(receive).catch_unwind();
            match receive.await {
                Ok(StateOrStop::State(state)) => actor.state = state,
                Ok(StateOrStop::Stop) => break,
                Err(e) => {
                    error!("Stopping actor {id}, because handler failed: {e:?}");
                    break;
                }
            }
        }

        if let Err(e) = terminated_in.send(id) {
            error!("Could not send Terminated signal for actor {id}: {e}");
        };
    });

    actor_ref
}

struct Actor<M, H, S>
where
    H: Handler<Msg = M, State = S>,
{
    state: S,
    handler: H,
}

#[cfg(test)]
mod tests {}
