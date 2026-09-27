//! The templates under `templates/`, embedded: `fragment new` scaffolds
//! them, and the cell makes a fragment from one (docs/phase-6.md, step 2).

/// A template's files: path (relative to the fragment's root) and bytes.
pub type Template = &'static [(&'static str, &'static [u8])];

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

#[cfg(test)]
mod tests {
    #[test]
    fn every_template_has_a_manifest() {
        for (name, files) in super::ALL {
            assert!(files.iter().any(|(p, _)| *p == "fragment.json"), "{name} has no fragment.json");
        }
    }

    /// The pet's browser reads models' answers to Stagehand
    /// (templates/pet/computer/browser/answer.mjs, run by Node here), as
    /// they came on Paul's pet (2026-09-27, `browser.log`): each of the
    /// first three failed there, `JSON.parse` saying what its line says.
    #[test]
    fn the_pet_browser_reads_the_answers_that_failed() {
        let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/pet/computer/browser/answer.mjs");
        let script = r#"
import assert from "node:assert/strict";
const { answer } = await import(new URL(`file://${process.argv[1]}`));
const text = { type: "string" };
const extraction = { type: "object", properties: { extraction: text }, required: ["extraction"], additionalProperties: false };
const element = { type: "object", properties: { elementId: { type: "string", pattern: "^\\d+-\\d+$" }, description: text, method: { type: "string", enum: ["click", "fill"] }, arguments: { type: "array", items: text } }, required: ["elementId", "description", "method", "arguments"], additionalProperties: false };
const observation = { type: "object", properties: { elements: { type: "array", items: element } }, required: ["elements"], additionalProperties: false };
const metadata = { type: "object", properties: { progress: text, completed: { type: "boolean" } }, required: ["progress", "completed"], additionalProperties: false };
const act = { type: "object", properties: { action: { anyOf: [element, { type: "null" }] }, twoStep: { type: "boolean" } }, required: ["action", "twoStep"], additionalProperties: false };
const top3 = "1. Show HN: A thing (example.com)\n2. [2024] Another story\n3. A third";

// flashx's Extraction, twice: "Unterminated fractional number in JSON at position 2"
assert.throws(() => JSON.parse(top3), SyntaxError);
assert.deepEqual(answer(top3, extraction), { extraction: top3 });
// Jev's, from stealth/space-bunny-alpha: "No number after minus sign in JSON at position 1"
assert.throws(() => JSON.parse("- A story\n- Another"), SyntaxError);
assert.deepEqual(answer("- A story\n- Another", extraction), { extraction: "- A story\n- Another" });
// flashx's Observation, a bare array of elements: "does not fit"
const clicks = [{ elementId: "0-21", description: "the Add button", method: "click", arguments: [] }];
assert.deepEqual(answer(JSON.stringify(clicks), observation), { elements: clicks });
assert.deepEqual(answer("[]", observation), { elements: [] });

// JSON wrapped: fences, words or a number before it, reasoning, a bare value
assert.deepEqual(answer('```json\n{"extraction": "$3.49"}\n```', extraction), { extraction: "$3.49" });
assert.deepEqual(answer('Here it is:\n{"extraction": "$3.49"} (from the page)', extraction), { extraction: "$3.49" });
assert.deepEqual(answer('2. {"progress": "found it", "completed": true}', metadata), { progress: "found it", completed: true });
assert.deepEqual(answer('<think>maybe {"progress": "", "completed": false}</think>\n{"progress": "done", "completed": true}', metadata), { progress: "done", completed: true });
assert.deepEqual(answer('"$3.49"', extraction), { extraction: "$3.49" });
assert.deepEqual(answer('{"action": null, "twoStep": false}', act), { action: null, twoStep: false });
assert.deepEqual(answer(`{"action": ${JSON.stringify(clicks[0])}, "twoStep": false}`, act), { action: clicks[0], twoStep: false });

// what does not fit is refused, so the next model answers
assert.throws(() => answer('{"price": 3.49}', extraction), /^Error: does not fit: \{"price"/);
assert.throws(() => answer("It is done.", metadata), /^Error: not JSON: It is done/);
assert.throws(() => answer('[{"elementId": "[0-21]", "description": "Add", "method": "click", "arguments": []}]', observation), /does not fit/);
assert.throws(() => answer('{"action": {"elementId": "0-21"}, "twoStep": false}', act), /does not fit/);
assert.throws(() => answer("  ", extraction), /an empty answer/);
assert.throws(() => answer(undefined, extraction), /an empty answer/);
"#;
        let out = std::process::Command::new("node").args(["--input-type=module", "-e", script]).arg(&module).output().expect("node, which the pet's computer runs");
        assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }
}
