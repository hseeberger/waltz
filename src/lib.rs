pub mod watch;

use async_trait::async_trait;
use core::fmt::Debug;
use futures::FutureExt;
use std::{
    fmt::{self, Display},
    panic::AssertUnwindSafe,
};
use tokio::{
    sync::{mpsc, watch as wtch},
    task,
};
use tracing::{debug, error, warn};
use uuid::{Timestamp, Uuid};

/// A stateful handler for received messages or signals.
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
#[derive(Debug)]
pub enum MsgOrSignal<M> {
    Msg(M),
    Terminated(ActorId),
}

/// Type which cannot be instantiated to  be used for actors which don't react to messages.
#[derive(Debug, Clone, Copy)]
pub enum NotUsed {}

/// Handlers either return the new state or their intent to stop. State is represented via the
/// `State` variant, stopping via the `Stop` variant.
pub enum StateOrStop<S> {
    State(S),
    Stop,
}

/// Contextual methods for a given actor, provided as handler parameter.
pub struct ActorContext<M> {
    self_ref: ActorRef<M>,
}

impl<M: Send + 'static + Debug> ActorContext<M> {
    /// The reference for the actor itself.
    pub fn self_ref(&self) -> &ActorRef<M> {
        &self.self_ref
    }

    /// Watch another actor, i.e. receive a [MsgOrSignal::Terminated] signal if that actor has
    /// stopped.
    pub fn watch<N: Debug>(&self, other: &ActorRef<N>) {
        let self_ref = self.self_ref().clone();
        let mut terminated = other.terminated.clone();
        task::spawn(async move {
            match terminated.changed().await {
                Ok(_) => {
                    let id = *terminated.borrow();
                    debug!("Watched actor {id} terminated");
                    if let Err(e) = self_ref.mailbox.send(MsgOrSignal::Terminated(id)).await {
                        warn!("Cannot send Terminated signal to actor {}: {e}", id);
                    }
                }
                Err(e) => warn!("Cannot receive Terminated signal: {e}"),
            }
        });
    }

    fn new(self_ref: ActorRef<M>) -> Self {
        Self { self_ref }
    }
}

/// A shareable reference to an actor, allowing to access its ID and send messages to it.
pub struct ActorRef<M> {
    id: ActorId,
    mailbox: mpsc::Sender<MsgOrSignal<M>>,
    terminated: wtch::Receiver<ActorId>,
}

impl<M: Debug> ActorRef<M> {
    /// The actor's ID.
    pub fn id(&self) -> ActorId {
        self.id
    }

    /// Send a message to the actor represented by this `ActorRef`. This might fail if the actor has
    /// stopped, resulting in a log event at warn level.
    pub async fn tell(&self, msg: M) {
        if let Err(e) = self.mailbox.send(MsgOrSignal::Msg(msg)).await {
            warn!("Cannot send message to actor {}: {e}", self.id);
        }
    }

    fn new(mailbox: mpsc::Sender<MsgOrSignal<M>>, terminated: wtch::Receiver<ActorId>) -> Self {
        Self {
            id: ActorId::new(),
            mailbox,
            terminated,
        }
    }
}

impl<M> AsRef<ActorRef<M>> for ActorRef<M> {
    fn as_ref(&self) -> &ActorRef<M> {
        self
    }
}

impl<M> Debug for ActorRef<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActorRef").field("id", &self.id).finish()
    }
}

impl<M> Clone for ActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            mailbox: self.mailbox.clone(),
            terminated: self.terminated.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A unique actor ID.
pub struct ActorId(Uuid);

impl ActorId {
    fn new() -> Self {
        Self(Uuid::new_v7(Timestamp::now()))
    }

    const fn nil() -> Self {
        Self(Uuid::nil())
    }
}

impl Display for ActorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Spawn an actor with the given handler and initial state. The returned [ActorRef] can be used
/// to send messages to this actor.
pub fn spawn<M, H, S, I>(handler: H, init: I) -> ActorRef<M>
where
    M: Send + 'static + Debug,
    H: Handler<Msg = M, State = S> + Send + 'static,
    S: Send + 'static,
    I: FnOnce(&ActorContext<M>) -> S,
{
    let (terminated_in, terminated_out) = wtch::channel::<ActorId>(ActorId::nil());
    let (mailbox_in, mut mailbox_out) = mpsc::channel::<MsgOrSignal<M>>(42);
    let actor_ref = ActorRef::new(mailbox_in, terminated_out);
    let id = actor_ref.id;
    let ctx = ActorContext::new(actor_ref.clone());
    let initial_state = init(&ctx);
    let mut actor = Actor::new(initial_state, handler);

    task::spawn(async move {
        while let Some(msg) = mailbox_out.recv().await {
            debug!("Actor {id} received {msg:?}");
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
        debug!("Actor {id} stopped");

        if let Err(e) = terminated_in.send(id) {
            warn!("Could not send Terminated signal for actor {id}: {e}");
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

impl<M, H, S> Actor<M, H, S>
where
    H: Handler<Msg = M, State = S>,
{
    fn new(initial_state: S, handler: H) -> Self {
        Self {
            state: initial_state,
            handler,
        }
    }
}

#[cfg(test)]
mod tests {}
