//! The bridge a computer image runs (docs/computers.md): an agent runtime on
//! one side (Hermes' Relay, or a scripted stub), the ordinary fragment API
//! on the other. `engine` holds its rules, pure; `driver` its I/O; `runtime`
//! the runtimes; `api` every route it calls.

pub mod api;
pub mod driver;
pub mod engine;
pub mod limits;
pub mod log;
pub mod net;
pub mod records;
pub mod runtime;
pub mod screen;
