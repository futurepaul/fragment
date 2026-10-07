//! The templates under `templates/`, embedded: `fragment new` scaffolds
//! them, and the cell makes a fragment from one (docs/api.md, Control API).

/// A template's files: path (relative to the fragment's root) and bytes.
pub type Template = &'static [(&'static str, &'static [u8])];

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

pub mod blessed;

#[cfg(test)]
mod tests {
    #[test]
    fn every_template_has_a_manifest() {
        for (name, files) in super::ALL {
            assert!(files.iter().any(|(p, _)| *p == "fragment.json"), "{name} has no fragment.json");
        }
    }
}
