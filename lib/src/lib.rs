extern crate carrier_core;
#[macro_use]
pub extern crate failure;
extern crate bs58;
extern crate byteorder;
extern crate crc8;
extern crate ed25519_dalek;
extern crate rand;
extern crate sha2;
extern crate subtle;
extern crate x25519_dalek;
#[macro_use]
extern crate prost_derive;
pub extern crate bytes;
extern crate prost;
#[macro_use]
extern crate log;
extern crate env_logger;
extern crate futures;
extern crate tokio;
#[macro_use]
extern crate lazy_static;
extern crate hpack;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate trust_dns_resolver;
extern crate interfaces2 as interfaces;
extern crate dirs;
extern crate fs2;

pub mod channel;
pub mod clock;
pub mod config;
pub mod connect;
pub mod dns;
pub mod endpoint;
pub mod keystore;
pub mod local_addrs;
pub mod publisher;
pub mod subscriber;

pub use carrier_core::*;
pub use identity::Identity;
pub use identity::Secret;

pub mod prelude {
    pub use bytes;
    pub use failure;
}

mod carrier {
    pub use super::*;
}
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/carrier.broker.v1.rs"));
}
