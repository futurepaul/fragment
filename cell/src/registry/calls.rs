//! The Registry's inner routes as types: each call's path, its body, and
//! its answer, defined once. The router and fragments ask with them
//! (`crate::ask_registry`), and the Registry decodes the same body and
//! answers the same type (`RegistryCell::route`), so the two ends agree at
//! compile time. The JSON on the wire is what the routes carried before.

use std::collections::BTreeMap;

use fragment_proto::{Identity, IdentityKind, IdentityView};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{CellError, CellResult};

/// A call to the Registry: `PATH` names its route, the implementing type is
/// its body, and `Answer` is what a 200 carries.
pub(crate) trait Call: Serialize + DeserializeOwned {
    const PATH: &'static str;
    type Answer: Serialize + DeserializeOwned;

    /// The asker's check of an answer, beyond its type: an identity's id
    /// is the registry's own form. The Registry made it, so a bad one is a
    /// host fault.
    fn checked(answer: Self::Answer) -> CellResult<Self::Answer> {
        Ok(answer)
    }
}

fn identity_checked(identity: Identity) -> CellResult<Identity> {
    if fragment_core::npub::is_identity(&identity.id) {
        Ok(identity)
    } else {
        Err(CellError::host(format!("the registry named a malformed identity {:?}", identity.id)))
    }
}

/// `POST /resolve`: the identity holding a key (401 unknown or revoked).
#[derive(Serialize, Deserialize)]
pub(crate) struct Resolve {
    pub key: String,
}

impl Call for Resolve {
    const PATH: &'static str = "/resolve";
    type Answer = Identity;
    fn checked(answer: Identity) -> CellResult<Identity> {
        identity_checked(answer)
    }
}

/// `POST /lookup`: whom an `id:`, an npub, or a 64-hex key names (an
/// active key's holder).
#[derive(Serialize, Deserialize)]
pub(crate) struct Lookup {
    pub who: String,
}

impl Call for Lookup {
    const PATH: &'static str = "/lookup";
    type Answer = Identity;
    fn checked(answer: Identity) -> CellResult<Identity> {
        identity_checked(answer)
    }
}

/// `POST /agents`: an agent its owner vouches for (the key's proof was
/// checked by the router).
#[derive(Serialize, Deserialize)]
pub(crate) struct RegisterAgent {
    pub owner: String,
    pub key: String,
}

impl Call for RegisterAgent {
    const PATH: &'static str = "/agents";
    type Answer = IdentityView;
}

/// A key changed on an identity by `by` (its proof checked by the router).
#[derive(Serialize, Deserialize)]
pub(crate) struct KeyChange {
    pub identity: String,
    pub key: String,
    pub by: String,
}

/// `POST /keys`: add a key.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct AddKey(pub KeyChange);

impl Call for AddKey {
    const PATH: &'static str = "/keys";
    type Answer = IdentityView;
}

/// `POST /revoke`: revoke one.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RevokeKey(pub KeyChange);

impl Call for RevokeKey {
    const PATH: &'static str = "/revoke";
    type Answer = IdentityView;
}

/// `POST /check`: whether `key` is one of `identity`'s active keys, asked
/// by the identity or one of its agents.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct CheckKey(pub KeyChange);

#[derive(Serialize, Deserialize)]
pub(crate) struct Active {
    pub active: bool,
}

impl Call for CheckKey {
    const PATH: &'static str = "/check";
    type Answer = Active;
}

/// `POST /view`: an identity, as it or its owner sees it.
#[derive(Serialize, Deserialize)]
pub(crate) struct View {
    pub identity: String,
    pub by: String,
}

impl Call for View {
    const PATH: &'static str = "/view";
    type Answer = IdentityView;
}

/// `POST /username/claim`: a person's username, chosen once.
#[derive(Serialize, Deserialize)]
pub(crate) struct ClaimUsername {
    pub identity: String,
    pub username: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Claimed {
    pub username: String,
    pub claimed: bool,
}

impl Call for ClaimUsername {
    const PATH: &'static str = "/username/claim";
    type Answer = Claimed;
}

/// `POST /username/lookup`: whoever holds a username, and their picture.
#[derive(Serialize, Deserialize)]
pub(crate) struct FindUsername {
    pub username: String,
}

/// A person's picture: its bytes are `pictures/<sha>` in BLOBS.
#[derive(Serialize, Deserialize)]
pub(crate) struct Picture {
    pub sha: String,
    pub mime: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Holder {
    #[serde(flatten)]
    pub identity: Identity,
    pub picture: Option<Picture>,
}

impl Call for FindUsername {
    const PATH: &'static str = "/username/lookup";
    type Answer = Holder;
    fn checked(answer: Holder) -> CellResult<Holder> {
        Ok(Holder { identity: identity_checked(answer.identity)?, picture: answer.picture })
    }
}

/// `POST /username/release`: an operator's undo of a username taken by
/// mistake (the router has checked its person owns nothing under it).
#[derive(Serialize, Deserialize)]
pub(crate) struct ReleaseUsername {
    pub username: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Released {
    pub username: String,
    pub identity: String,
    pub released: bool,
}

impl Call for ReleaseUsername {
    const PATH: &'static str = "/username/release";
    type Answer = Released;
}

/// `POST /profiles`: what anyone may know of some identities, as a page
/// shows a name (the Registry answers at most 64 at once).
#[derive(Serialize, Deserialize)]
pub(crate) struct Profiles {
    pub ids: Vec<String>,
}

/// A person's username and picture, or an agent's owner's username.
#[derive(Serialize, Deserialize)]
pub(crate) struct Profile {
    pub kind: IdentityKind,
    pub username: Option<String>,
    /// Where the picture is served, on the platform's origin.
    pub picture: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ProfilesAnswer {
    /// By identity; one the registry does not hold is left out.
    pub profiles: BTreeMap<String, Profile>,
}

impl Call for Profiles {
    const PATH: &'static str = "/profiles";
    type Answer = ProfilesAnswer;
}

/// `POST /picture/set`: a person's picture (its bytes already stored).
#[derive(Serialize, Deserialize)]
pub(crate) struct SetPicture {
    pub identity: String,
    pub sha: String,
    pub mime: String,
}

impl Call for SetPicture {
    const PATH: &'static str = "/picture/set";
    type Answer = Picture;
}

/// `POST /test`: dev fleets' controls (`FRAGMENT_TEST_HOOKS=allow`).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TestHook {
    /// Answer 503 to everything else (`true`), or answer again.
    Down(bool),
    /// How many calls the Registry has had since it started (`{calls:
    /// null}`): a test counts a request's round trips by the difference.
    Calls,
    Signins(SigninsHook),
}

/// The controls over sign-in's rows.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SigninsHook {
    /// How many rows each table holds.
    Count,
    /// Every pending sign-in and unspent redemption expires now (sessions stay).
    Expire,
    /// The sweep runs now.
    Sweep,
}

/// How many rows sign-in's tables hold.
#[derive(Serialize, Deserialize)]
pub(crate) struct SigninCounts {
    pub logins: u64,
    pub redemptions: u64,
    pub sessions: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum TestAnswer {
    Down { down: bool },
    Calls { calls: u64 },
    Signins(SigninCounts),
}

impl Call for TestHook {
    const PATH: &'static str = "/test";
    type Answer = TestAnswer;
}

// ------------------------------------------------------------- sign-in

/// `POST /login/begin`: a sign-in starts (`link_to`: a signed-in session
/// adding a second sign-in to its person).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Begin {
    pub return_to: String,
    pub link_to: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Began {
    pub state: String,
}

impl Call for Begin {
    const PATH: &'static str = "/login/begin";
    type Answer = Began;
}

/// `POST /login/exchange`: WorkOS's code, exchanged through KEYS, and the
/// sign-in finished.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exchange {
    pub state: String,
    pub code: String,
    pub client_id: String,
    pub issuer: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exchanged {
    /// The platform session's token.
    pub token: String,
    pub id: String,
    pub created: bool,
    pub linked: bool,
    pub return_to: String,
}

impl Call for Exchange {
    const PATH: &'static str = "/login/exchange";
    type Answer = Exchanged;
}

/// `POST /session`: the identity a live session names (401 when it is
/// not live): the platform's (`fragment` none) or one fragment's.
#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    pub token: String,
    pub fragment: Option<String>,
}

impl Call for Session {
    const PATH: &'static str = "/session";
    type Answer = Identity;
    fn checked(answer: Identity) -> CellResult<Identity> {
        identity_checked(answer)
    }
}

/// `POST /session/end`: a fragment's `__signout`.
#[derive(Serialize, Deserialize)]
pub(crate) struct EndSession {
    pub token: String,
    pub fragment: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Ended {
    pub ended: bool,
}

impl Call for EndSession {
    const PATH: &'static str = "/session/end";
    type Answer = Ended;
}

/// `POST /logout`: the platform session ends, and every site session made
/// from it.
#[derive(Serialize, Deserialize)]
pub(crate) struct Logout {
    pub token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoggedOut {
    /// WorkOS's session, to end there too.
    pub workos_sid: Option<String>,
}

impl Call for Logout {
    const PATH: &'static str = "/logout";
    type Answer = LoggedOut;
}

/// `POST /redeem/mint`: a single-use redemption of a platform session for
/// one fragment's origin.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Mint {
    pub token: String,
    pub fragment: String,
    pub return_to: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Minted {
    pub redeem: String,
}

impl Call for Mint {
    const PATH: &'static str = "/redeem/mint";
    type Answer = Minted;
}

/// `POST /redeem`: a redemption spent on its fragment.
#[derive(Serialize, Deserialize)]
pub(crate) struct Redeem {
    pub redeem: String,
    pub fragment: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Redeemed {
    /// The site session's token.
    pub token: String,
    pub return_to: String,
}

impl Call for Redeem {
    const PATH: &'static str = "/redeem";
    type Answer = Redeemed;
}

/// `POST /cli/add`: a key the signed-in person approved joins them (the
/// router checked the key's own proof).
#[derive(Serialize, Deserialize)]
pub(crate) struct ApproveKey {
    pub token: String,
    pub key: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Approved {
    pub id: String,
    /// The key's npub.
    pub key: String,
    pub added: bool,
}

impl Call for ApproveKey {
    const PATH: &'static str = "/cli/add";
    type Answer = Approved;
}
