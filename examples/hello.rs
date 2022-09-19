use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use waltz::{spawn, terminated::terminated, ActorContext, Handler, MsgOrSignal, StateOrStop};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let hello = spawn(Hello, |ctx| async { (ctx, ()) }).await;
    let hello_terminated = terminated(hello.clone());
    hello.tell(SayHello).await;
    let _ = hello_terminated.await;
    Ok(())
}

struct SayHello;

struct Hello;

#[async_trait]
impl Handler for Hello {
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
