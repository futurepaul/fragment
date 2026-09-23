// The CLI's tests run against the shared code.storage fake
// (crates/fakes), which also answers the fragment host's storage-token and
// refresh routes here, since these tests have no cell. Two entries per
// listing page, so pagination is exercised.
use fragment_fakes::codestorage::{CodeStorage, Options};

pub struct MockServer {
    pub url: String,
    inner: CodeStorage,
}

impl MockServer {
    pub fn start() -> MockServer {
        let inner = CodeStorage::start(Options { page_size: 2, host_routes: true, ..Options::default() }).expect("start the code.storage fake");
        MockServer { url: inner.url.clone(), inner }
    }
}

impl std::ops::Deref for MockServer {
    type Target = CodeStorage;
    fn deref(&self) -> &CodeStorage {
        &self.inner
    }
}
