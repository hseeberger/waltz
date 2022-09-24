use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use waltz::{spawn, system, ActorContext, ActorSystem, Handler, MsgOrSignal, NotUsed, StateOrStop};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let system = system!(Guardian, |ctx| {
        spawn!(ctx, Noop, |ctx| {
            spawn!(ctx, Noop, |ctx| {
                spawn!(ctx, Noop, ()).await;
                ()
            })
            .await;
            ()
        })
        .await;
        ()
    })
    .await;

    system.guardian().tell(()).await;

    let _ = system.terminated().await;
    Ok(())
}

struct Guardian;

#[async_trait]
impl Handler for Guardian {
    type Msg = ();

    type State = ();

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        _: MsgOrSignal<Self::Msg>,
        _: Self::State,
    ) -> StateOrStop<Self::State> {
        StateOrStop::Stop
    }
}

struct Noop;

#[async_trait]
impl Handler for Noop {
    type Msg = NotUsed;

    type State = ();

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        _: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        StateOrStop::State(state)
    }
}

fn init_tracing() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()
        .context("Cannot initialize tracing subscriber")
}
