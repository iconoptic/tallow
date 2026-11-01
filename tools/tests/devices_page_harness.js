// Runs the script in app/devices_page.html against a fake DOM and walks the page the way a person would.
// Used by test_devices_page.py. Prints one "ok <what>" per check, and exits 1 on the first miss.
const fs = require("fs");
const path = require("path");
const assert = require("assert");

const html = fs.readFileSync(path.join(__dirname, "..", "..", "app", "devices_page.html"), "utf8");
const script = html.match(/<script>([\s\S]*)<\/script>/)[1];

// Every element the page creates, by tag, so a check can see what the page built.
const created = [];
// Any write to innerHTML. The page must never make one: its map text comes from the air.
let innerHtmlWrites = 0;

function element(tag) {
  created.push(tag);
  const el = {
    tag, children: [], listeners: {}, parent: null, hidden: false, textContent: "", attrs: {},
    _classes: new Set(),
    get className() { return [...this._classes].join(" "); },
    set className(value) { this._classes = new Set(String(value).split(/\s+/).filter(Boolean)); },
    classList: {
      add: (name) => el._classes.add(name),
      remove: (name) => el._classes.delete(name),
      contains: (name) => el._classes.has(name),
    },
    append(...nodes) { nodes.forEach((node) => { if (node.parent) node.parent.children = node.parent.children.filter((c) => c !== node); node.parent = el; el.children.push(node); }); },
    replaceChildren(...nodes) { el.children.forEach((c) => { c.parent = null; }); el.children = []; el.append(...nodes); },
    setAttribute(name, value) { el.attrs[name] = value; },
    addEventListener(type, fn) { (el.listeners[type] ||= []).push(fn); },
  };
  Object.defineProperty(el, "innerHTML", { set() { innerHtmlWrites += 1; }, get() { return ""; } });
  return el;
}

const ids = {};
for (const id of ["error", "stale", "reason", "live", "stamp", "devices", "no-devices", "nodes", "no-nodes", "identities", "no-identities", "suggestions", "no-suggestions"]) ids[id] = element("div");
ids.stale.hidden = true;
ids.live.hidden = true;
ids["no-devices"].hidden = true;
ids["no-nodes"].hidden = true;
ids["no-identities"].hidden = true;
ids["no-suggestions"].hidden = true;

const docListeners = {};
const document = {
  hidden: false,
  getElementById: (id) => ids[id],
  createElement: element,
  addEventListener: (type, fn) => (docListeners[type] ||= []).push(fn),
};
const location = { href: "/devices" };

// The first refresh reads this: no map file yet.
let answer = { status: 200, body: { stale: true, reason: "missing", now: 1000000 } };
async function fetch(url) {
  if (answer.error) throw new Error(answer.error);
  return { status: answer.status, ok: answer.status >= 200 && answer.status < 300, json: async () => answer.body };
}

const intervals = [];
const fakeSetInterval = (fn, ms) => { intervals.push({ fn, ms }); return intervals.length; };

new Function("document", "location", "fetch", "setInterval", script)(document, location, fetch, fakeSetInterval);

const settle = async () => { for (let i = 0; i < 8; i++) await new Promise((resolve) => setImmediate(resolve)); };
// A check runs to the end before its "ok". An async body would print "ok" first, so it is refused.
const check = (what, fn) => { assert.ok(!(fn() instanceof Promise), `${what}: await first, then check`); console.log("ok", what); };
const walk = (node) => [node, ...node.children.flatMap(walk)];
const textOf = (node) => walk(node).map((n) => n.textContent).join("\n");
const cardWith = (list, text) => list.children.find((card) => textOf(card).includes(text));
const refreshNow = async () => { await intervals[0].fn(); await settle(); };

// The map as tallowd writes it, with one of each case the page must show.
const NAME_FROM_THE_AIR = "<img src=x onerror=alert(1)>";
const LIVE = {
  stale: false,
  now: 1000000,
  age_s: 3,
  nodes: {
    "0a0b0c0d": { rank: 0, heap_free: 90112, heap_min: 40960, hops: 0, parent: 0, seen: 999997, link_rssi: 0 },
    "0a0b0c0e": { rank: 2, heap_free: 81920, heap_min: 30720, hops: 2, parent: 0x0a0b0c0d, seen: 999998, link_rssi: -60 },
  },
  devices: {
    "41:22:13:00:00:01 ble-random": {
      addr: "41:22:13:00:00:01", kind: "ble-random",
      heard_by: { "0a0b0c0e": { rssi: -70, hits: 4, last: 999990 } }, nearest: "0a0b0c0e",
    },
    "AA:BB:CC:DD:EE:01 classic": {
      addr: "AA:BB:CC:DD:EE:01", kind: "classic", name: "Kitchen speaker", room: "Kitchen",
      heard_by: { "0a0b0c0e": { rssi: -127, hits: 2, last: 999980 } }, nearest: "0a0b0c0e",
    },
    "41:22:13:00:00:02 ble-random": {
      addr: "41:22:13:00:00:02", kind: "ble-random", resolved: "Earbuds",
      heard_by: { "0a0b0c0e": { rssi: -55, hits: 9, last: 999995 } }, nearest: "0a0b0c0e", room: "Hall",
    },
    "AA:BB:CC:DD:EE:02 classic": {
      addr: "AA:BB:CC:DD:EE:02", kind: "classic", name: NAME_FROM_THE_AIR,
      heard_by: { "0a0b0c0e": { rssi: -61, hits: 1, last: 999960 } }, nearest: "0a0b0c0e",
    },
  },
  identities: {
    "Klipsch soundbar": {
      type: "speaker",
      keys: ["23:00:00:11:A0:38 classic", "DC:00:00:11:A0:38 ble-public"],
    },
    [NAME_FROM_THE_AIR]: {
      type: "speaker",
      keys: [NAME_FROM_THE_AIR, "AA:BB:CC:DD:EE:99 classic"],
    },
  },
  suggestions: [
    {
      keys: ["AA:BB:CC:DD:EE:10 classic", "AA:BB:CC:DD:EE:11 ble-public"],
      reason: NAME_FROM_THE_AIR,
    },
  ],
};

(async () => {
  await settle();
  check("no map file shows the empty state, says it is not running, and gives the command", () => {
    assert.equal(ids.stale.hidden, false);
    assert.equal(ids.live.hidden, true);
    // The static text lives in the page source, which this fake DOM does not parse.
    assert.match(html, /<strong>The map is not running\.<\/strong>/);
    assert.match(html, /<code>tools\/mesh\.sh daemon run &lt;port&gt; --out data\/mesh\/map\.json<\/code>/);
    assert.equal(ids.reason.textContent, "No map file has been written yet.");
    assert.equal(ids.devices.children.length, 0);
  });

  answer = { status: 200, body: { stale: true, reason: "old", age_s: 120, now: 1000000 } };
  await refreshNow();
  check("an old map says how long ago it was written", () => {
    assert.equal(ids.reason.textContent, "The map was last written 2 min ago.");
  });

  answer = { status: 200, body: LIVE };
  await refreshNow();
  check("a live map hides the empty state and shows the map", () => {
    assert.equal(ids.stale.hidden, true);
    assert.equal(ids.live.hidden, false);
    assert.equal(ids.stamp.textContent, "Map written 3 s ago.");
    assert.equal(ids.devices.children.length, 4);
    assert.equal(ids.nodes.children.length, 2);
    assert.equal(ids["no-devices"].hidden, true);
    assert.equal(ids["no-nodes"].hidden, true);
  });

  check("a device with no room says so", () => {
    const card = cardWith(ids.devices, "AA:BB:CC:DD:EE:02");
    assert.ok(card, "the hostile device has a card");
    assert.match(textOf(card), /Room\nNo room yet/);
    const withRoom = cardWith(ids.devices, "Kitchen speaker");
    assert.match(textOf(withRoom), /Room\nKitchen/);
  });

  check("a node with no parent says so, and a node with one shows its parent", () => {
    const root = cardWith(ids.nodes, "0a0b0c0d");
    assert.match(textOf(root), /Parent\nno parent/);
    assert.match(textOf(root), /Rank\n0/);
    assert.match(textOf(root), /Heap\n88 KB free, lowest 40 KB/);
    const child = cardWith(ids.nodes, "0a0b0c0e");
    assert.match(textOf(child), /Parent\n0a0b0c0d/);
    assert.match(textOf(child), /Rank\n2/);
  });

  check("a rotating address is named as a changing address, not a stable identity", () => {
    const card = cardWith(ids.devices, "41:22:13:00:00:01");
    assert.match(textOf(card), /Changing address/);
    assert.match(textOf(card), /not a stable identity/);
    assert.doesNotMatch(textOf(card), /Unnamed device/);
  });

  check("a resolved address shows its name and no changing-address note", () => {
    const card = cardWith(ids.devices, "Earbuds");
    assert.ok(card);
    assert.doesNotMatch(textOf(card), /Changing address/);
    assert.doesNotMatch(textOf(card), /not a stable identity/);
  });

  check("a Classic page at -127 shows as paged, not as a distance", () => {
    const card = cardWith(ids.devices, "Kitchen speaker");
    assert.match(textOf(card), /Nearest\n0a0b0c0e, paged, no signal strength/);
    assert.match(textOf(card), /Heard by\n0a0b0c0e paged, no signal strength/);
    assert.doesNotMatch(textOf(card), /-127/);
  });

  check("a named identity shows its type and both addresses", () => {
    const card = cardWith(ids.identities, "Klipsch soundbar");
    assert.ok(card, "the named identity has a card");
    assert.equal(card.children[0].textContent, "Klipsch soundbar");
    assert.match(textOf(card), /Type\nspeaker/);
    assert.match(textOf(card), /23:00:00:11:A0:38 classic/);
    assert.match(textOf(card), /DC:00:00:11:A0:38 ble-public/);
    assert.equal(ids["no-identities"].hidden, true);
  });

  check("a suggestion is labelled as a suggestion and offers nothing to accept", () => {
    assert.match(html, /These are suggestions\. Nothing here accepts or merges them\./);
    const card = cardWith(ids.suggestions, "AA:BB:CC:DD:EE:10 classic");
    assert.ok(card, "the suggestion has a card");
    assert.equal(card.children[0].textContent, "Suggestion");
    assert.match(textOf(card), /AA:BB:CC:DD:EE:11 ble-public/);
    assert.equal(created.filter((tag) => tag === "button").length, 0, "nothing to accept or merge");
    assert.equal(ids["no-suggestions"].hidden, true);
  });

  check("a hostile identity name, key, and suggestion reason are text", () => {
    const named = cardWith(ids.identities, NAME_FROM_THE_AIR);
    assert.ok(named, "the hostile identity has a card");
    assert.equal(named.children[0].textContent, NAME_FROM_THE_AIR);
    assert.match(textOf(named), new RegExp(NAME_FROM_THE_AIR.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    const suggestion = cardWith(ids.suggestions, "Suggestion");
    const reason = suggestion.children[1].children.find((node) => node.textContent === NAME_FROM_THE_AIR);
    assert.ok(reason, "the suggestion reason is text");
    assert.equal(reason.textContent, NAME_FROM_THE_AIR);
    assert.equal(created.filter((tag) => tag === "img").length, 0, "no img element was built");
    assert.equal(innerHtmlWrites, 0, "innerHTML was never set");
  });

  check("a name from the air is text, not markup", () => {
    const card = cardWith(ids.devices, "AA:BB:CC:DD:EE:02");
    const title = card.children[0];
    assert.equal(title.tag, "h3");
    assert.equal(title.textContent, NAME_FROM_THE_AIR);
    assert.equal(created.filter((tag) => tag === "img").length, 0, "no img element was built");
    assert.equal(innerHtmlWrites, 0, "innerHTML was never set");
  });

  answer = { error: "network down" };
  await refreshNow();
  check("a fetch that fails leaves the last view and says the panel did not answer", () => {
    assert.equal(ids.error.textContent, "The panel did not answer.");
    assert.equal(ids.live.hidden, false);
  });

  answer = { status: 401, body: null };
  await refreshNow();
  check("a login that has run out sends the page to login", () => {
    assert.equal(location.href, "/login");
  });

  check("the page never wrote markup", () => assert.equal(innerHtmlWrites, 0));
  process.exit(0);
})().catch((error) => { console.error(error.message); process.exit(1); });
