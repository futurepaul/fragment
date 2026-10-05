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
    /// The agent fragment it is made from (a computer's agent, whose key is
    /// its fragment's own), which names it: `None` for an agent of a CLI's
    /// or of a fragment's `agent` block.
    #[serde(default)]
    pub fragment: Option<String>,
}

impl Call for RegisterAgent {
    const PATH: &'static str = "/agents";
    type Answer = IdentityView;
}

/// `POST /held`: an agent held below its owner, or let go (`held: None`),
/// by its owner (decision 36).
#[derive(Serialize, Deserialize)]
pub(crate) struct Hold {
    pub agent: String,
    pub held: Option<fragment_proto::Role>,
    pub by: By,
}

impl Call for Hold {
    const PATH: &'static str = "/held";
    type Answer = IdentityView;
}

/// `POST /subject`: the subject a person first signed in as with
/// `issuer` (their WorkOS user, whose connections a computer swaps in), or
/// none.
#[derive(Serialize, Deserialize)]
pub(crate) struct SubjectOf {
    pub identity: String,
    pub issuer: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SubjectAnswer {
    pub subject: Option<String>,
}

impl Call for SubjectOf {
    const PATH: &'static str = "/subject";
    type Answer = SubjectAnswer;
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

/// A person's username and picture, or an agent's owner's username, and
/// an agent made from an agent fragment's name (its label) and fragment.
#[derive(Serialize, Deserialize)]
pub(crate) struct Profile {
    pub kind: IdentityKind,
    pub username: Option<String>,
    /// Where the picture is served, on the platform's origin.
    pub picture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
    /// An agent's fragment's title (`XBT-2000`), filled by the fragment a
    /// page asks (serve.rs `__people`), never by the registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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

/// `POST /test`: the levers of a fleet with a test secret
/// (`FRAGMENT_TEST_SECRET`; cell/src/levers.rs).
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
    /// An e2e person (`<name>@e2e.test`, the e2e issuer) signed in: a new
    /// platform session, and the person made the first time
    /// (`E2eSignedIn`); on a branch, within the day's caps.
    E2eSignIn(E2eSignIn),
    /// The e2e people, by identity, a page at a time (`E2ePeopleAnswer`).
    E2ePeople(E2ePeople),
    /// Whether an identity is an e2e person (`{e2e}`): a branch's ledger
    /// levers reach no one else.
    E2eIs(String),
    /// A person as a sign-in keeps them, made without one (`KeptPerson`):
    /// the e2e's stand-in for the people a sign-in made before it changed
    /// (docs/self-host.md, seam 4: WorkOS's, `(workos:<client id>, user id)`).
    Person(KeptPerson),
}

/// `TestHook::Person`: their `(issuer, subject)`, and their email. Answers
/// `{identity, created}`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeptPerson {
    pub issuer: String,
    pub subject: String,
    pub email: String,
}

/// `TestHook::E2eSignIn`: who, and the paid calls they are lent (the
/// day's count on a branch).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct E2eSignIn {
    pub email: String,
    pub paid_calls: u64,
}

/// `TestHook::E2eSignIn`'s answer.
#[derive(Serialize, Deserialize)]
pub(crate) struct E2eSignedIn {
    pub token: String,
    pub identity: String,
    pub created: bool,
}

/// `TestHook::E2ePeople`: the page after `after` (an identity), or the first.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct E2ePeople {
    #[serde(default)]
    pub after: Option<String>,
}

/// One e2e person, as the sweep finds them.
#[derive(Serialize, Deserialize)]
pub(crate) struct E2ePerson {
    pub identity: String,
    pub email: String,
}

/// A page of e2e people (at most `E2E_PEOPLE_PAGE`), and the identity to
/// ask after for the next (`None`: this was the last).
#[derive(Serialize, Deserialize)]
pub(crate) struct E2ePeopleAnswer {
    pub people: Vec<E2ePerson>,
    pub next: Option<String>,
}

/// The e2e people one page lists.
pub(crate) const E2E_PEOPLE_PAGE: u32 = 100;

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
/// adding a second sign-in to its person), with a PKCE verifier and a
/// nonce, which the registry keeps with it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Begin {
    pub return_to: String,
    pub link_to: Option<String>,
}

/// What the authorization request carries: the state, the verifier's S256
/// challenge (the verifier stays in the registry), and the nonce the
/// id_token must carry back.
#[derive(Serialize, Deserialize)]
pub(crate) struct Began {
    pub state: String,
    pub challenge: String,
    pub nonce: String,
}

impl Call for Begin {
    const PATH: &'static str = "/login/begin";
    type Answer = Began;
}

/// `POST /login/exchange`: the provider's code, exchanged with the
/// sign-in's verifier and the client's secret (KEYS), its id_token
/// verified, and the sign-in finished. `redirect_uri` is the one the
/// authorization request named.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exchange {
    pub state: String,
    pub code: String,
    pub redirect_uri: String,
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
/// not live): the platform's (`fragment` none) or one fragment's (or one
/// share sheet's: `sheet`), a frame's (`frame`: made in a frame, for the
/// page that framed it) or a top-level one. A token of the other kind is
/// not live.
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
    /// A frame's session: the origin of the page that framed it, the only
    /// page its answers may show in.
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
    /// The session's id_token: the hint its provider's logout takes
    /// (RP-Initiated Logout 1.0); none for a session made before sign-in
    /// was OpenID Connect, or one whose seal no longer opens.
    pub id_token: Option<String>,
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
    /// A frame redemption's: the origin of the page whose frame redeems it
    /// (the platform's own, `/auth/frame`), the only page the fragment's
    /// answers to that frame may show in. `None`: a top-level one.
    #[serde(default)]
    pub embedder: Option<String>,
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

// ---- people's own nodes (bring your own computer, experimental: nodes.rs) ----

/// `POST /nodes/pair/begin`: a node asks to be paired (unsigned: the router
/// read its body; the registry refuses it where BYOC is off).
#[derive(Serialize, Deserialize)]
pub(crate) struct PairBegin {
    pub name: String,
    pub arch: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PairBegun {
    pub user_code: String,
    pub device_code: String,
    pub expires_at: i64,
    pub interval_s: u32,
}

impl Call for PairBegin {
    const PATH: &'static str = "/nodes/pair/begin";
    type Answer = PairBegun;
}

/// `POST /nodes/pair/show`: the pairing a person's code names, for its
/// page, their platform session checked in the same turn (a wrong code is
/// counted against them).
#[derive(Serialize, Deserialize)]
pub(crate) struct PairShow {
    pub token: String,
    pub code: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PairShown {
    pub user_code: String,
    pub name: String,
    pub arch: String,
    pub expires_at: i64,
}

impl Call for PairShow {
    const PATH: &'static str = "/nodes/pair/show";
    type Answer = PairShown;
}

/// `POST /nodes/pair/approve`: the person approves the pairing their code
/// names (the router checked the form came from the platform's own page),
/// and, when they say so (`prefer`), chooses the node for their new
/// computers in the same step: a person with no computer yet gets their
/// first one there (the shell starts it as they pick a username, before
/// settings could be reached).
#[derive(Serialize, Deserialize)]
pub(crate) struct PairApprove {
    pub token: String,
    pub code: String,
    #[serde(default)]
    pub prefer: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PairApproved {
    pub node: String,
    pub name: String,
}

impl Call for PairApprove {
    const PATH: &'static str = "/nodes/pair/approve";
    type Answer = PairApproved;
}

/// `POST /nodes/pair/poll`: a node's poll, by its device code.
#[derive(Serialize, Deserialize)]
pub(crate) struct PairPoll {
    pub device_code: String,
}

impl Call for PairPoll {
    const PATH: &'static str = "/nodes/pair/poll";
    type Answer = fragment_proto::nodes::PairPolled;
}

/// One of a person's own nodes.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct OwnNode {
    pub id: String,
    pub name: String,
    pub arch: String,
    pub paired_at: i64,
    pub revoked_at: Option<i64>,
}

/// A person's own nodes, the node they chose, and whether BYOC is on.
#[derive(Serialize, Deserialize)]
pub(crate) struct OwnNodesAnswer {
    pub byoc: bool,
    pub nodes: Vec<OwnNode>,
    pub prefer: Option<String>,
}

/// `POST /nodes/own`: a person's own nodes (`owner`: resolved by the router).
#[derive(Serialize, Deserialize)]
pub(crate) struct OwnNodes {
    pub owner: String,
}

impl Call for OwnNodes {
    const PATH: &'static str = "/nodes/own";
    type Answer = OwnNodesAnswer;
}

/// A person's node as the platform's own code needs it: its secret while it
/// is live (`None` once revoked). Never an API's answer.
#[derive(Serialize, Deserialize)]
pub(crate) struct PairedRecord {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub arch: String,
    pub revoked: bool,
    pub secret: Option<String>,
}

/// `POST /nodes/node`: one person's node by id (the uplink's dial, a
/// computer's container on it), or none.
#[derive(Serialize, Deserialize)]
pub(crate) struct PairedNode {
    pub id: String,
}

impl Call for PairedNode {
    const PATH: &'static str = "/nodes/node";
    type Answer = Option<PairedRecord>;
}

/// `POST /nodes/revoke`: its owner revokes a node of theirs.
#[derive(Serialize, Deserialize)]
pub(crate) struct RevokeNode {
    pub owner: String,
    pub id: String,
}

impl Call for RevokeNode {
    const PATH: &'static str = "/nodes/revoke";
    type Answer = OwnNodesAnswer;
}

/// `POST /nodes/prefer`: where a person's new computers run (`None`: the
/// deployment's rule).
#[derive(Serialize, Deserialize)]
pub(crate) struct PreferNode {
    pub owner: String,
    pub node: Option<String>,
}

impl Call for PreferNode {
    const PATH: &'static str = "/nodes/prefer";
    type Answer = OwnNodesAnswer;
}

/// `POST /nodes/choice`: what placing a person's new computer needs.
#[derive(Serialize, Deserialize)]
pub(crate) struct ChoiceOf {
    pub owner: String,
}

/// The node they chose (`None`: the deployment's rule); its record when it
/// is theirs and still usable; why not, when it is theirs and is not.
#[derive(Serialize, Deserialize)]
pub(crate) struct ChoiceAnswer {
    pub prefer: Option<String>,
    pub own: Option<PairedRecord>,
    pub gone: Option<String>,
}

impl Call for ChoiceOf {
    const PATH: &'static str = "/nodes/choice";
    type Answer = ChoiceAnswer;
}
