use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::time;
use waltz::{spawn, ActorRef, Handler};

#[tokio::main]
async fn main() -> Result<()> {
    let echo_request_handler = spawn(EchoRequestHandler, ());
    let echo_reply_handler = spawn(EchoReplyHandler, ());

    echo_request_handler
        .tell(EchoRequest {
            text: "Echo".to_string(),
            reply_to: echo_reply_handler.clone(),
        })
        .await;

    let echo_request_handler_2 = echo_request_handler.clone();
    tokio::spawn(async move {
        echo_request_handler_2
            .tell(EchoRequest {
                text: "Echo 2".to_string(),
                reply_to: echo_reply_handler,
            })
            .await;
    });

    time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

struct EchoRequest {
    text: String,
    reply_to: ActorRef<EchoReply>,
}

struct EchoReply {
    text: String,
}

struct EchoRequestHandler;

#[async_trait]
impl Handler for EchoRequestHandler {
    type Msg = EchoRequest;
    type State = ();

    async fn receive(&mut self, msg: Self::Msg, _: Self::State) {
        let EchoRequest { text, reply_to } = msg;
        reply_to.tell(EchoReply { text }).await;
    }
}

struct EchoReplyHandler;

#[async_trait]
impl Handler for EchoReplyHandler {
    type Msg = EchoReply;
    type State = ();

    async fn receive(&mut self, msg: Self::Msg, _: Self::State) {
        let EchoReply { text } = msg;
        println!("Reveived reply: {text}");
    }
}
