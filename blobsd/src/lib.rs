//! blobsd library surface. The binary is a thin boot shell over this crate;
//! integration tests link the same modules the server runs.

pub mod app;
pub mod auth;
pub mod bucket;
pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod logging;
