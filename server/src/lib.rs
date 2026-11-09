#![forbid(unsafe_code)]
//! The server of the holzWave browser build: the supervisor, the sign-in methods and one worker per person.

pub mod accounts;
pub mod acme;
pub mod audit;
pub mod auth;
pub mod cli;
pub mod clock;
pub mod config;
pub mod device;
pub mod http;
pub mod keyfile;
pub mod limits;
pub mod log;
pub mod proxy;
pub mod sealed;
pub mod store;
pub mod supervisor;
pub mod tls;
pub mod unlock;
pub mod wire;
pub mod worker;
