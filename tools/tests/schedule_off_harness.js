// Loads the light page script and checks that a disabled schedule is shown as off.
// Used by test_h7020_library.py. Prints "ok schedule off" or exits 1.
const fs = require("fs");
const path = require("path");
const assert = require("assert");

const html = fs.readFileSync(path.join(__dirname, "..", "..", "app", "h7020_page.html"), "utf8");
const script = html.match(/<script>([\s\S]*)<\/script>/)[1];

function element() {
  const el = {
    children: [],
    listeners: {},
    attrs: {},
    dataset: {},
    value: "",
    textContent: "",
    hidden: false,
    checked: false,
    max: "",
    type: "",
    parent: null,
    previousElementSibling: { hidden: false },
    replaceChildren() { el.children = []; },
    appendChild(node) { node.parent = el; el.children.push(node); return node; },
    append(...nodes) { nodes.forEach((node) => el.appendChild(node)); },
    querySelectorAll() { return []; },
    addEventListener(type, fn) { (el.listeners[type] ||= []).push(fn); },
    setAttribute(name, value) { el.attrs[name] = String(value); },
    getAttribute(name) { return el.attrs[name] ?? null; },
    fire(type) { (el.listeners[type] || []).forEach((fn) => fn({ preventDefault() {} })); },
  };
  return el;
}

const cache = new Map();
const document = {
  activeElement: null,
  createElement: () => element(),
  querySelector: (sel) => {
    if (!cache.has(sel)) cache.set(sel, element());
    return cache.get(sel);
  },
};
const location = { href: "/lights" };
const calls = [];
const schedule = {
  id: "evening",
  enabled: false,
  time: "19:30",
  days: ["mon"],
  action: { power: false },
};

async function fetch(url, opts = {}) {
  calls.push({ url, method: opts.method || "GET", body: opts.body });
  const payloads = {
    "/api/state": {},
    "/api/capabilities": { bulbs: 1 },
    "/api/scenes": { scenes: [] },
    "/api/patterns": { patterns: [] },
    "/api/schedules": { schedules: [schedule] },
  };
  return { status: 200, ok: true, json: async () => payloads[url] || {} };
}

new Function("document", "location", "fetch", "setInterval", script)(document, location, fetch, () => 0);

(async () => {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setImmediate(resolve));
  const list = cache.get("#schedule-list");
  assert.equal(list.children.length, 1);
  const item = list.children[0];
  assert.match(item.textContent, /\(off\)/);
  const turnOn = item.children.find((child) => child.textContent === "Turn on");
  assert.ok(turnOn, "missing Turn on");
  turnOn.fire("click");
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setImmediate(resolve));
  const put = calls.find((call) => call.method === "PUT" && call.url === "/api/schedules/evening");
  assert.ok(put, "missing PUT");
  assert.equal(JSON.parse(put.body).enabled, true);
  console.log("ok schedule off");
})().catch((error) => {
  console.error(error.stack || error.message);
  process.exit(1);
});
