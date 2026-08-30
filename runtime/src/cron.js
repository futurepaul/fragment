// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
const MONTHS = { jan: 1, feb: 2, mar: 3, apr: 4, may: 5, jun: 6, jul: 7, aug: 8, sep: 9, oct: 10, nov: 11, dec: 12 };
const DAYS = { sun: 1, mon: 2, tue: 3, wed: 4, thu: 5, fri: 6, sat: 7 };
function parseField(spec, min, max, names = null) {
  if (!spec) throw new Error("cron: empty field");
  const values = /* @__PURE__ */ new Set();
  for (const part of spec.split(",")) {
    const m = part.match(/^(.+?)(?:\/(\d+))?$/);
    if (!m) throw new Error(`cron: bad field part ${part}`);
    let [, range, stepS] = m;
    const step = stepS ? parseInt(stepS, 10) : 1;
    if (step < 1) throw new Error("cron: bad step");
    let lo, hi;
    if (range === "*") {
      if (part.includes(",")) throw new Error("cron: * inside list");
      lo = min;
      hi = max;
    } else if (range.includes("-")) {
      const [a, b] = range.split("-").map((x) => named(x, names));
      if (a === void 0 || b === void 0) throw new Error(`cron: bad range ${range}`);
      if (a > b) throw new Error(`cron: descending range ${range} refused (celld semantics)`);
      lo = a;
      hi = b;
    } else {
      const v = named(range, names);
      if (v === void 0) throw new Error(`cron: bad value ${range}`);
      lo = hi = v;
    }
    if (lo < min || hi > max) throw new Error(`cron: value out of range ${range}`);
    if (stepS && hi - lo + 1 <= 1) {
    }
    for (let v = lo; v <= hi; v += step) values.add(v);
  }
  return values;
}
function named(x, names) {
  if (/^\d+$/.test(x)) return parseInt(x, 10);
  return names ? names[x.toLowerCase()] : void 0;
}
function parseCron(expr) {
  const fields = expr.trim().split(/\s+/);
  if (fields.length !== 5) throw new Error(`cron: need 5 fields, got ${fields.length}`);
  if (fields.some((f) => f.includes("?") || f.includes("L") || f.includes("W") || f.includes("#")))
    throw new Error("cron: L/W/#/? extensions not supported by fragment runtime");
  const minute = parseField(fields[0], 0, 59);
  const hour = parseField(fields[1], 0, 23);
  const dom = parseField(fields[2], 1, 31);
  const month = parseField(fields[3], 1, 12, MONTHS);
  const dowRaw = fields[4];
  if (/(^|[,\-])0([,\-\/]|$)/.test(dowRaw) || dowRaw === "0") throw new Error("cron: day-of-week 0 refused (1=Sunday..7=Saturday)");
  const dow = parseField(dowRaw, 1, 7, DAYS);
  return { minute, hour, dom, month, dow, domAny: fields[2] === "*", dowAny: dowRaw === "*" };
}
function cronMatches(parsed, date) {
  const minute = date.getUTCMinutes();
  const hour = date.getUTCHours();
  const dom = date.getUTCDate();
  const month = date.getUTCMonth() + 1;
  const dowJs = date.getUTCDay();
  const dow = dowJs === 0 ? 1 : dowJs + 1;
  if (!parsed.minute.has(minute) || !parsed.hour.has(hour) || !parsed.month.has(month)) return false;
  const domMatch = parsed.dom.has(dom);
  const dowMatch = parsed.dow.has(dow);
  if (!parsed.domAny && !parsed.dowAny) return domMatch || dowMatch;
  return domMatch && dowMatch;
}
function nextRun(expr, afterMs) {
  const parsed = typeof expr === "string" ? parseCron(expr) : expr;
  let t = Math.floor(afterMs / 6e4) * 6e4 + 6e4;
  const limit = afterMs + 366 * 24 * 3600 * 1e3;
  while (t < limit) {
    if (cronMatches(parsed, new Date(t))) return t;
    t += 6e4;
  }
  return null;
}
export {
  cronMatches,
  nextRun,
  parseCron
};
