use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::time;
use waltz::{spawn, ActorRef, Handler};

#[tokio::main]
async fn main() -> Result<()> {
    let echo_request_handler = spawn(EchoRequestHandler);
    let echo_reply_handler = spawn(EchoReplyHandler);

    echo_request_handler
        .tell(EchoRequest {
            text: "Echo".to_string(),
            reply_to: echo_reply_handler,
        })
        .await;

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
impl Handler<EchoRequest> for EchoRequestHandler {
    async fn receive(&mut self, msg: EchoRequest) {
        let EchoRequest { text, reply_to } = msg;
        reply_to.tell(EchoReply { text }).await;
    }
}

struct EchoReplyHandler;

#[async_trait]
impl Handler<EchoReply> for EchoReplyHandler {
    async fn receive(&mut self, msg: EchoReply) {
        let EchoReply { text } = msg;
        println!("Reveived reply: {text}");
    }
}
