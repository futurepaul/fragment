//! The Registry's inner routes as types: each call's path, its body, and
//! its answer, defined once. The router and fragments ask with them
//! (`crate::ask_registry`), and the Registry decodes the same body and
//! answers the same type (`RegistryCell::route`), so the two ends agree at
//! compile time.

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

/// Who asks the Registry to act, resolved in the same turn as the act:
/// one round trip, and a key revoked a moment before cannot act.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum By {
    /// The key that signed the request (64 hex; the router checked the
    /// signature): 401 when no one holds it, or it was revoked.
    Key(String),
    /// A platform session's token (a browser on the platform's pages): 401
    /// when it is not live.
    Session(String),
    /// An identity the platform already resolved (an agent made for its
    /// owner): 404 when there is none.
    Identity(String),
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
    pub owner: By,
    pub key: String,
}

impl Call for RegisterAgent {
    const PATH: &'static str = "/agents";
    type Answer = IdentityView;
}

/// A key changed on an identity (`None`: the asker's own) by `by` (its
/// proof checked by the router).
#[derive(Serialize, Deserialize)]
pub(crate) struct KeyChange {
    pub identity: Option<String>,
    pub key: String,
    pub by: By,
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

/// `POST /view`: an identity (`None`: the asker's own), as it or its
/// owner sees it.
#[derive(Serialize, Deserialize)]
pub(crate) struct View {
    pub identity: Option<String>,
    pub by: By,
}

impl Call for View {
    const PATH: &'static str = "/view";
    type Answer = IdentityView;
}

/// `POST /username/claim`: the asker's username, chosen once.
#[derive(Serialize, Deserialize)]
pub(crate) struct ClaimUsername {
    pub by: By,
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

/// `POST /picture/set`: the asker's picture (its bytes already stored).
#[derive(Serialize, Deserialize)]
pub(crate) struct SetPicture {
    pub by: By,
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
    /// The next call waits this many milliseconds (at most
    /// `TEST_HOLD_MAX_MS`) before it is answered: a test lets other
    /// requests run while a fragment waits on the Registry.
    Hold(u32),
    Signins(SigninsHook),
}

/// The longest `TestHook::Hold`.
pub(crate) const TEST_HOLD_MAX_MS: u32 = 10_000;

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
    /// The session this token names expires now (its row stays for the
    /// sweep; a platform session's site sessions end with it).
    ExpireSession(String),
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SigninCounts {
    pub logins: u64,
    pub redemptions: u64,
    pub sessions: u64,
}

impl Call for TestHook {
    const PATH: &'static str = "/test";
    /// The hook's setting, `{down}`, `{calls}`, or `{hold}`, or `SigninCounts`.
    type Answer = serde_json::Value;
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
    pub return_to: String,
}

impl Call for Exchange {
    const PATH: &'static str = "/login/exchange";
    type Answer = Exchanged;
}

/// `POST /session`: the identity a live session names (401 when it is
/// not live): the platform's (`fragment` none) or one fragment's, a
/// frame's (`frame`: made in a frame, for the page that framed it) or a
/// top-level one. A token of the other kind is not live.
#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    pub token: String,
    pub fragment: Option<String>,
    #[serde(default)]
    pub frame: bool,
}

/// Whom a live session names, and the email of their first sign-in (the
/// platform's pages show it; `None` when there is none).
#[derive(Serialize, Deserialize)]
pub(crate) struct LiveSession {
    #[serde(flatten)]
    pub identity: Identity,
    pub email: Option<String>,
    /// A frame's session: the origin of the page that framed it
    /// (`__frame`), the only page its answers may show in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedder: Option<String>,
}

impl Call for Session {
    const PATH: &'static str = "/session";
    type Answer = LiveSession;
    fn checked(answer: LiveSession) -> CellResult<LiveSession> {
        Ok(LiveSession { identity: identity_checked(answer.identity)?, ..answer })
    }
}

/// `POST /session/end`: a fragment's `__signout`: the sessions its
/// cookies carry end, and the person's yes to it is forgotten.
#[derive(Serialize, Deserialize)]
pub(crate) struct EndSession {
    pub site: Option<String>,
    pub frame: Option<String>,
    pub fragment: String,
}

impl Call for EndSession {
    const PATH: &'static str = "/session/end";
    type Answer = ();
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

/// Why a sign-in may tell a fragment who the person is.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Consent {
    /// Only if they said yes to it before: else nothing is minted.
    Remembered,
    /// They are in it (the router asked the fragment): it knows them.
    Member,
    /// They say yes now: remembered from here on.
    Given,
}

/// `POST /redeem/mint`: a single-use redemption of a platform session for
/// one fragment's origin, when `consent` allows it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Mint {
    pub token: String,
    pub fragment: String,
    pub return_to: String,
    pub consent: Consent,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Minted {
    /// `None`: the person has not said yes to this fragment.
    pub redeem: Option<String>,
    /// Whom the session names.
    pub identity: Identity,
}

impl Call for Mint {
    const PATH: &'static str = "/redeem/mint";
    type Answer = Minted;
    fn checked(answer: Minted) -> CellResult<Minted> {
        Ok(Minted { identity: identity_checked(answer.identity)?, ..answer })
    }
}

/// `POST /redeem/frame`: a frame redemption (`__frame`): from the session
/// `token` names on `from` (a site or frame session, `frame` says which),
/// which must be `owner`'s, for `fragment` in a frame of `embedder` only.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MintFrame {
    pub token: String,
    pub frame: bool,
    pub from: String,
    pub owner: String,
    pub fragment: String,
    pub embedder: String,
    pub return_to: String,
}

impl Call for MintFrame {
    const PATH: &'static str = "/redeem/frame";
    type Answer = Minted;
    fn checked(answer: Minted) -> CellResult<Minted> {
        Mint::checked(answer)
    }
}

/// `POST /redeem`: a redemption spent on its fragment, shown to a frame
/// (`framed`) or to a top-level page: a frame redemption only to a frame,
/// any other only to a top-level page (else refused, and spent).
#[derive(Serialize, Deserialize)]
pub(crate) struct Redeem {
    pub redeem: String,
    pub fragment: String,
    pub framed: bool,
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

impl Call for ApproveKey {
    const PATH: &'static str = "/cli/add";
    type Answer = ();
}
