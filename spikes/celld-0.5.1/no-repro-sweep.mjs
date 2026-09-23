// For each delay d, arm "now", wait d ms, arm "now" again, then check that
// the alarm ran twice within 2 s (the second arm must not be lost behind
// the handler's own +4 s re-arm).
const base = "http://127.0.0.1:8795";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let lost = 0, total = 0;
const results = [];
for (let round = 0; round < 2; round++) {
  for (let d = 0; d <= 40; d += 4) {
    const c = `s${Date.now()}_${d}_${round}`;
    await fetch(`${base}/arm?c=${c}`);
    await sleep(d);
    await fetch(`${base}/arm?c=${c}`);
    await sleep(2000);
    const st = await (await fetch(`${base}/?c=${c}`)).json();
    total++;
    if (st.rows.length < 2) { lost++; results.push({ d, rows: st.rows.length, alarmIn: st.alarm ? st.alarm - st.now : null }); }
  }
}
console.log(JSON.stringify({ total, lost, results }));
