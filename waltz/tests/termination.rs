use std::{convert::Infallible, time::Duration};
use thiserror::Error;
use tokio::{sync::mpsc, time::timeout};
use waltz::{Actor, ActorContext, ActorRef, ActorSystem, Control, Incoming, Nothing};

const TIMEOUT: Duration = Duration::from_secs(5);
const SELF_SENDS: usize = 1_000;
const TERMINATION_ORDER: &[&str] = &["grandchild", "child", "root"];

/// An actor system terminates once its root actor has stopped.
#[tokio::test]
async fn stopping_root_terminates_system() {
    let (terminated_tx, _terminated_rx) = mpsc::channel(TERMINATION_ORDER.len());
    let root = Root(Terminated("root", terminated_tx));
    let system = ActorSystem::new(root);

    system.root().tell(());

    assert_terminates(system).await;
}

/// A stopping actor stops its child actors first and only terminates once all descendants have
/// terminated, i.e. the actor tree terminates bottom-up.
#[tokio::test(flavor = "multi_thread")]
async fn descendants_terminate_bottom_up() {
    let (terminated_tx, mut terminated_rx) = mpsc::channel(TERMINATION_ORDER.len());
    let root = Root(Terminated("root", terminated_tx));
    let system = ActorSystem::new(root);

    system.root().tell(());

    let mut terminated = Vec::new();
    for _ in 0..TERMINATION_ORDER.len() {
        let actor = recv(&mut terminated_rx, "not all actors terminated").await;
        terminated.push(actor);
    }
    assert_eq!(terminated, TERMINATION_ORDER);

    assert_terminates(system).await;
}

/// Dropping an actor system does not stop its actors: the root actor keeps running and processing
/// messages, it only forfeits `ActorSystem::terminated`.
#[tokio::test]
async fn dropping_the_system_does_not_stop_the_actors() {
    let (received_tx, mut received_rx) = mpsc::channel(1);
    let system = ActorSystem::new(Echo(received_tx));
    let root = system.root().clone();

    drop(system);

    root.tell(());
    recv(
        &mut received_rx,
        "root actor did not receive the message after its actor system was dropped",
    )
    .await;

    root.tell(());
    recv(
        &mut received_rx,
        "root actor stopped processing messages after its actor system was dropped",
    )
    .await;
}

/// `tell` never blocks, so an actor may send itself any number of messages from `init` even though
/// its mailbox is not drained yet. With the default `Unbounded` mailbox none of them are dropped.
#[tokio::test]
async fn init_may_self_send_without_blocking() {
    let (received_tx, mut received_rx) = mpsc::channel(1);
    let actor = SelfSender { received_tx };
    let system = ActorSystem::new(actor);

    let received = recv(
        &mut received_rx,
        "actor did not receive all messages sent by `init`",
    )
    .await;
    assert_eq!(received, SELF_SENDS);

    assert_terminates(system).await;
}

/// A terminated signal must prove that the actor's destructors have run, but a panic escaping one
/// of them must not skip the signal: the actor value is dropped on the termination path, hence a
/// panic there would otherwise unwind the actor's task before its watchers are signaled and the
/// actor system would never terminate.
#[tokio::test]
async fn panicking_actor_destructor_still_terminates_system() {
    let system = ActorSystem::new(PanickingActor);

    system.root().tell(());

    assert_terminates(system).await;
}

/// The same for a panic escaping the destructor of an actor's state, which is dropped when the
/// parent stops the actor: its watchers must still be signaled. The watch is registered while the
/// actor is still alive, so that the signal comes from the termination path rather than from
/// watching an already terminated actor.
#[tokio::test(flavor = "multi_thread")]
async fn panicking_state_destructor_still_signals_watchers() {
    let (child_tx, mut child_rx) = mpsc::channel(1);
    let system = ActorSystem::new(PanickingStateRoot(child_tx));

    let child = recv(&mut child_rx, "root actor did not spawn its child actor").await;

    let (observed_tx, mut observed_rx) = mpsc::channel(2);
    let observer = ActorSystem::new(Observer(observed_tx));
    observer.root().tell(child);
    assert_eq!(
        recv(&mut observed_rx, "watcher did not register its watch").await,
        Observed::Watching
    );

    system.root().tell(());

    assert_eq!(
        recv(
            &mut observed_rx,
            "watcher was not signaled about the terminated actor"
        )
        .await,
        Observed::Terminated
    );
    assert_terminates(system).await;
}

/// A state destructor panicking while `receive` returns an error is contained: the state is
/// dropped on receive's normal return path, so the panic starts a fresh unwind which is caught
/// like any other panic and fed to supervision, here the default `Stop`. Only a destructor
/// panicking during the unwind of a panicking `receive` or `init` aborts the process, as anywhere
/// in Rust.
#[tokio::test]
async fn panicking_state_destructor_on_error_is_supervised() {
    let system = ActorSystem::new(FailingWithPanickingState);

    system.root().tell(());

    assert_terminates(system).await;
}

async fn recv<T>(rx: &mut mpsc::Receiver<T>, not_received: &str) -> T {
    timeout(TIMEOUT, rx.recv())
        .await
        .expect(not_received)
        .expect("channel closed")
}

async fn assert_terminates<M>(system: ActorSystem<M>)
where
    M: Send + 'static,
{
    timeout(TIMEOUT, system.terminated())
        .await
        .expect("actor system did not terminate")
        .expect("watching the root actor failed");
}

struct Root(Terminated);

impl Actor for Root {
    type Message = ();
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        context.spawn(Child(self.0.child("child")));
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        _: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Ok(Control::Stop)
    }
}

struct Child(Terminated);

impl Actor for Child {
    type Message = Nothing;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        let grand_child = GrandChild {
            _terminated: self.0.child("grandchild"),
        };
        context.spawn(grand_child);
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Ok(Control::Continue(state))
    }
}

struct GrandChild {
    _terminated: Terminated,
}

impl Actor for GrandChild {
    type Message = Nothing;
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Ok(Control::Continue(state))
    }
}

/// Report every message it receives, so that its sender can tell that this actor keeps running.
struct Echo(mpsc::Sender<()>);

impl Actor for Echo {
    type Message = ();
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let _ = self.0.try_send(());
        Ok(Control::Continue(state))
    }
}

struct SelfSender {
    received_tx: mpsc::Sender<usize>,
}

impl Actor for SelfSender {
    type Message = SelfSend;
    type State = usize;
    type Error = Infallible;

    /// The mailbox is FIFO, hence `Done` arrives behind every `Tick` and the count it reports is
    /// the number of ticks which survived.
    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        for _ in 0..SELF_SENDS {
            context.self_ref().tell(SelfSend::Tick);
        }
        context.self_ref().tell(SelfSend::Done);
        Ok(0)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(SelfSend::Tick) => Ok(Control::Continue(state + 1)),

            Incoming::Message(SelfSend::Done) => {
                let _ = self.received_tx.try_send(state);
                Ok(Control::Stop)
            }

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

enum SelfSend {
    Tick,
    Done,
}

struct PanickingActor;

impl Drop for PanickingActor {
    fn drop(&mut self) {
        panic!("panicking actor destructor");
    }
}

impl Actor for PanickingActor {
    type Message = ();
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        _: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Ok(Control::Stop)
    }
}

struct PanickingStateRoot(mpsc::Sender<ActorRef<Nothing>>);

impl Actor for PanickingStateRoot {
    type Message = ();
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        let _ = self.0.try_send(context.spawn(PanickingStateChild));
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        _: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Ok(Control::Stop)
    }
}

struct PanickingStateChild;

impl Actor for PanickingStateChild {
    type Message = Nothing;
    type State = PanickingState;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(PanickingState)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Ok(Control::Continue(state))
    }
}

struct PanickingState;

impl Drop for PanickingState {
    fn drop(&mut self) {
        panic!("panicking state destructor");
    }
}

struct FailingWithPanickingState;

impl Actor for FailingWithPanickingState {
    type Message = ();
    type State = PanickingState;
    type Error = Boom;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(PanickingState)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        _state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Err(Boom)
    }
}

#[derive(Debug, Error)]
#[error("boom")]
struct Boom;

struct Observer(mpsc::Sender<Observed>);

impl Actor for Observer {
    type Message = ActorRef<Nothing>;
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(target) => {
                context.watch(&target);
                let _ = self.0.try_send(Observed::Watching);
            }

            Incoming::Terminated(_) => {
                let _ = self.0.try_send(Observed::Terminated);
            }
        }

        Ok(Control::Continue(state))
    }
}

/// What the observer saw: that it registered its watch, or the terminated signal itself.
#[derive(Debug, PartialEq, Eq)]
enum Observed {
    Watching,
    Terminated,
}

struct Terminated(&'static str, mpsc::Sender<&'static str>);

impl Terminated {
    fn child(&self, name: &'static str) -> Self {
        Self(name, self.1.clone())
    }
}

impl Drop for Terminated {
    fn drop(&mut self) {
        let _ = self.1.try_send(self.0);
    }
}
