// Two-client rooms smoke: A and B join, A sends, B receives; state persists;
// C reconnects later and gets hello with state + history.
const base = process.argv[2]; // ws url incl. ?view= if needed
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);

function client(name) {
  const ws = new WebSocket(base);
  const c = { ws, name, hello: null, msgs: [], state: undefined };
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.type === 'hello') { c.hello = m; log(name, 'hello', `state=${JSON.stringify(m.state)}`, `history=${m.history.length}`); }
    if (m.type === 'msg') { c.msgs.push(m); log(name, 'msg', JSON.stringify(m.data)); }
    if (m.type === 'state') { c.state = m.value; log(name, 'state', JSON.stringify(m.value)); }
    if (m.type === 'presence') log(name, 'presence', m.list.map((p) => p.clientId).join(','));
  };
  c.send = (obj) => ws.send(JSON.stringify(obj));
  c.ready = new Promise((res) => { ws.onopen = res; });
  return c;
}

const A = client('A');
await A.ready;
const B = client('B');
await B.ready;
await new Promise((r) => setTimeout(r, 300));
A.send({ type: 'msg', data: { text: 'hello from A' } });
A.send({ type: 'state:set', value: { note: 'shared doc v1' } });
await new Promise((r) => setTimeout(r, 500));
B.send({ type: 'msg', data: { text: 'reply from B' } });
await new Promise((r) => setTimeout(r, 500));
A.ws.close(); B.ws.close();
await new Promise((r) => setTimeout(r, 300));
log('--- C joins after A/B left');
const C = client('C');
await C.ready;
await new Promise((r) => setTimeout(r, 500));
const ok = C.hello && C.hello.state?.note === 'shared doc v1' && C.hello.history.length >= 2;
log(ok ? 'ROOMS OK' : 'ROOMS FAIL: ' + JSON.stringify(C.hello));
C.ws.close();
process.exit(ok ? 0 : 1);
