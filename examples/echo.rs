use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use waltz::{
    init, spawn, terminated::terminated, ActorContext, ActorRef, Handler, MsgOrSignal, StateOrStop,
};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let echo_requester = spawn(EchoRequester, init!(ctx, 0)).await;
    let echo_replyer = spawn(EchoReplyer, init!(ctx, ())).await;

    echo_requester
        .tell(EchoRequest {
            text: "Echo".to_string(),
            reply_to: echo_replyer.clone(),
        })
        .await;

    let echo_requester_2 = echo_requester.clone();
    tokio::spawn(async move {
        echo_requester_2
            .tell(EchoRequest {
                text: "Echo 2".to_string(),
                reply_to: echo_replyer,
            })
            .await;
    });

    let _ = terminated(echo_requester).await;
    Ok(())
}

struct EchoRequest {
    text: String,
    reply_to: ActorRef<EchoReply>,
}

struct EchoRequester;

#[async_trait]
impl Handler for EchoRequester {
    type Msg = EchoRequest;

    type State = u32;

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        if let MsgOrSignal::Msg(EchoRequest { text, reply_to }) = msg {
            reply_to.tell(EchoReply { text }).await;
        }
        if state < 1 {
            StateOrStop::State(state + 1)
        } else {
            StateOrStop::Stop
        }
    }
}

#[derive(Debug)]
struct EchoReply {
    text: String,
}

struct EchoReplyer;

#[async_trait]
impl Handler for EchoReplyer {
    type Msg = EchoReply;

    type State = ();

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        if let MsgOrSignal::Msg(EchoReply { text }) = msg {
            println!("Reveived reply: {text}");
        }
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
