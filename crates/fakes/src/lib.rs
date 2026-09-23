//! Fakes of the services the platform calls, for the e2e, `cargo xtask
//! dev`, and unit tests. Each implements the documented HTTP shape of the
//! real service for the surface the platform touches, plus test levers the
//! real service does not have (they are methods, never production routes).

pub mod codestorage;
pub mod http;
