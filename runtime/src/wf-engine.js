// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
const WF_PARAM_LIMIT = 64 * 1024;
const WF_REPORT_TRIES = 5;
const NPUB_RE = /^npub1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{58}$/;
function wfInstanceId(fragmentNpub, runId, attempt) {
  if (!NPUB_RE.test(String(fragmentNpub))) throw new Error(`corrupt state: fragment npub ${JSON.stringify(fragmentNpub)} is not a bech32 npub`);
  if (!(Number.isSafeInteger(runId) && runId > 0)) throw new Error(`corrupt state: run id ${runId} is not a positive integer`);
  if (!(Number.isSafeInteger(attempt) && attempt > 0)) throw new Error(`corrupt state: attempt ${attempt} is not a positive integer`);
  const id = `f${fragmentNpub.slice(5, 21)}r${runId}a${attempt}`;
  if (!(id.length <= 100 && /^[a-z0-9]+$/.test(id))) throw new Error(`instance id ${id} is outside the Workflows id alphabet`);
  return id;
}
async function runNativeAttempt(event, step, env) {
  const call = async (route, body) => {
    const headers = {
      "content-type": "application/json",
      "x-fragment-token": event.token
    };
    if (event.hostSecret) headers["x-fragment-host-secret"] = event.hostSecret;
    const base = String(env.FRAGMENT_INTERNAL_URL || "http://127.0.0.1:8789");
    const resp = await fetch(`${base}/__internal/f/${encodeURIComponent(event.fragment)}${route}`, {
      method: "POST",
      headers,
      body: JSON.stringify(body)
    });
    if (!resp.ok) throw new Error(`wf ${route} -> ${resp.status}: ${(await resp.text()).slice(0, 300)}`);
    return await resp.json();
  };
  const outcome = await step.do(`attempt`, async () => {
    return await call("/wf/attempt", { runId: event.runId, attempt: event.attempt });
  });
  for (let t = 1; t <= WF_REPORT_TRIES; t++) {
    try {
      await step.do(`report:${t}`, async () => {
        await call("/wf/complete", { runId: event.runId, attempt: event.attempt, outcome });
      });
      return outcome;
    } catch (e) {
      if (t === WF_REPORT_TRIES) throw e;
      await step.sleep(`report-backoff:${t}`, `${Math.min(5 * t, 30)} seconds`);
    }
  }
  throw new Error("unreachable: report retry loop exhausted");
}
async function launchNativeRun(cell, runId, attempt) {
  const binding = cell.env.WORKFLOWS;
  if (!binding || typeof binding.create !== "function") {
    throw new Error("native Workflows binding (WORKFLOWS) missing on this host \u2014 deploy the runtime with its wrangler workflows config");
  }
  const name = cell.getMeta("name");
  const token = cell.makeToken({ kind: "wf-run", runId, attempt });
  const event = {
    fragment: name,
    runId,
    attempt,
    token,
    ...cell.env.FRAGMENT_HOST_SECRET ? { hostSecret: String(cell.env.FRAGMENT_HOST_SECRET) } : {}
  };
  const encoded = JSON.stringify(event);
  if (encoded.length > WF_PARAM_LIMIT) throw new Error("workflow params exceed sanity limit");
  const id = wfInstanceId(cell.getMeta("fragment_npub"), runId, attempt);
  await binding.create({ id, params: JSON.parse(encoded) });
  return id;
}
async function nativeStatus(cell, instanceId) {
  const binding = cell.env.WORKFLOWS;
  if (!binding || typeof binding.get !== "function") return null;
  try {
    const inst = await binding.get(instanceId);
    return await inst.status();
  } catch {
    return null;
  }
}
const MANUAL_AWAIT_MS = 3e5;
const POLL_TICK_MS = 500;
async function awaitNativeRun(cell, instanceId) {
  const deadline = Date.now() + MANUAL_AWAIT_MS;
  for (; ; ) {
    const st = await nativeStatus(cell, instanceId);
    if (st && (st.status === "complete" || st.status === "errored" || st.status === "terminated")) {
      return { status: st.status === "complete" ? "complete" : "errored" };
    }
    if (Date.now() + POLL_TICK_MS > deadline) return { status: "timeout" };
    await new Promise((r) => setTimeout(r, POLL_TICK_MS));
  }
}
export {
  MANUAL_AWAIT_MS,
  WF_PARAM_LIMIT,
  WF_REPORT_TRIES,
  awaitNativeRun,
  launchNativeRun,
  nativeStatus,
  runNativeAttempt,
  wfInstanceId
};
