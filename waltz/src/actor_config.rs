use crate::backoff::Backoff;
use std::{
    num::{NonZeroU32, NonZeroUsize},
    time::Duration,
};

/// Configuration for an [Actor].
///
/// [Actor]: crate::Actor
#[derive(Debug, Default, Clone, Copy)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize),
    serde(default, deny_unknown_fields)
)]
pub struct ActorConfig {
    /// The capacity of the actor's mailbox. Defaults to [MailboxCapacity::Unbounded].
    pub mailbox_capacity: MailboxCapacity,

    /// The strategy deciding what happens if the actor has failed. Defaults to
    /// [SupervisionStrategy::Stop].
    pub supervision_strategy: SupervisionStrategy,
}

/// The capacity of an actor's mailbox.
#[derive(Debug, Default, Clone, Copy)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum MailboxCapacity {
    /// Messages are never dropped, but an actor which cannot keep up grows its mailbox without
    /// limit.
    #[default]
    Unbounded,

    /// Messages sent to a full mailbox are dropped and logged as dead letters, but a terminated
    /// signal is still delivered.
    Bounded(NonZeroUsize),
}

/// The strategy deciding what happens to an actor that has failed.
#[derive(Debug, Default, Clone, Copy)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum SupervisionStrategy {
    /// Stop the actor.
    #[default]
    Stop,

    /// Stop the child actors and replace the current state with a newly initialized one, limited
    /// and paced by the given [RestartPolicy].
    ///
    /// [Actor::init] is re-run on the same actor value: anything the actor itself carries, e.g.
    /// via interior mutability, survives the restart; only the state is rebuilt. That includes
    /// whatever a caught panic left behind, e.g. a poisoned mutex.
    ///
    /// [Actor::init]: crate::Actor::init
    Restart(RestartPolicy),
}

/// The limit and pacing for restarts, applied to failures of [Actor::receive] and [Actor::init]
/// alike, including the first initialization at spawn.
///
/// Failures form a streak: the n-th restart within a streak is delayed by `backoff.min() *
/// 2^(n-1)`, capped at `backoff.max()`; once a streak exceeds `max_restarts`, the actor stops, so
/// persistent failure escalates to the watchers. A streak ends, resetting count and backoff, once
/// the actor has run for at least `reset_after` without failing.
///
/// [Actor::init]: crate::Actor::init
/// [Actor::receive]: crate::Actor::receive
#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct RestartPolicy {
    /// The maximum number of restarts within a streak; one more failure stops the actor.
    pub max_restarts: NonZeroU32,

    /// The bounds pacing the restarts of a streak. Defaults to [Backoff::default].
    #[cfg_attr(feature = "serde", serde(default))]
    pub backoff: Backoff,

    /// Running this long without failure ends the streak. Defaults to
    /// [RestartPolicy::DEFAULT_RESET_AFTER].
    #[cfg_attr(
        feature = "serde",
        serde(default = "default_reset_after", with = "humantime_serde")
    )]
    pub reset_after: Duration,
}

impl RestartPolicy {
    /// The `reset_after` of a policy created by [RestartPolicy::new].
    pub const DEFAULT_RESET_AFTER: Duration = Duration::from_secs(30);

    /// A policy with the given restart limit, paced by [Backoff::default] and
    /// [RestartPolicy::DEFAULT_RESET_AFTER].
    pub fn new(max_restarts: NonZeroU32) -> Self {
        Self {
            max_restarts,
            backoff: Backoff::default(),
            reset_after: Self::DEFAULT_RESET_AFTER,
        }
    }
}

#[cfg(feature = "serde")]
fn default_reset_after() -> Duration {
    RestartPolicy::DEFAULT_RESET_AFTER
}

#[cfg(test)]
mod tests {
    use crate::{Backoff, RestartPolicy};
    use std::num::NonZeroU32;

    /// A new policy is paced by the defaults its documentation names, so a caller only has to
    /// overwrite what it wants to differ.
    #[test]
    fn a_new_policy_is_paced_by_the_defaults() {
        let policy = RestartPolicy::new(NonZeroU32::MIN);

        assert_eq!(policy.backoff.min(), Backoff::DEFAULT_MIN);
        assert_eq!(policy.backoff.max(), Backoff::DEFAULT_MAX);
        assert_eq!(policy.reset_after, RestartPolicy::DEFAULT_RESET_AFTER);
    }

    /// The snake_case variant names, the per-field defaults and the humantime duration format are
    /// a public configuration surface; this pins it down.
    #[cfg(feature = "serde")]
    #[test]
    fn a_config_deserializes_from_its_documented_form() {
        use crate::{ActorConfig, MailboxCapacity, SupervisionStrategy};
        use std::time::Duration;

        let config = serde_json::from_str::<ActorConfig>(
            r#"{
                "mailbox_capacity": { "bounded": 42 },
                "supervision_strategy": { "restart": { "max_restarts": 3, "reset_after": "1m" } }
            }"#,
        )
        .expect("the documented config form deserializes");

        assert!(matches!(
            config.mailbox_capacity,
            MailboxCapacity::Bounded(capacity) if capacity.get() == 42
        ));

        let SupervisionStrategy::Restart(policy) = config.supervision_strategy else {
            panic!("expected a restart strategy")
        };
        assert_eq!(policy.max_restarts.get(), 3);
        assert_eq!(policy.reset_after, Duration::from_secs(60));
        assert_eq!(policy.backoff.min(), Backoff::DEFAULT_MIN);
        assert_eq!(policy.backoff.max(), Backoff::DEFAULT_MAX);
    }

    /// A misspelled key must be an error, not a silently applied default.
    #[cfg(feature = "serde")]
    #[test]
    fn deserializing_rejects_unknown_fields() {
        use crate::ActorConfig;

        let config =
            serde_json::from_str::<ActorConfig>(r#"{ "mailbox_capacty": { "bounded": 42 } }"#);
        assert!(config.is_err());

        let config = serde_json::from_str::<ActorConfig>(
            r#"{
                "supervision_strategy": {
                    "restart": { "max_restarts": 3, "reset_atfer": "1m" }
                }
            }"#,
        );
        assert!(config.is_err());
    }
}
