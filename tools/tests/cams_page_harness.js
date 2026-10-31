// Runs the script in app/cams_page.html against a fake DOM and walks the page the way a person would.
// Used by test_cams_page.py. Prints one "ok <what>" per check, and exits 1 on the first miss.
const fs = require("fs");
const path = require("path");
const assert = require("assert");

const html = fs.readFileSync(path.join(__dirname, "..", "..", "app", "cams_page.html"), "utf8");
const script = html.match(/<script>([\s\S]*)<\/script>/)[1];

function element(tag) {
  const el = {
    tag, children: [], listeners: {}, parent: null, hidden: false, disabled: false, textContent: "", tabIndex: -1, attrs: {},
    _classes: new Set(),
    get className() { return [...this._classes].join(" "); },
    set className(value) { this._classes = new Set(String(value).split(/\s+/).filter(Boolean)); },
    classList: {
      add: (name) => el._classes.add(name),
      remove: (name) => el._classes.delete(name),
      contains: (name) => el._classes.has(name),
      toggle: (name, force) => {
        const on = force === undefined ? !el._classes.has(name) : force;
        if (on) el._classes.add(name); else el._classes.delete(name);
        return on;
      },
    },
    append(...nodes) { nodes.forEach((node) => { if (node.parent) node.parent.children = node.parent.children.filter((c) => c !== node); node.parent = el; el.children.push(node); }); },
    remove() { if (el.parent) el.parent.children = el.parent.children.filter((c) => c !== el); el.parent = null; },
    setAttribute(name, value) { el.attrs[name] = value; },
    removeAttribute(name) { delete el[name]; },
    addEventListener(type, fn) { (el.listeners[type] ||= []).push(fn); },
    fire(type, event = {}) { (el.listeners[type] || []).forEach((fn) => fn({ preventDefault() {}, ...event })); },
  };
  return el;
}

const ids = {};
for (const id of ["cams", "error", "bar", "empty", "all-on", "all-off"]) ids[id] = element("div");
ids.bar.hidden = true;
ids.empty.hidden = true;

const docListeners = {};
const document = {
  hidden: false,
  getElementById: (id) => ids[id],
  createElement: element,
  addEventListener: (type, fn) => (docListeners[type] ||= []).push(fn),
};
const winListeners = {};
const window = { addEventListener: (type, fn) => (winListeners[type] ||= []).push(fn) };
const location = { href: "/cams" };

let answers = { list: { cams: [{ id: "toby-cam-1", name: "Cam 1" }], error: null }, status: { "toby-cam-1": { online: true, viewers: 0 } } };
async function fetch(url) {
  const body = url.startsWith("/api/cams/status") ? answers.status : answers.list;
  return { status: 200, ok: true, json: async () => body };
}

const timers = [];
const fakeSetTimeout = (fn, ms) => { timers.push({ fn, ms }); return timers.length; };
const fakeClearTimeout = (id) => { if (id && timers[id - 1]) timers[id - 1].fn = null; };
const intervals = [];
const fakeSetInterval = (fn, ms) => { intervals.push({ fn, ms }); return intervals.length; };

new Function("document", "window", "location", "fetch", "setTimeout", "clearTimeout", "setInterval", script)(
  document, window, location, fetch, fakeSetTimeout, fakeClearTimeout, fakeSetInterval,
);

const settle = async () => { for (let i = 0; i < 6; i++) await new Promise((resolve) => setImmediate(resolve)); };
const check = (what, fn) => { fn(); console.log("ok", what); };
const tile = (n = 0) => ids.cams.children[n];
const part = (box, className) => box.children.flatMap(function walk(c) { return [c, ...c.children.flatMap(walk)]; }).find((c) => c._classes.has(className));
const buttonOf = (box) => box.children[0].children[1];
const imgOf = (box) => part(box, "screen").children[1];
const stateOf = (box) => box.children[0].children[0].children[1].children[1].textContent;

(async () => {
  await settle();
  check("one cam makes one tile and hides the all-cams bar", () => {
    assert.equal(ids.cams.children.length, 1);
    assert.equal(ids.bar.hidden, true);
    assert.equal(ids.empty.hidden, true);
    assert.equal(stateOf(tile()), "Ready");
    assert.equal(buttonOf(tile()).textContent, "Watch");
  });

  buttonOf(tile()).fire("click");
  check("Watch starts the stream and shows Connecting", () => {
    assert.match(imgOf(tile()).src, /^\/cams\/toby-cam-1\/stream\?t=\d+$/);
    assert.equal(buttonOf(tile()).textContent, "Stop");
    assert.equal(imgOf(tile()).hidden, false);
    assert.equal(stateOf(tile()), "Connecting");
  });

  imgOf(tile()).fire("load");
  check("the first frame makes it live", () => {
    assert.match(stateOf(tile()), /^Live \d\d:\d\d$/);
    assert.ok(part(tile(), "screen").classList.contains("live"));
  });

  part(tile(), "screen").fire("click");
  check("tapping a live picture fills the screen, and Esc puts it back", () => {
    assert.ok(tile().classList.contains("big"));
    docListeners.keydown.forEach((fn) => fn({ key: "Escape" }));
    assert.ok(!tile().classList.contains("big"));
  });

  let before = timers.length;
  imgOf(tile()).fire("error");
  check("losing the cam retries after a pause and says when", () => {
    assert.equal(timers.length, before + 1);
    assert.equal(timers.at(-1).ms, 2000);
    assert.equal(stateOf(tile()), "Connecting");
    assert.equal(part(tile(), "screen").children[0].textContent, "Reconnecting…");
    assert.match(part(tile(), "err").textContent, /not answering\. Trying again in 2 s\./);
  });
  timers.at(-1).fn();
  check("the retry asks for the stream again", () => assert.match(imgOf(tile()).src, /stream\?t=/));

  const pauses = [];
  for (let i = 0; i < 6; i++) {
    imgOf(tile()).fire("error");
    pauses.push(timers.at(-1).ms);
    timers.at(-1).fn();
  }
  check("it keeps trying, with longer pauses that stop growing at 30 s", () => {
    assert.deepEqual(pauses, [4000, 8000, 15000, 30000, 30000, 30000]);
    assert.equal(buttonOf(tile()).textContent, "Stop");
    assert.equal(tile().children.length, 3);
  });

  imgOf(tile()).fire("load");
  check("a frame clears the complaint and starts the pauses over", () => {
    assert.equal(part(tile(), "err").textContent, "");
    assert.match(stateOf(tile()), /^Live/);
    imgOf(tile()).fire("error");
    assert.equal(timers.at(-1).ms, 2000);
    timers.at(-1).fn();
    imgOf(tile()).fire("load");
  });

  answers.status = { "toby-cam-1": { online: true, viewers: 0 } };
  before = timers.length;
  await intervals.find((i) => i.ms === 5000).fn();
  await settle();
  check("a stream the panel stopped sending is noticed, though the picture gave no error", () => {
    assert.equal(timers.length, before + 1);
    assert.match(part(tile(), "err").textContent, /Trying again/);
  });
  timers.at(-1).fn();
  imgOf(tile()).fire("load");
  answers.status = { "toby-cam-1": { online: true, viewers: 1 } };
  await intervals.find((i) => i.ms === 5000).fn();
  await settle();
  check("while someone is watching, the page checks every 5 s and a counted stream is left alone", () => {
    assert.match(stateOf(tile()), /^Live/);
  });

  buttonOf(tile()).fire("click");
  check("Stop ends the retrying and the stream", () => {
    assert.equal(buttonOf(tile()).textContent, "Watch");
    assert.equal(imgOf(tile()).hidden, true);
  });
  answers.status = { "toby-cam-1": { online: true, viewers: 0 } };

  answers = { list: { cams: [{ id: "toby-cam-1", name: "Cam 1" }, { id: "toby-cam-2", name: "Porch" }], error: null },
    status: { "toby-cam-1": { online: true, viewers: 0 }, "toby-cam-2": { online: false, viewers: 0 } } };
  await intervals.find((i) => i.ms === 20000).fn();
  await settle();
  check("a second cam appears on the next refresh with no reload", () => {
    assert.equal(ids.cams.children.length, 2);
    assert.equal(ids.bar.hidden, false);
    assert.equal(stateOf(tile(1)), "Offline");
    assert.equal(buttonOf(tile(1)).disabled, true);
    assert.equal(tile(1).children[0].children[0].children[0].textContent, "Porch");
  });

  ids["all-on"].fire("click");
  check("Watch all starts the cams that are up and skips the ones that are not", () => {
    assert.equal(buttonOf(tile(0)).textContent, "Stop");
    assert.equal(buttonOf(tile(1)).textContent, "Watch");
  });

  document.hidden = true;
  docListeners.visibilitychange.forEach((fn) => fn());
  check("switching away stops the stream", () => assert.equal(buttonOf(tile(0)).textContent, "Watch"));
  document.hidden = false;
  docListeners.visibilitychange.forEach((fn) => fn());
  check("coming back picks the stream up again", () => assert.equal(buttonOf(tile(0)).textContent, "Stop"));

  ids["all-off"].fire("click");
  answers = { list: { cams: [{ id: "toby-cam-1", name: "Cam 1" }], error: "cams.json: id must be a lowercase slug" }, status: { "toby-cam-1": { online: true, viewers: 0 } } };
  await intervals.find((i) => i.ms === 20000).fn();
  await settle();
  check("a cam removed from the file leaves the page, and a bad edit is shown", () => {
    assert.equal(ids.cams.children.length, 1);
    assert.equal(ids.bar.hidden, true);
    assert.match(ids.error.textContent, /not read: cams\.json: id must be/);
  });

  answers = { list: { cams: [], error: null }, status: {} };
  await intervals.find((i) => i.ms === 20000).fn();
  await settle();
  check("no cams shows how to add one", () => {
    assert.equal(ids.cams.children.length, 0);
    assert.equal(ids.empty.hidden, false);
  });
  process.exit(0);
})().catch((error) => { console.error(error.message); process.exit(1); });
