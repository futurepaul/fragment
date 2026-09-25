// The CLI's tests run against the shared code.storage fake
// (crates/fakes), which also answers the fragment host's storage-token and
// refresh routes here, since these tests have no cell. Two entries per
// listing page, so pagination is exercised.
use fragment_fakes::codestorage::{CodeStorage, Options};

pub type MockServer = CodeStorage;

pub fn start() -> MockServer {
    with_token_ttl(Options::default().token_ttl_s)
}

/// Storage tokens that last `ttl_s` seconds.
pub fn with_token_ttl(ttl_s: i64) -> MockServer {
    CodeStorage::start(Options { page_size: 2, host_routes: true, token_ttl_s: ttl_s, ..Options::default() }).expect("start the code.storage fake")
}
