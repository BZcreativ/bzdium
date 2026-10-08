// Evaluate JS expressions in the bzdium UI webview via CDP.
// Usage: node cdp-eval.js <expression>
const target = process.argv[2];
const expr = process.argv[3];
const ws = new WebSocket(target);
let id = 0;
const pending = new Map();
function send(method, params = {}) {
  return new Promise((resolve) => {
    const msgId = ++id;
    pending.set(msgId, resolve);
    ws.send(JSON.stringify({ id: msgId, method, params }));
  });
}
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
  }
};
ws.onopen = async () => {
  const r = await send("Runtime.evaluate", {
    expression: expr,
    awaitPromise: true,
    returnByValue: true,
    replMode: true,
  });
  if (r.result && r.result.exceptionDetails) {
    console.log("EXCEPTION:", JSON.stringify(r.result.exceptionDetails, null, 1).slice(0, 1500));
  } else if (r.result && r.result.result) {
    console.log(JSON.stringify(r.result.result.value, null, 1));
  } else {
    console.log(JSON.stringify(r).slice(0, 800));
  }
  ws.close();
  process.exit(0);
};
setTimeout(() => { console.log("TIMEOUT"); process.exit(1); }, 15000);
