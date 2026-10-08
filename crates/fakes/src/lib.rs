//! Fakes of the services the platform calls, for the e2e, `cargo xtask
//! dev`, and unit tests. Each implements the documented HTTP shape of the
//! real service for the surface the platform touches, plus test levers the
//! real service does not have (they are methods, never production routes):
//! code.storage, Workers AI (the model route's lower rung, text and
//! images), Email Sending (the platform's mail), a web push service, WorkOS
//! (AuthKit and Pipes), and the provider APIs a computer's swap sends to.

pub mod codestorage;
pub mod http;
pub mod mail;
pub mod push;
pub mod stripe;
pub mod upstream;
pub mod workers_ai;
pub mod workos;
