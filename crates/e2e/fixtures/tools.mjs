// A job's text step with tools (docs/api.md, AI): a tool loop with no
// computer (the model asks for a tool, the job runs it, and asks again with
// its result), its text streamed into a channel's draft, and tools as the
// input names them.
import { DurableObject } from "cloudflare:workers";

const DICTIONARY = { shard: "a small piece of something larger", fragment: "a part broken off" };

const LOOKUP = {
  type: "function",
  function: {
    name: "lookup",
    description: "Looks a word up in the dictionary.",
    parameters: { type: "object", properties: { word: { type: "string" } }, required: ["word"] },
  },
};

// At most this many calls a run: each is a step, and a run takes at most 256.
const CALLS_MAX = 4;

export class App extends DurableObject {
  // the model's tool calls answered from the dictionary until it answers in
  // words; with a draft, its text streams there, and the answer is posted
  // under the draft's turn, which replaces it on a page
  async look_up({ question, draft, model }, job) {
    const messages = [{ role: "user", content: question }];
    const answers = [];
    for (let i = 0; i < CALLS_MAX; i++) {
      const r = await job.ai.text({ model, messages, tools: [LOOKUP], draft });
      answers.push(r);
      messages.push(r.message);
      if (r.finish_reason !== "tool_calls") break;
      for (const call of r.message.tool_calls) {
        const { word } = JSON.parse(call.function.arguments);
        messages.push({ role: "tool", tool_call_id: call.id, content: DICTIONARY[word] ?? "no such word" });
      }
    }
    const last = answers[answers.length - 1];
    if (draft) await job.publish(draft.channel, { turn: draft.turn, text: last.text });
    return { text: last.text, answers };
  }

  // tools as the input names them (to be refused, or not), and a prompt
  // repeated past what an input carries
  async tooled({ prompt, repeat, tools, tool_choice, draft }, job) {
    return await job.ai.text({ prompt: prompt.repeat(repeat ?? 1), tools, tool_choice, draft });
  }
}
