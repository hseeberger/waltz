use crate::{ActorId, mailbox::SendError};
use derive_more::Debug;
use std::{any::type_name, cell::Cell, time::Duration};
use thiserror::Error;
use tracing::warn;

pub(crate) type SendReply<R> = Box<dyn FnOnce(R) + Send>;

/// A single-shot destination for the reply to a request, carried inside the request message.
///
/// Created by [ActorRef::ask] or [ActorContext::reply_to] and consumed by [ReplyTo::reply], hence
/// at most one reply can be sent, enforced at compile time. The responder cannot tell how a
/// [ReplyTo] was created; both origins are handled the same way.
///
/// With the `remote` feature a [ReplyTo] is serializable and can travel inside a message to
/// another node. Serializing it also consumes the destination, so a reply to a value serialized
/// twice is dropped and logged as a dead letter.
///
/// [ActorRef::ask]: crate::ActorRef::ask
/// [ActorContext::reply_to]: crate::ActorContext::reply_to
#[derive(Debug)]
pub struct ReplyTo<R> {
    recipient: ActorId,

    #[debug(skip)]
    send_reply: Cell<Option<SendReply<R>>>,
}

impl<R> ReplyTo<R> {
    /// Send the reply without blocking. If it cannot be delivered, e.g. because the asker has
    /// terminated or is no longer awaiting it, the reply is dropped and logged as a dead letter.
    pub fn reply(self, reply: R) {
        match self.send_reply.into_inner() {
            Some(send_reply) => send_reply(reply),

            None => warn!(
                recipient = %self.recipient,
                reply_type = type_name::<R>(),
                error = "reply destination already serialized",
                "dead letter"
            ),
        }
    }

    pub(crate) fn new<F>(recipient: ActorId, send_reply: F) -> Self
    where
        F: FnOnce(R) + Send + 'static,
    {
        Self {
            recipient,
            send_reply: Cell::new(Some(Box::new(send_reply))),
        }
    }

    #[cfg(feature = "remote")]
    pub(crate) fn recipient(&self) -> ActorId {
        self.recipient
    }

    #[cfg(feature = "remote")]
    pub(crate) fn take_send_reply(&self) -> Option<SendReply<R>> {
        self.send_reply.take()
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
    use crate::{ActorId, ReplyTo};
    use std::sync::mpsc;

    /// The sink is invoked exactly with the value passed to `reply`.
    #[test]
    fn reply_invokes_the_sink_with_the_reply() {
        let (reply_tx, reply_rx) = mpsc::channel();
        let reply_to = ReplyTo::new(ActorId::new(), move |reply| {
            reply_tx.send(reply).expect("reply is received")
        });

        reply_to.reply(42);

        assert_eq!(reply_rx.recv(), Ok(42));
    }

    /// The sink is skipped in the `Debug` output and adds no `Debug` bound on the reply type, so
    /// message enums holding a `ReplyTo` can derive `Debug`.
    #[test]
    fn debug_skips_the_sink() {
        struct NotDebug;

        let reply_to = ReplyTo::new(ActorId::new(), |NotDebug| ());

        assert!(format!("{reply_to:?}").starts_with("ReplyTo"));
    }

    /// A reply after the sink was taken, as remote serialization does, is a dead letter rather
    /// than a panic.
    #[cfg(feature = "remote")]
    #[test]
    fn reply_after_take_is_a_dead_letter() {
        let (reply_tx, reply_rx) = mpsc::channel();
        let reply_to = ReplyTo::new(ActorId::new(), move |reply| {
            reply_tx.send(reply).expect("reply is received")
        });

        let send_reply = reply_to.take_send_reply();
        assert!(send_reply.is_some());
        assert!(reply_to.take_send_reply().is_none());

        reply_to.reply(42);

        assert!(reply_rx.try_recv().is_err());
    }
}
