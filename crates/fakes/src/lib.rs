//! Fakes of the services the platform calls, for the e2e, `cargo xtask
//! dev`, and unit tests. Each implements the documented HTTP shape of the
//! real service for the surface the platform touches, plus test levers the
//! real service does not have (they are methods, never production routes):
//! code.storage, Workers AI (the model route's lower rung, text and
//! images), Email Sending (the platform's mail), a web push service, WorkOS
//! (AuthKit and Pipes), OpenRouter (an own key's sign-in and its models),
//! and the provider APIs a computer's swap sends to.

pub mod codestorage;
pub mod http;
pub mod mail;
pub mod openrouter;
pub mod push;
pub mod stripe;
pub mod upstream;
pub mod workers_ai;
pub mod workos;
