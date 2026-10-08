//! The bridge a computer image runs (docs/computers.md): an agent runtime on
//! one side (Hermes' Relay, or a scripted stub), the ordinary fragment API
//! on the other. `engine` holds its rules, pure; `driver` its I/O; `runtime`
//! the runtimes; `api` every route it calls; `ready` which agents the image
//! has made ready, when it says; `note` what a turn after a cut one is told,
//! from the journal; `screen` each agent's screen, `screens` the image's
//! map of their displays, `lease` who drives one.

pub mod api;
pub mod driver;
pub mod engine;
pub mod lease;
pub mod limits;
pub mod log;
pub mod net;
pub mod note;
pub mod ready;
pub mod records;
pub mod runtime;
pub mod screen;
pub mod screens;
