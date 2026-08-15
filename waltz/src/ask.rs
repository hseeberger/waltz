use crate::mailbox::SendError;
use derive_more::Debug;
use std::time::Duration;
use thiserror::Error;

/// A single-shot destination for the reply to a request, carried inside the request message.
///
/// Created by [ActorRef::ask] or [ActorContext::reply_to] and consumed by [ReplyTo::reply], hence
/// at most one reply can be sent, enforced at compile time. The responder cannot tell how a
/// [ReplyTo] was created; both origins are handled the same way.
///
/// [ActorRef::ask]: crate::ActorRef::ask
/// [ActorContext::reply_to]: crate::ActorContext::reply_to
#[derive(Debug)]
pub struct ReplyTo<R> {
    #[debug(skip)]
    send_reply: Box<dyn FnOnce(R) + Send>,
}

impl<R> ReplyTo<R> {
    /// Send the reply without blocking. If it cannot be delivered, e.g. because the asker has
    /// terminated or is no longer awaiting it, the reply is dropped and logged as a dead letter.
    pub fn reply(self, reply: R) {
        (self.send_reply)(reply);
    }

    pub(crate) fn new<F>(send_reply: F) -> Self
    where
        F: FnOnce(R) + Send + 'static,
    {
        Self {
            send_reply: Box::new(send_reply),
        }
    }
}

/// The possible failures of [ActorRef::ask].
///
/// [ActorRef::ask]: crate::ActorRef::ask
#[derive(Debug, Error)]
pub enum AskError {
    /// The request was not sent: the actor's bounded mailbox was full.
    #[error("mailbox full")]
    MailboxFull,

    /// The request was not sent: the actor has terminated.
    #[error("actor terminated")]
    ActorTerminated,

    /// The request was sent, but no reply will ever arrive: the [ReplyTo] was dropped without a
    /// reply or the actor stopped with the request still queued.
    #[error("no reply")]
    NoReply,

    /// The request was sent, but no reply arrived within the given duration; a late reply is
    /// dropped and logged as a dead letter.
    #[error("no reply within {0:?}")]
    Timeout(Duration),
}

impl From<SendError> for AskError {
    fn from(error: SendError) -> Self {
        match error {
            SendError::MailboxFull(_) => Self::MailboxFull,
            SendError::ActorTerminated(_) => Self::ActorTerminated,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ReplyTo;
    use std::sync::mpsc;

    /// The sink is invoked exactly with the value passed to `reply`.
    #[test]
    fn reply_invokes_the_sink_with_the_reply() {
        let (reply_tx, reply_rx) = mpsc::channel();
        let reply_to = ReplyTo::new(move |reply| reply_tx.send(reply).expect("reply is received"));

        reply_to.reply(42);

        assert_eq!(reply_rx.recv(), Ok(42));
    }

    /// The sink is skipped in the `Debug` output and adds no `Debug` bound on the reply type, so
    /// message enums holding a `ReplyTo` can derive `Debug`.
    #[test]
    fn debug_skips_the_sink() {
        struct NotDebug;

        let reply_to = ReplyTo::new(|NotDebug| ());

        assert!(format!("{reply_to:?}").starts_with("ReplyTo"));
    }
}
