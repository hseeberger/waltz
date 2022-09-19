use crate::{
    init, spawn, ActorContext, ActorId, ActorRef, Handler, MsgOrSignal, NotUsed, StateOrStop,
};
use async_trait::async_trait;
use std::fmt::Debug;
use thiserror::Error;
use tokio::sync::oneshot::{self, Sender};

#[derive(Debug, Error)]
/// Errors for this module.
pub enum Error {
    /// Unexpected failure during watching an actor.
    #[error("unexpected failure during watching actor {id}")]
    Watch {
        id: ActorId,
        source: oneshot::error::RecvError,
    },
}

/// Watch the given actor for termination: the returned future completes once the actor has
/// terminated.
pub async fn terminated<M>(actor_ref: ActorRef<M>) -> Result<(), Error> {
    let id = actor_ref.id();
    let (terminated_sender, terminated_receiver) = oneshot::channel::<()>();
    let _ = spawn(
        Watcher,
        init!(ctx, {
            ctx.watch(actor_ref);
            terminated_sender
        }),
    )
    .await;
    terminated_receiver
        .await
        .map_err(|source| Error::Watch { id, source })
}

struct Watcher;

#[async_trait]
impl Handler for Watcher {
    type Msg = NotUsed;

    type State = Sender<()>;

    async fn receive(
        &mut self,
        _ctx: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        match msg {
            // Cannot happen, because Msg = NotUsed!
            MsgOrSignal::Msg(_) => StateOrStop::State(state),
            MsgOrSignal::Terminated(_) => {
                // We do not care if the receiver has been dropped
                let _ = state.send(());
                StateOrStop::Stop
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{
        task,
        time::{self, timeout},
    };

    struct Stopper;

    #[async_trait]
    impl Handler for Stopper {
        type Msg = ();

        type State = ();

        async fn receive(
            &mut self,
            _ctx: &ActorContext<Self::Msg>,
            msg: MsgOrSignal<Self::Msg>,
            state: Self::State,
        ) -> StateOrStop<Self::State> {
            match msg {
                MsgOrSignal::Msg(_) => StateOrStop::Stop,
                _ => StateOrStop::State(state),
            }
        }
    }

    #[tokio::test]
    async fn test_watch_before_stop() {
        let stopper = spawn(Stopper, |ctx| async { (ctx, ()) }).await;
        let stopper_2 = stopper.clone();
        let watching = task::spawn(async move { terminated(stopper_2).await });
        assert!(!watching.is_finished());

        stopper.tell(()).await;
        let result = timeout(Duration::from_millis(100), watching).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_watch_after_stop() {
        let stopper = spawn(Stopper, |ctx| async { (ctx, ()) }).await;
        stopper.tell(()).await;

        time::sleep(Duration::from_millis(100)).await;

        let watching = task::spawn(async move { terminated(stopper).await });
        let result = timeout(Duration::from_millis(100), watching).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.is_ok());
    }
}
