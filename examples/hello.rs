use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use waltz::{init, ActorContext, ActorSystem, Handler, MsgOrSignal, StateOrStop};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let system = ActorSystem::new(Guardian, init!(ctx, ())).await;
    system.guardian().tell(SayHello).await;

    let _ = system.terminated().await;
    Ok(())
}

struct SayHello;

struct Guardian;

#[async_trait]
impl Handler for Guardian {
    type Msg = SayHello;

    type State = ();

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        _: MsgOrSignal<Self::Msg>,
        _: Self::State,
    ) -> StateOrStop<Self::State> {
        println!("Hello");
        StateOrStop::Stop
    }
}

fn init_tracing() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()
        .context("Cannot initialize tracing subscriber")
}
