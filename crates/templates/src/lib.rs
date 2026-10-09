//! The templates under `templates/`, embedded: `fragment new` scaffolds
//! them, and the cell makes a fragment from one (docs/api.md, Control API).

/// A template's files: path (relative to the fragment's root) and bytes.
pub type Template = &'static [(&'static str, &'static [u8])];

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

pub mod blessed;

#[cfg(test)]
#[path = "../../../images/hermes/boot/src/skill_references.rs"]
mod skill_references;

#[cfg(test)]
mod tests {
    /// Every text file beneath a managed skill resolves its references
    /// through the release's files, just as Hermes' skill_view does.
    #[test]
    fn every_managed_skill_reference_exists() {
        let mut missing = std::collections::BTreeSet::new();
        for (skill, _) in super::SKILLS.iter().filter(|(p, _)| p.ends_with("/SKILL.md")) {
            let root = skill.strip_suffix("SKILL.md").unwrap();
            for (path, bytes) in super::SKILLS.iter().filter(|(p, _)| p.starts_with(root)) {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    for reference in super::skill_references::paths(text) {
                        let target = format!("{root}{reference}");
                        if !super::SKILLS.iter().any(|(p, _)| *p == target || p.starts_with(&format!("{}/", target.trim_end_matches('/')))) {
                            missing.insert(format!("{path} mentions missing {reference}"));
                        }
                    }
                }
            }
        }
        assert!(missing.is_empty(), "{}", missing.into_iter().collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn every_template_has_a_manifest() {
        for (name, files) in super::ALL {
            assert!(files.iter().any(|(p, _)| *p == "fragment.json"), "{name} has no fragment.json");
        }
    }
}
