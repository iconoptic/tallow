// Runs the script in app/traffic_page.html against a fake DOM.
// Used by test_traffic_page.py. Prints one "ok <what>" per check, and exits 1 on the first miss.
const fs = require("fs");
const path = require("path");
const assert = require("assert");

const html = fs.readFileSync(path.join(__dirname, "..", "..", "app", "traffic_page.html"), "utf8");
const scripts = [...html.matchAll(/<script([^>]*)>([\s\S]*?)<\/script>/g)];
const inline = scripts.find((match) => !match[1].includes("src"));
assert.ok(inline, "the page has an inline script");
const script = inline[2];

let innerHtmlWrites = 0;
const created = [];

function element(tag) {
  created.push(tag);
  const el = {
    tag,
    children: [],
    listeners: {},
    parent: null,
    hidden: false,
    textContent: "",
    checked: false,
    attrs: {},
    append(...nodes) {
      nodes.forEach((node) => {
        if (node && node.parent) node.parent.children = node.parent.children.filter((child) => child !== node);
        if (node) {
          node.parent = el;
          el.children.push(node);
        }
      });
    },
    replaceChildren(...nodes) {
      el.children.forEach((child) => {
        child.parent = null;
      });
      el.children = [];
      el.append(...nodes);
    },
    setAttribute(name, value) {
      el.attrs[name] = String(value);
    },
    addEventListener(type, fn) {
      (el.listeners[type] ||= []).push(fn);
    },
    getBoundingClientRect() {
      return { left: 0, top: 0, width: 1000, height: 640 };
    },
  };
  Object.defineProperty(el, "innerHTML", {
    set() {
      innerHtmlWrites += 1;
    },
    get() {
      return "";
    },
  });
  return el;
}

const ids = {};
for (const id of ["error", "summary", "map", "band", "lanes", "empty", "win-live", "win-hour", "win-day", "passes", "clear"]) {
  ids[id] = element(id === "map" ? "svg" : "div");
}
ids.empty.hidden = true;

const document = {
  getElementById: (id) => ids[id],
  createElement: element,
  createElementNS: (_ns, tag) => element(tag),
};

const NOW = 1_700_000_000_000;
const HOSTILE = "<img src=x onerror=alert(1)>";
const FIXTURE = {
  now: NOW,
  from: NOW - 15 * 60 * 1000,
  to: NOW,
  truncated: false,
  nodes: [
    { id: "aaaaaaaa", label: "Kitchen", x: 0.2, y: 0.5 },
    { id: "bbbbbbbb", label: "Hall", x: 0.8, y: 0.5 },
  ],
  tracks: [
    {
      id: "device Speaker",
      label: "Speaker",
      class: "move",
      named: true,
      stays: [
        { node: "aaaaaaaa", start: NOW - 20000, end: NOW - 10000, rssi: -60 },
        { node: "bbbbbbbb", start: NOW - 10000, end: NOW, rssi: -40 },
      ],
      samples: [
        { t: NOW - 5000, node: "aaaaaaaa", rssi: -50 },
        { t: NOW - 5000, node: "bbbbbbbb", rssi: -60 },
      ],
    },
    {
      id: "BB:BB:BB:BB:BB:02 ble-random",
      label: "BB:BB:BB:BB:BB:02",
      class: "pass",
      named: false,
      stays: [{ node: "aaaaaaaa", start: NOW - 80000, end: NOW - 80000, rssi: -75 }],
    },
    {
      id: "hostile",
      label: HOSTILE,
      class: "linger",
      named: true,
      stays: [{ node: "bbbbbbbb", start: NOW - 5000, end: NOW, rssi: -50 }],
    },
  ],
  edges: [{ from: "aaaaaaaa", to: "bbbbbbbb", count: 1 }],
};

const calls = [];
let answer = FIXTURE;
let clearStatus = 200;
async function fetch(url, options) {
  calls.push({ url: String(url), options: options || {} });
  const method = (options && options.method) || "GET";
  if (method === "POST" && String(url).endsWith("/api/traffic/clear")) {
    return { ok: clearStatus >= 200 && clearStatus < 300, status: clearStatus, json: async () => ({ ok: true }) };
  }
  if (method === "POST") return { ok: true, status: 200, json: async () => ({ ok: true }) };
  if (answer.error) throw new Error(answer.error);
  return { ok: true, status: 200, json: async () => answer };
}

const intervals = [];
const fakeSetInterval = (fn, ms) => {
  intervals.push({ fn, ms });
  return intervals.length;
};

let confirmed = true;
function confirm() {
  return confirmed;
}

class DataSet {
  constructor(items) {
    this.items = (items || []).slice();
  }
  add(items) {
    this.items = this.items.concat(items);
  }
  clear() {
    this.items = [];
  }
}

const timelines = [];
class Timeline {
  constructor(el, items, groups, options) {
    this.el = el;
    this.items = items;
    this.groups = groups;
    this.options = options;
    this.handlers = {};
    timelines.push(this);
  }
  on(name, fn) {
    this.handlers[name] = fn;
  }
  addCustomTime() {}
  setCustomTime() {}
}
const vis = { DataSet, Timeline };

const settle = async () => {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setImmediate(resolve));
};
const check = (what, fn) => {
  assert.ok(!(fn() instanceof Promise), `${what}: await first, then check`);
  console.log("ok", what);
};
const walk = (node) => [node, ...(node.children || []).flatMap(walk)];
const textOf = (node) => walk(node).map((child) => child.textContent || "").join("\n");

new Function("document", "fetch", "setInterval", "confirm", "vis", script)(
  document,
  fetch,
  fakeSetInterval,
  confirm,
  vis,
);

(async () => {
  await settle();
  check("the move and the hostile name are text on the map", () => {
    const map = textOf(ids.map);
    assert.ok(map.includes("Speaker"), map);
    assert.ok(map.includes(HOSTILE), map);
    assert.equal(innerHtmlWrites, 0);
    assert.ok(!created.includes("img"), created.join(","));
  });
  check("a pass-by is folded into one band", () => {
    assert.ok(ids.band.textContent.includes("1 passed by"), ids.band.textContent);
    const groups = timelines[0].groups.items.map((group) => group.content);
    assert.ok(groups.includes("Speaker"), groups.join("|"));
    assert.ok(groups.includes("Passed by"), groups.join("|"));
    assert.ok(!groups.includes("BB:BB:BB:BB:BB:02"), groups.join("|"));
    assert.ok(groups.some((group) => group.includes("&lt;img")), groups.join("|"));
  });
  check("showing one-ping addresses gives that address its own lane", () => {
    ids.passes.checked = true;
    ids.passes.listeners.change.forEach((fn) => fn());
    const groups = timelines[0].groups.items.map((group) => group.content);
    assert.ok(groups.includes("BB:BB:BB:BB:BB:02"), groups.join("|"));
    assert.equal(ids.band.textContent, "");
  });
  check("the summary counts stays, moves, and pass-bys", () => {
    assert.ok(ids.summary.textContent.includes("1 staying"), ids.summary.textContent);
    assert.ok(ids.summary.textContent.includes("1 moving"), ids.summary.textContent);
    assert.ok(ids.summary.textContent.includes("1 passed by"), ids.summary.textContent);
  });
  check("a dot heard by two nodes sits between them, and one listener stays beside its node", () => {
    const dots = ids.map.children.filter((child) => child.tag === "circle" && !child.attrs["data-node"]);
    const between = dots.find((dot) => {
      const x = Number(dot.attrs.cx);
      return x > 250 && x < 750;
    });
    assert.ok(between, dots.map((dot) => dot.attrs.cx).join(","));
    const beside = dots.find((dot) => {
      const x = Number(dot.attrs.cx);
      const y = Number(dot.attrs.cy);
      return Math.abs(Math.hypot(x - 800, y - 320) - 36) < 1;
    });
    assert.ok(beside, dots.map((dot) => `${dot.attrs.cx},${dot.attrs.cy}`).join(" "));
    const orbit = 800 + Math.cos(0.8) * 40;
    assert.ok(Math.abs(Number(beside.attrs.cx) - orbit) > 1);
  });
  check("an arc joins the two nodes of a move", () => {
    const path = ids.map.children.find((child) => child.tag === "path");
    assert.ok(path, "no arc");
    assert.ok(path.attrs.d.includes("200") || path.attrs.d.includes("800"), path.attrs.d);
  });
  check("dragging a node saves the layout", () => {
    const circle = ids.map.children.find((child) => child.attrs && child.attrs["data-node"] === "aaaaaaaa");
    assert.ok(circle, "no kitchen node");
    circle.listeners.pointerdown.forEach((fn) => fn({ target: circle, pointerId: 1, clientX: 0, clientY: 0 }));
    ids.map.listeners.pointermove.forEach((fn) => fn({ clientX: 250, clientY: 320 }));
    ids.map.listeners.pointerup.forEach((fn) => fn({}));
    const post = calls.filter((call) => call.options.method === "POST" && call.url === "/api/traffic/layout").pop();
    assert.ok(post, "no layout post");
    const body = JSON.parse(post.options.body);
    assert.ok(Math.abs(body.nodes.aaaaaaaa.x - 0.25) < 0.02, JSON.stringify(body));
    assert.ok(Math.abs(body.nodes.aaaaaaaa.y - 0.5) < 0.02, JSON.stringify(body));
  });
  await ids["win-hour"].listeners.click[0]();
  await settle();
  check("the hour button asks for that window", () => {
    assert.ok(calls.some((call) => call.url.includes("window=hour")), calls.map((call) => call.url).join(" "));
  });
  confirmed = false;
  ids.clear.listeners.click[0]();
  check("clear asks first and does not post when refused", () => {
    assert.ok(!calls.some((call) => call.url === "/api/traffic/clear"));
  });
  confirmed = true;
  await ids.clear.listeners.click[0]();
  await settle();
  check("clear posts and reads the log again", () => {
    assert.ok(calls.some((call) => call.options.method === "POST" && call.url === "/api/traffic/clear"));
  });
  check("the page never writes markup of its own", () => {
    assert.equal(innerHtmlWrites, 0);
    assert.equal(intervals[0].ms, 5000);
  });

  function modelRssi(node, device) {
    const distance = Math.hypot(node.x - device.x, (node.y - device.y) * 0.64);
    return -45 - 27 * Math.log10(distance);
  }

  function placed(listeners, device) {
    const t = NOW;
    return {
      now: t,
      from: t - 60_000,
      to: t,
      truncated: false,
      nodes: listeners.map((node, index) => ({
        id: "n" + index,
        label: "n" + index,
        x: node.x,
        y: node.y,
      })),
      tracks: [
        {
          id: "device Dot",
          label: "Dot",
          class: "linger",
          named: true,
          samples: listeners.map((node, index) => ({
            t: t,
            node: "n" + index,
            rssi: modelRssi(node, device),
          })),
          stays: listeners.map((node, index) => ({
            node: "n" + index,
            start: t - 1000,
            end: t,
            rssi: modelRssi(node, device),
          })),
        },
      ],
      edges: [],
    };
  }

  function trackDot() {
    return ids.map.children.find((child) => child.tag === "circle" && !child.attrs["data-node"]);
  }

  answer = placed(
    [
      { x: 0.1, y: 0.1 },
      { x: 0.9, y: 0.1 },
      { x: 0.5, y: 0.9 },
    ],
    { x: 0.3, y: 0.3 },
  );
  await ids["win-live"].listeners.click[0]();
  await settle();
  check("three listeners place a noiseless dot", () => {
    const dot = trackDot();
    assert.ok(dot, "no dot");
    const x = Number(dot.attrs.cx);
    const y = Number(dot.attrs.cy);
    assert.ok(Math.hypot(x - 300, y - 192) < 10, `${x},${y}`);
  });

  answer = placed(
    [
      { x: 0.1, y: 0.1 },
      { x: 0.9, y: 0.1 },
      { x: 0.5, y: 0.9 },
      { x: 0.1, y: 0.9 },
    ],
    { x: 0.5, y: 0.7 },
  );
  await ids["win-live"].listeners.click[0]();
  await settle();
  check("four listeners place a noiseless dot", () => {
    const dot = trackDot();
    assert.ok(dot, "no dot");
    const x = Number(dot.attrs.cx);
    const y = Number(dot.attrs.cy);
    assert.ok(Math.hypot(x - 500, y - 448) < 10, `${x},${y}`);
  });

  const firstTo = NOW + 60_000;
  answer = {
    now: firstTo,
    from: firstTo - 60_000,
    to: firstTo,
    truncated: false,
    nodes: [{ id: "n0", label: "n0", x: 0.2, y: 0.5 }],
    tracks: [
      {
        id: "early",
        label: "Early",
        class: "linger",
        named: true,
        samples: [{ t: firstTo, node: "n0", rssi: -40 }],
        stays: [{ node: "n0", start: firstTo - 1000, end: firstTo, rssi: -40 }],
      },
    ],
    edges: [],
  };
  await ids["win-live"].listeners.click[0]();
  await settle();
  const secondTo = firstTo + 30_000;
  answer = {
    now: secondTo,
    from: secondTo - 60_000,
    to: secondTo,
    truncated: false,
    nodes: [{ id: "n0", label: "n0", x: 0.2, y: 0.5 }],
    tracks: [
      {
        id: "late",
        label: "Late",
        class: "linger",
        named: true,
        samples: [{ t: firstTo + 15_000, node: "n0", rssi: -40 }],
        stays: [{ node: "n0", start: firstTo + 14_000, end: firstTo + 15_000, rssi: -40 }],
      },
    ],
    edges: [],
  };
  await ids["win-live"].listeners.click[0]();
  await settle();
  check("a live refresh follows a device heard after the page opened", () => {
    assert.ok(textOf(ids.map).includes("Late"), textOf(ids.map));
  });

  timelines[0].handlers.timechange({ id: "scrub", time: new Date(secondTo - 20_000) });
  answer = {
    now: secondTo + 30_000,
    from: secondTo,
    to: secondTo + 30_000,
    truncated: false,
    nodes: [{ id: "n0", label: "n0", x: 0.2, y: 0.5 }],
    tracks: [
      {
        id: "after",
        label: "AfterDrag",
        class: "linger",
        named: true,
        samples: [{ t: secondTo + 25_000, node: "n0", rssi: -40 }],
        stays: [{ node: "n0", start: secondTo + 24_000, end: secondTo + 25_000, rssi: -40 }],
      },
    ],
    edges: [],
  };
  intervals[0].fn();
  await settle();
  check("dragging the marker stops the live view from jumping ahead", () => {
    assert.ok(!textOf(ids.map).includes("AfterDrag"), textOf(ids.map));
  });

  clearStatus = 500;
  await ids.clear.listeners.click[0]();
  await settle();
  check("a failed clear says the log was not cleared", () => {
    assert.equal(ids.error.textContent, "The log was not cleared.");
  });
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
