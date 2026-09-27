// A model's answer to one of Stagehand's calls (browser-mcp.mjs), read as
// the value its JSON schema asks for. Answers come wrapped (markdown fences,
// words or a number before the JSON, reasoning after it), and some leave out
// a schema's only field: its bare value (Observation's elements as a bare
// array), or, where that field is text (an extraction), plain words. It
// throws, saying why, when nothing in the answer fits. No dependencies, so a
// test runs it alone (crates/templates).

// what an answer's JSON is looked for in, at most
const TEXT_MAX = 256 * 1024;

export function answer(text, schema) {
  const said = unfenced(String(text ?? "").slice(0, TEXT_MAX));
  if (!said) throw new Error("an empty answer");
  const values = found(said);
  const fields = schema.type === "object" ? Object.keys(schema.properties ?? {}) : [];
  const one = fields.length === 1 ? fields[0] : null;
  const fitting = values.flatMap((v) => [v, ...(one ? [{ [one]: v }] : [])]).filter((v) => fits(v, schema));
  // the last: what a model says after its reasoning is its answer
  if (fitting.length) return fitting.at(-1);
  // words, where the one field is text and the answer is not JSON-shaped
  if (one && schema.properties[one].type === "string" && !/^[[{"]/.test(said)) return { [one]: said };
  throw new Error(`${values.length ? "does not fit" : "not JSON"}: ${said.slice(0, 200)}`);
}

// the answer, less reasoning in <think> tags and the fences around it
function unfenced(text) {
  return text
    .replace(/<think>[\s\S]*?<\/think>/g, "")
    .trim()
    .replace(/^```[a-z]*\s*\n?|\n?\s*```$/gi, "")
    .trim();
}

// the JSON values in a text, in order: all of it, or each object or array
// in it that parses (one inside another is part of it)
function found(text) {
  try {
    return [JSON.parse(text)];
  } catch {}
  const values = [];
  for (let i = 0; i < text.length; i++) {
    if (text[i] !== "{" && text[i] !== "[") continue;
    const end = closing(text, i);
    try {
      values.push(JSON.parse(text.slice(i, end + 1)));
      i = end;
    } catch {}
  }
  return values;
}

// where the object or array that opens at `i` closes, strings minded (-1: never)
function closing(text, i) {
  let [depth, quoted] = [0, false];
  for (let j = i; j < text.length; j++) {
    const c = text[j];
    if (quoted) {
      if (c === "\\") j++;
      else if (c === '"') quoted = false;
    } else if (c === '"') quoted = true;
    else if (c === "{" || c === "[") depth++;
    else if ((c === "}" || c === "]") && --depth === 0) return j;
  }
  return -1;
}

// whether a value fits a JSON schema, as far as Stagehand's go
export function fits(v, s) {
  if (s.anyOf) return s.anyOf.some((x) => fits(v, x));
  if (Array.isArray(s.type)) return s.type.some((type) => fits(v, { ...s, type }));
  if (s.enum && !s.enum.includes(v)) return false;
  switch (s.type) {
    case "object": {
      const known = ([k, x]) => (s.properties?.[k] ? fits(x, s.properties[k]) : s.additionalProperties !== false);
      return v !== null && typeof v === "object" && !Array.isArray(v) && (s.required ?? []).every((k) => k in v) && Object.entries(v).every(known);
    }
    case "array":
      return Array.isArray(v) && v.every((x) => !s.items || fits(x, s.items));
    case "string":
      return typeof v === "string" && (!s.pattern || new RegExp(s.pattern).test(v));
    case "integer":
      return Number.isInteger(v);
    case "number":
      return typeof v === "number";
    case "boolean":
      return typeof v === "boolean";
    case "null":
      return v === null;
    default:
      return true;
  }
}
