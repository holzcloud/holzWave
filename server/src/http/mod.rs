//! The HTTP side of the server: the listener, the routes and the checks every request passes.

pub mod app;
pub mod cookies;
pub mod csrf;
pub mod embed;
pub mod features;
pub mod headers;
pub mod health;
pub mod router;
pub mod routes_admin;
pub mod routes_auth;
pub mod routes_device;
pub mod routes_enrol;
pub mod routes_files;
pub mod routes_me;
pub mod routes_unlock;
pub mod transport;
pub mod ws;
