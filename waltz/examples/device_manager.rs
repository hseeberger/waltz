//! waltz's take on Akka's IoT device manager: the root routes commands to per-group and per-device
//! actors, spawning them on demand and pruning its registry via watch. Reading a group's average
//! temperature is served by a per-request `Query` child which fans `Read` out to every device: the
//! watch ordering guarantee proves that a terminated device will never reply, so the only deadline
//! needed is the timeout of the `ask` at the async boundary. Devices restart on failure: an
//! invalid reading fails the device, which loses its last reading but keeps its mailbox, so it
//! answers the queued read with no reading yet.
//!
//! The averages are printed to stdout and waltz logs to stderr; the log level is configured via
//! `RUST_LOG`, e.g. `RUST_LOG=waltz=debug cargo run --quiet -p waltz --example device_manager`.

use anyhow::Context;
use std::{collections::HashMap, convert::Infallible, io, num::NonZeroU32, time::Duration};
use thiserror::Error;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use waltz::{
    Actor, ActorConfig, ActorContext, ActorId, ActorRef, ActorSystem, Control, Incoming, ReplyTo,
    RestartPolicy, SupervisionStrategy,
};

const MAX_RESTARTS: NonZeroU32 = NonZeroU32::new(3).expect("3 is not zero");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let system = ActorSystem::new(DeviceManager);
    let manager = system.root();

    manager.tell(Command::Record {
        group: "attic",
        device: "left",
        degrees: 21.5,
    });
    manager.tell(Command::Record {
        group: "attic",
        device: "right",
        degrees: 19.1,
    });
    manager.tell(Command::Record {
        group: "cellar",
        device: "main",
        degrees: 11.0,
    });
    // The invalid reading fails the device, which restarts and thereby loses the 11.0.
    manager.tell(Command::Record {
        group: "cellar",
        device: "main",
        degrees: f64::NAN,
    });

    for group in ["attic", "cellar", "garage"] {
        let average = manager
            .ask(Duration::from_secs(2), |reply_to| Command::Average {
                group,
                reply_to,
            })
            .await
            .context("asking for the average temperature")?;

        match average {
            Some(average) => println!("## Average temperature in {group}: {average:.1}"),
            None => println!("## No temperature readings in {group}"),
        }
    }

    manager.tell(Command::Shutdown);

    system
        .terminated()
        .await
        .context("awaiting actor system termination")
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_writer(io::stderr),
        )
        .init();
}

struct DeviceManager;

impl Actor for DeviceManager {
    type Message = Command;
    type State = HashMap<&'static str, ActorRef<GroupMessage>>;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(HashMap::new())
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        mut groups: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(Command::Record {
                group,
                device,
                degrees,
            }) => {
                let group_ref = groups.entry(group).or_insert_with(|| {
                    let group_ref = context.spawn(Group { name: group });
                    context.watch(&group_ref);
                    group_ref
                });
                group_ref.tell(GroupMessage::Record { device, degrees });
                Ok(Control::Continue(groups))
            }

            Incoming::Message(Command::Average { group, reply_to }) => {
                match groups.get(group) {
                    Some(group_ref) => group_ref.tell(GroupMessage::Average(reply_to)),
                    None => reply_to.reply(None),
                }
                Ok(Control::Continue(groups))
            }

            Incoming::Message(Command::Shutdown) => Ok(Control::Stop),

            // Watch-based pruning keeps the registry free of terminated groups.
            Incoming::Terminated(actor_id) => {
                groups.retain(|_, group_ref| group_ref.actor_id() != actor_id);
                Ok(Control::Continue(groups))
            }
        }
    }
}

enum Command {
    Record {
        group: &'static str,
        device: &'static str,
        degrees: f64,
    },

    Average {
        group: &'static str,
        reply_to: ReplyTo<Option<f64>>,
    },

    Shutdown,
}

struct Group {
    name: &'static str,
}

impl Actor for Group {
    type Message = GroupMessage;
    type State = HashMap<&'static str, ActorRef<DeviceMessage>>;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(HashMap::new())
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        mut devices: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(GroupMessage::Record { device, degrees }) => {
                let device_ref = devices.entry(device).or_insert_with(|| {
                    // A sensor is flaky by nature, so a device restarts on failure.
                    let config = ActorConfig::default().with_supervision_strategy(
                        SupervisionStrategy::Restart(RestartPolicy::new(MAX_RESTARTS)),
                    );
                    let device_ref = context.spawn_with_config(
                        Device {
                            group: self.name,
                            name: device,
                        },
                        config,
                    );
                    context.watch(&device_ref);
                    device_ref
                });
                device_ref.tell(DeviceMessage::Record(degrees));
                Ok(Control::Continue(devices))
            }

            Incoming::Message(GroupMessage::Average(reply_to)) => {
                let query = context.spawn(Query);
                query.tell(QueryMessage::Start {
                    devices: devices.values().cloned().collect(),
                    reply_to,
                });
                Ok(Control::Continue(devices))
            }

            Incoming::Terminated(actor_id) => {
                devices.retain(|_, device_ref| device_ref.actor_id() != actor_id);
                Ok(Control::Continue(devices))
            }
        }
    }
}

enum GroupMessage {
    Record { device: &'static str, degrees: f64 },
    Average(ReplyTo<Option<f64>>),
}

struct Query;

impl Actor for Query {
    type Message = QueryMessage;
    type State = QueryState;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(QueryState::Starting)
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(QueryMessage::Start { devices, reply_to }) => {
                if devices.is_empty() {
                    reply_to.reply(None);
                    Ok(Control::Stop)
                } else {
                    let mut pending = HashMap::new();
                    for device in devices {
                        context.watch(&device);
                        let device_id = device.actor_id();
                        device.tell(DeviceMessage::Read(context.reply_to(move |reading| {
                            QueryMessage::Reading { device_id, reading }
                        })));
                        pending.insert(device_id, device);
                    }
                    Ok(Control::Continue(QueryState::Running {
                        pending,
                        sum: 0.0,
                        count: 0,
                        reply_to,
                    }))
                }
            }

            Incoming::Message(QueryMessage::Reading { device_id, reading }) => {
                let QueryState::Running {
                    mut pending,
                    mut sum,
                    mut count,
                    reply_to,
                } = state
                else {
                    unreachable!("a reading only arrives after the start message")
                };

                // Unwatch after the reply: a later terminated signal must not count this device
                // again.
                if let Some(device) = pending.remove(&device_id) {
                    context.unwatch(&device);
                }
                if let Some(degrees) = reading {
                    sum += degrees;
                    count += 1;
                }
                Query::conclude(pending, sum, count, reply_to)
            }

            Incoming::Terminated(device_id) => {
                let QueryState::Running {
                    mut pending,
                    sum,
                    count,
                    reply_to,
                } = state
                else {
                    unreachable!("watching only starts with the start message")
                };

                // The ordering guarantee proves this device's reply can no longer arrive.
                pending.remove(&device_id);
                Query::conclude(pending, sum, count, reply_to)
            }
        }
    }
}

impl Query {
    fn conclude(
        pending: HashMap<ActorId, ActorRef<DeviceMessage>>,
        sum: f64,
        count: u32,
        reply_to: ReplyTo<Option<f64>>,
    ) -> Result<Control<QueryState>, Infallible> {
        if pending.is_empty() {
            reply_to.reply((count > 0).then(|| sum / f64::from(count)));
            Ok(Control::Stop)
        } else {
            Ok(Control::Continue(QueryState::Running {
                pending,
                sum,
                count,
                reply_to,
            }))
        }
    }
}

enum QueryMessage {
    Start {
        devices: Vec<ActorRef<DeviceMessage>>,
        reply_to: ReplyTo<Option<f64>>,
    },

    Reading {
        device_id: ActorId,
        reading: Option<f64>,
    },
}

enum QueryState {
    Starting,

    Running {
        pending: HashMap<ActorId, ActorRef<DeviceMessage>>,
        sum: f64,
        count: u32,
        reply_to: ReplyTo<Option<f64>>,
    },
}

struct Device {
    group: &'static str,
    name: &'static str,
}

impl Actor for Device {
    type Message = DeviceMessage;
    type State = Option<f64>;
    type Error = InvalidReading;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(None)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        last_reading: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(message) = incoming else {
            unreachable!("the device watches no actor, hence never gets a terminated signal")
        };

        match message {
            DeviceMessage::Record(degrees) if degrees.is_nan() => Err(InvalidReading),

            DeviceMessage::Record(degrees) => {
                println!("## {}/{} recorded {degrees}", self.group, self.name);
                Ok(Control::Continue(Some(degrees)))
            }

            DeviceMessage::Read(reply_to) => {
                reply_to.reply(last_reading);
                Ok(Control::Continue(last_reading))
            }
        }
    }
}

enum DeviceMessage {
    Record(f64),
    Read(ReplyTo<Option<f64>>),
}

#[derive(Debug, Error)]
#[error("reading is not a number")]
struct InvalidReading;
