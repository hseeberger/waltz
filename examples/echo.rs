//! In this example we create a guardian actor which creates two actors: one – the requester –
//! sending two echo requests to the other – the replyer. Once the requester gets two replies, it
//! stops which leads to the guardian, which is watching the requester, to stop and therefore the
//! actor system to terminate.

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use waltz::{
    spawn, system, ActorContext, ActorRef, ActorSystem, Handler, MsgOrSignal, NoMsg, StateOrStop,
};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let system = system!(Guardian, |ctx| {
        // Create the replyer actor, no particular init needed
        let echo_replyer = spawn!(ctx, EchoReplyer, ()).await;

        // Create the requester actor, send request to replyer during init
        let echo_requester = spawn!(ctx, EchoRequester(echo_replyer.clone()), |ctx| {
            echo_replyer
                .tell(EchoRequest {
                    text: "Echo".to_string(),
                    reply_to: ctx.self_ref().to_owned(),
                })
                .await;
            0
        })
        .await;

        // The reqeuster is expected to stop after receiving two responses – watching it from
        // the guardian leads to terminating the actor system
        ctx.watch(echo_requester);
    })
    .await;

    // Await actor system termination (see above)
    let _ = system.terminated().await;
    Ok(())
}

struct Guardian;

#[async_trait]
impl Handler for Guardian {
    type Msg = NoMsg;

    type State = ();

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        match msg {
            // We are only interested in watching the termination of the only watched actor – the
            // requester
            MsgOrSignal::Terminated(_) => StateOrStop::Stop,
            _ => StateOrStop::State(state),
        }
    }
}

struct EchoRequest {
    text: String,
    reply_to: ActorRef<EchoReply>,
}

struct EchoRequester(ActorRef<EchoRequest>);

#[async_trait]
impl Handler for EchoRequester {
    type Msg = EchoReply;

    // Keep track of the number of "invocations" to be able to stop after two
    type State = u32;

    async fn receive(
        &mut self,
        ctx: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        match msg {
            MsgOrSignal::Msg(EchoReply { text }) => {
                eprintln!("Reveived reply with text {text}");
                if state < 1 {
                    self.0
                        .tell(EchoRequest {
                            text: "Echo 2".to_string(),
                            reply_to: ctx.self_ref().to_owned(),
                        })
                        .await;
                    StateOrStop::State(state + 1)
                } else {
                    StateOrStop::Stop
                }
            }
            _ => StateOrStop::State(state),
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
    type Msg = EchoRequest;

    type State = ();

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        if let MsgOrSignal::Msg(EchoRequest { text, reply_to }) = msg {
            eprintln!("Reveived request with text {text}");
            reply_to.tell(EchoReply { text }).await;
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
