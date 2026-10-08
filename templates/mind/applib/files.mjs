// A message's files (docs/optchat.md, "Attachments"): blobs of the mind's
// that a page uploaded (`fragment.blob`) or goose's bridge did, named on a
// `say` record or a reply as docs/chat-records.md's ATTACHMENT, `{sha256,
// name, type, size}`. The log keeps them with their message (its
// `attachments`, JSON), and the memory reads a message with its files: each
// named, and a small text one whole (read by its job with `job.blob`, so
// the compactor, zoom and a turn see it). Pure: no SQLite, no steps.
import { capText } from "./optmem.mjs";

export const FILES_MAX = 8;
const NAME_MAX = 200;
const TYPE_MAX = 100;
// A file the memory reads whole is at most this big (job.blob's head), and
// one message's files are read up to this much in all.
export const READ_MAX = 64 * 1024;
export const READ_TOTAL_MAX = 128 * 1024;

const SHA = /^[0-9a-f]{64}$/;
const TEXT_TYPE =
  /^(?:text\/|application\/(?:json|xml|javascript|ecmascript|x-javascript|yaml|x-yaml|toml|x-toml|x-sh|x-python|sql|graphql|x-ndjson|csv|x-tex|x-subrip|rtf)$|application\/[a-z0-9.-]+\+(?:json|xml)$)/;
const TEXT_NAME =
  /\.(?:txt|text|md|markdown|csv|tsv|json|jsonl|ndjson|ya?ml|toml|ini|cfg|conf|log|xml|svg|html?|css|s?css|less|m?js|cjs|ts|tsx|jsx|py|rb|rs|go|java|kt|swift|c|h|cc|cpp|hpp|cs|php|lua|sh|bash|zsh|fish|sql|graphql|proto|tex|bib|org|rst|adoc|srt|vtt|ics|vcf|diff|patch|env|gitignore|dockerfile|makefile)$/i;

const oneLine = (s) => (typeof s === "string" ? s.replace(/[\u0000-\u001f\u007f]+/g, " ").replace(/\s+/g, " ").trim() : "");
const cutChars = (s, n) => {
  const cps = Array.from(s);
  return cps.length > n ? `${cps.slice(0, n - 1).join("")}…` : s;
};

/// The attachments a record or an operation names, made sure of: at most
/// FILES_MAX, each a SHA-256, a size in bytes, and a name and a type cut to
/// their limits (a type that is no `x/y` is `application/octet-stream`).
/// With `texts`, a file's `text` (what its job read) stays, capped as a
/// logged message is. Answers `{files}`, or `{error}` saying what is wrong.
export function attachmentsOf(list, { texts = false } = {}) {
  if (list === undefined || list === null) return { files: [] };
  if (!Array.isArray(list)) return { error: "attachments is a list of {sha256, name, type, size}" };
  if (list.length > FILES_MAX) return { error: `a message carries at most ${FILES_MAX} attachments` };
  const files = [];
  for (const a of list) {
    if (!a || typeof a !== "object" || typeof a.sha256 !== "string" || !SHA.test(a.sha256)) {
      return { error: "an attachment is {sha256, name, type, size}, its sha256 64 lowercase hex" };
    }
    if (!Number.isSafeInteger(a.size) || a.size < 0) return { error: "an attachment's size is its bytes, a count" };
    if (files.some((f) => f.sha256 === a.sha256)) continue;
    const t = typeof a.type === "string" ? a.type.split(";")[0].trim().toLowerCase() : "";
    const f = {
      sha256: a.sha256,
      name: cutChars(oneLine(a.name) || "file", NAME_MAX),
      type: /^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(t) && t.length <= TYPE_MAX ? t : "application/octet-stream",
      size: a.size,
    };
    if (texts && typeof a.text === "string") f.text = capText(a.text);
    files.push(f);
  }
  return { files };
}

/// A log row's `attachments` (JSON, or null), read back.
export function filesOf(json) {
  if (typeof json !== "string" || !json) return [];
  try {
    const v = JSON.parse(json);
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

/// Whether the memory reads a file whole: small, and text by its type or
/// its name.
export const readable = (f) => f.size <= READ_MAX && (TEXT_TYPE.test(f.type) || TEXT_NAME.test(f.name));

/// Files as a record or a page's query names them: no text.
export const described = (files) => files.map(({ sha256, name, type, size }) => ({ sha256, name, type, size }));

/// A size in words.
export function bytesText(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/// Text fenced as code, with a fence longer than any run of backticks in it.
export function fence(text, lang = "") {
  const run = Math.max(2, ...Array.from(String(text).matchAll(/`+/g), (m) => m[0].length));
  const ticks = "`".repeat(run + 1);
  return `${ticks}${lang}\n${String(text).replace(/^\n+|\s+$/g, "")}\n${ticks}`;
}

/// A message as the memory reads it (the tree's level 0, zoom, a turn's new
/// messages): its words, then each file named, one that was read whole
/// below its name.
export function rendered(text, files) {
  if (!Array.isArray(files) || !files.length) return text;
  const named = files.map((f) => {
    const head = `[file: ${f.name} (${f.type}, ${bytesText(f.size)})]`;
    return typeof f.text === "string" ? `${head}\n${fence(f.text)}` : head;
  });
  return [text, ...named].filter((s) => s !== "").join("\n\n");
}
