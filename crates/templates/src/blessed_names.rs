/// The templates the platform serves from its release (decision 40): the
/// cell makes a fragment of one and runs its release's code, and no one
/// scaffolds one (`fragment create --template chat` names it). The build
/// reads this list too (build.rs): without the `blessed` feature, as the CLI
/// builds, it indexes and embeds the other templates alone.
pub const BLESSED: [&str; 4] = ["agent", "chat", "brain", "skills"];
