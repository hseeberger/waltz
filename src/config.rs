use lazy_static::lazy_static;

pub struct Config {
    pub default_mailbox_size: usize,
}

lazy_static! {
    pub static ref CONFIG: Config = Config {
        default_mailbox_size: 42
    };
}
