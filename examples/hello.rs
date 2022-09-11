use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::time;
use waltz::{spawn, Handler};

#[tokio::main]
async fn main() -> Result<()> {
    let say_hello_ref = spawn(SayHelloHandler);
    say_hello_ref.tell(SayHello).await;

    time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

struct SayHello;

struct SayHelloHandler;

#[async_trait]
impl Handler<SayHello> for SayHelloHandler {
    async fn receive(&mut self, _msg: SayHello) {
        println!("Hello");
    }
}
