//! What a section needs to run (`Suite::section`), so a run's set is
//! chosen by where it runs, never by a hand-kept list. A local run has
//! everything; a hosted one (a branch deployment on real vendors) has the
//! levers its test secret opens, Chrome when it is installed, and the
//! computers and models its deployment offers, and nothing that stands in
//! for a vendor or controls the node.

/// One thing a section needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Need {
    /// A vendor fake it writes or scripts: code.storage's git or webhooks,
    /// the scripted model, WorkOS beyond sign-in (Pipes' accounts), the
    /// push service, the swap's upstream, a local server a job fetches.
    Fakes,
    /// The node itself: crashed, restarted, or started with settings of its
    /// own (another shape, test-sized limits, a path-mode fleet), or its
    /// local address and clock.
    Node,
    /// The whole deployment: a lever on the registry (held down, slowed,
    /// its sign-ins filled), its operator's key, or its own secrets. On a
    /// shared preview these reach everyone.
    Deployment,
    /// `/api/test/*` levers on the section's own fragments, people and
    /// ledgers: a preview has them when deployed with a test secret.
    Levers,
    /// Docker on this machine.
    LocalDocker,
    /// Headless Chrome on this machine.
    Chrome,
    /// Computers: the stub image locally, the deployment's own (Hermes) hosted.
    Computers,
    /// Real models through the deployment's gateway: hosted, each call is
    /// paid, from the run's budget (`--max-paid-calls`).
    Models,
    /// The platform on a site of its own, cross-site from the fragments (as
    /// fragment.club is from fragment.boats, and the local node's
    /// 127.0.0.1 from `*.fragment.localhost`). A branch preview puts both
    /// in one zone: one site.
    TwoSites,
}

impl Need {
    pub const ALL: [Need; 9] = [Need::Fakes, Need::Node, Need::Deployment, Need::Levers, Need::LocalDocker, Need::Chrome, Need::Computers, Need::Models, Need::TwoSites];

    pub fn name(self) -> &'static str {
        match self {
            Need::Fakes => "fakes",
            Need::Node => "node",
            Need::Deployment => "deployment",
            Need::Levers => "levers",
            Need::LocalDocker => "local-docker",
            Need::Chrome => "chrome",
            Need::Computers => "computers",
            Need::Models => "models",
            Need::TwoSites => "two-sites",
        }
    }
}

/// What a hosted run's deployment and machine offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offers {
    /// The run carries the deployment's test secret.
    pub levers: bool,
    /// The deployment makes computers (its config's `computers`).
    pub computers: bool,
    /// The deployment calls models (its config's `ai_gateway`), and the run
    /// may lend paid calls.
    pub models: bool,
    /// Chrome is installed here.
    pub chrome: bool,
}

/// Where a run runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Under `wrangler dev`, with the fakes: everything.
    Local,
    /// Under `celld dev`, with the fakes (docs/self-host.md, seam 1):
    /// everything but the runtime's own containers, which a self-hosted
    /// deployment places on a sandcastle node.
    Celld,
    /// A branch deployment on real vendors.
    Hosted(Offers),
}

/// Why `need` is missing on `rung`, or `None` when it is there.
pub fn missing(need: Need, rung: Rung) -> Option<&'static str> {
    let offers = match rung {
        Rung::Local => return None,
        Rung::Celld => {
            return matches!(need, Need::LocalDocker | Need::Computers)
                .then_some("it needs the runtime's containers, and celld runs none: a self-hosted deployment's computers are a sandcastle node's (docs/self-host.md, seam 2)");
        }
        Rung::Hosted(offers) => offers,
    };
    match need {
        Need::Fakes => Some("it needs the vendor fakes (a scripted model, code.storage's git, WorkOS' accounts, the push service, a local upstream), which only a local run has"),
        Need::Node => Some("it needs the node itself (a crash, a restart, settings of its own, its local address or clock), which only a local run has"),
        Need::Deployment => Some("it needs the whole deployment (a lever on its registry, its operator's key, its secrets), which a shared preview never lends a run"),
        Need::LocalDocker => Some("it needs Docker on this machine (the stub image's scripted runtime), which a hosted run does not use"),
        Need::TwoSites => Some("it needs the platform cross-site from the fragments, and a branch preview puts both in one zone"),
        Need::Levers if !offers.levers => Some("it needs the deployment's test levers, and the run has no test secret"),
        Need::Chrome if !offers.chrome => Some("it needs Chrome, and none is installed here"),
        Need::Computers if !offers.computers => Some("it needs computers, and the deployment makes none (no `computers` in its config)"),
        Need::Models if !offers.models => Some("it needs models, and the deployment calls none (no `ai_gateway` in its config), or the run lends no paid calls"),
        Need::Levers | Need::Chrome | Need::Computers | Need::Models => None,
    }
}

/// The first of `needs` missing on `rung`, with why (`None`: the section
/// runs). A hosted run without the levers runs nothing: its people sign in
/// through them.
pub fn unmet(needs: &[Need], rung: Rung) -> Option<(Need, &'static str)> {
    if let Rung::Hosted(Offers { levers: false, .. }) = rung {
        return Some((Need::Levers, "the run has no test secret, and a hosted run's people sign in through the levers"));
    }
    needs.iter().find_map(|need| missing(*need, rung).map(|why| (*need, why)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERYTHING: Offers = Offers { levers: true, computers: true, models: true, chrome: true };
    const NOTHING: Offers = Offers { levers: false, computers: false, models: false, chrome: false };

    /// A local run has everything: every section's needs are met, as before.
    #[test]
    fn a_local_run_has_everything() {
        assert_eq!(unmet(&Need::ALL, Rung::Local), None);
        for need in Need::ALL {
            assert_eq!(missing(need, Rung::Local), None, "{need:?}");
        }
    }

    /// A hosted run never has what stands in for a vendor or controls the
    /// node, the deployment, or local Docker, whatever its deployment offers.
    #[test]
    fn a_hosted_run_never_has_the_fakes_the_node_or_the_deployment() {
        for need in [Need::Fakes, Need::Node, Need::Deployment, Need::LocalDocker, Need::TwoSites] {
            assert!(missing(need, Rung::Hosted(EVERYTHING)).is_some(), "{need:?}");
            assert_eq!(unmet(&[Need::Levers, need], Rung::Hosted(EVERYTHING)).map(|(n, _)| n), Some(need));
        }
    }

    /// Levers, Chrome, computers and models are there when offered.
    #[test]
    fn a_hosted_run_has_what_its_deployment_offers() {
        for need in [Need::Levers, Need::Chrome, Need::Computers, Need::Models] {
            assert_eq!(missing(need, Rung::Hosted(EVERYTHING)), None, "{need:?}");
            assert!(missing(need, Rung::Hosted(NOTHING)).is_some(), "{need:?}");
        }
        let computers_only = Offers { levers: true, computers: true, ..NOTHING };
        assert_eq!(unmet(&[Need::Computers], Rung::Hosted(computers_only)), None);
        assert_eq!(unmet(&[Need::Computers, Need::Models], Rung::Hosted(computers_only)).map(|(n, _)| n), Some(Need::Models));
        // a section that needs nothing runs anywhere people can sign in
        assert_eq!(unmet(&[], Rung::Hosted(Offers { levers: true, ..NOTHING })), None);
    }

    /// Without the test secret no one signs in on a preview: nothing runs,
    /// whatever it declares.
    #[test]
    fn a_hosted_run_without_levers_runs_nothing() {
        let no_secret = Offers { levers: false, ..EVERYTHING };
        assert_eq!(unmet(&[], Rung::Hosted(no_secret)).map(|(n, _)| n), Some(Need::Levers));
        assert_eq!(unmet(&[Need::Computers], Rung::Hosted(no_secret)).map(|(n, _)| n), Some(Need::Levers));
        assert_eq!(unmet(&[], Rung::Local), None);
    }

    /// The first need missing is the one said, in the order declared.
    #[test]
    fn the_first_unmet_need_is_said() {
        let (need, why) = unmet(&[Need::Levers, Need::Fakes, Need::Node], Rung::Hosted(EVERYTHING)).unwrap();
        assert_eq!(need, Need::Fakes);
        assert!(why.contains("fakes"), "{why}");
        assert_eq!(Need::ALL.iter().map(|n| n.name()).collect::<std::collections::BTreeSet<_>>().len(), Need::ALL.len(), "names are distinct");
    }
}
