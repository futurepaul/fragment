//! The sections, in the order they run.

mod app;
mod author;
mod control;
mod members;
mod plane;
mod restart;
mod site;
mod sync;

use anyhow::Result;

use crate::api::Api;
use crate::Suite;

pub fn run(s: &mut Suite, api: Api) -> Result<()> {
    control::auth(s, &api)?;
    control::create(s, &api)?;
    control::lockdown(s, &api)?;
    members::members(s, &api)?;
    members::secrets(s, &api)?;
    plane::files(s, &api)?;
    plane::deploy(s, &api)?;
    app::ops(s, &api)?;
    app::public(s, &api)?;
    site::site(s, &api)?;
    site::watch(s, &api)?;
    author::schemas(s, &api)?;
    author::channels(s, &api)?;
    author::live(s, &api)?;
    author::routes(s, &api)?;
    author::cli(s, &api)?;
    author::browser(s, &api)?;
    sync::folder_sync(s, &api)?;
    let api = restart::restart(s, api)?;
    restart::pathmode(s, api)?;
    Ok(())
}
