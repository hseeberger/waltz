//! An actor framework.
//!
//! An actor is created by implementing [Actor]: [Actor::init] creates its initial state and
//! [Actor::receive] applies received messages and signals to that state until it returns
//! [Control::Stop].
//!
//! Actors form a tree: the root actor is spawned by creating an [ActorSystem] and any actor can
//! spawn child actors via [ActorContext::spawn]. When an actor stops, its child actors are stopped
//! first; only once all descendants have terminated does it terminate itself. Hence
//! [ActorSystem::terminated] resolves once the whole actor tree has terminated.
//!
//! When [Actor::init] or [Actor::receive] fails with an error or a panic, the actor's
//! [SupervisionStrategy] decides what happens: [SupervisionStrategy::Stop] terminates the actor,
//! [SupervisionStrategy::Restart] rebuilds its state via [Actor::init], limited and paced by a
//! [RestartPolicy].
//!
//! Actors are addressed by [ActorRef], which is used to [ActorRef::tell] them messages and to
//! [ActorRef::ask] them requests from outside the actor tree, awaiting the reply; between actors,
//! [ActorContext::reply_to] creates a [ReplyTo] which delivers the reply as an ordinary message
//! instead. Actors can observe each other via [ActorContext::watch], which delivers an
//! [Incoming::Terminated] signal, and [ActorContext::unwatch], which reverts that. The signal is
//! ordered behind all messages the terminated actor has delivered to the watcher, hence receiving
//! it proves that the watcher has seen every message from that actor it will ever see: each
//! arrived before the signal or was dropped as a dead letter.

#![warn(missing_docs)]

mod actor;
mod actor_config;
mod actor_context;
mod actor_id;
mod actor_ref;
mod actor_system;
mod ask;
mod backoff;
mod mailbox;
mod quota;
mod sync;

pub use crate::{
    actor::{Actor, Control, Incoming, Nothing},
    actor_config::{ActorConfig, MailboxCapacity, RestartPolicy, SupervisionStrategy},
    actor_context::ActorContext,
    actor_id::ActorId,
    actor_ref::ActorRef,
    actor_system::{ActorSystem, Error},
    ask::{AskError, ReplyTo},
    backoff::{Backoff, InvalidBackoff},
};
