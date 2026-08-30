// drop-zone ingest: runs whenever files change on the editor plane
// ("trigger": "sync" — sync pushes, CLI writes). New arrivals under drop/
// are processed into output/. Uses generateText (the platform
// `fragment:ai` module) when the host has an inference key; falls back
// to a plain digest otherwise. Workflow writes never
// re-trigger sync workflows, so writing output/ here is loop-safe.
import { generateText } from "fragment:ai";

export async function run(ctx, input) {
  const changed = input?.sync?.paths || [];
  const arrivals = changed.filter((p) => p.startsWith("drop/"));
  if (!arrivals.length) {
    ctx.log("no arrivals under drop/");
    return { processed: 0 };
  }
  const done = new Set((await ctx.state.get("processed")) || []);
  const outputs = [];
  let n = 0;
  for (const p of arrivals) {
    if (done.has(p)) continue;
    let text;
    try {
      text = await ctx.files.read(p);
    } catch {
      continue; // binary or vanished; skip quietly
    }
    let out;
    try {
      const summary = (await generateText({
        prompt: "Summarize this file in 3 bullets, then list any action items:\n\n" + text.slice(0, 20000),
      })).text;
      out = `# ${p}\n\n_ingested ${new Date().toISOString()}_\n\n${summary}\n`;
    } catch (e) {
      const words = text.split(/\s+/).filter(Boolean).length;
      const heads = text
        .split("\n")
        .filter((l) => /^#{1,3} /.test(l))
        .slice(0, 12)
        .join("\n");
      out =
        `# ${p}\n\n_digest mode (generateText unavailable: ${String(e).slice(0, 120)})_\n\n` +
        `- ${words} words\n\n` +
        (heads ? `## headings\n\n${heads}\n` : "");
    }
    const outPath =
      "output/" + p.replace(/^drop\//, "").replace(/\.[^.]+$/, "") + "-" + Date.now() + ".md";
    await ctx.files.write(outPath, out);
    outputs.push(outPath);
    done.add(p);
    n++;
  }
  await ctx.state.put("processed", [...done].slice(-500));
  // announce outputs to the room (run-scope writes don't re-trigger sync
  // workflows by design, so the ingest notifies the viewers itself)
  if (n > 0) {
    const prev = (await ctx.rooms.getState("vault")) || {};
    await ctx.rooms.setState("vault", { at: Date.now(), paths: outputs, n: (prev.n || 0) + 1 });
  }
  ctx.log(`ingested ${n} file(s)`);
  return { processed: n };
}
