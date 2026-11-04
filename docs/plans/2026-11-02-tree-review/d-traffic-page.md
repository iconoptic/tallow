# Chunk D: the traffic page (`app/traffic_page.html`)

What the user asked for (session `48f6ca04`): *"Don't just show devices grouped around their nearest AP. Use the relationships between my own devs and the dB estimates from various sources to attempt a triangulation of the device location."* The model is in the page: `rssi = tx - 10·n·log10(d)`. `n` is fitted from the nodes' own parent links, one listener sits beside its node, two split the segment, and three or more are solved by Gauss-Newton.

The page builds DOM with `textContent` and `createElementNS`. Keep it that way: no `innerHTML`, `outerHTML`, or `insertAdjacentHTML`. `tools/tests/test_traffic_page.py` checks this.

## D1 Fix the three-listener solver

**Done (2026-11-02).** `node tools/tests/traffic_page_harness.js` printed `ok three listeners place a noiseless dot` and `ok four listeners place a noiseless dot`. `.venv/bin/python -m unittest tools/tests/test_traffic_page.py` passed with the ok floor at 16 after D2. `tools/mesh.sh check --fast` passed.

- **Tier:** Grok.
- **Files:** `app/traffic_page.html` (`exponent`, `section`, `solvePlane`, `placeTrack`), `tools/tests/traffic_page_harness.js`, `tools/tests/test_traffic_page.py` (raise its floor on the count of `ok` lines to match the new checks).
- **Why:**
  1. **Sign.** `solvePlane` builds `jx`, `jy`, `jt` as the Jacobian of the residual `r = rssi - predicted`, then solves `JᵀJ·δ = Jᵀr`. The Gauss-Newton step for that `J` is `JᵀJ·δ = -Jᵀr`. During review, noiseless readings from four nodes at (0.1, 0.1), (0.9, 0.1), (0.5, 0.9), (0.1, 0.9) with `n = 2.7`, `tx = -45` gave `null` for (0.3, 0.3) and (0.2, 0.6), and (0.512, 0.258) for (0.5, 0.7). With the right-hand side negated, all four came back exact. A `null` falls back to the two loudest listeners, which hid the bug.
  2. **Aspect.** Places are 0..1 on both axes of a 1000×640 canvas. Distances in `exponent`, `section`, and `solvePlane` are taken in that unit square, so 0.1 across is 100 px and 0.1 down is 64 px. Rooms drawn as squares on screen are not squares to the solver.
- **Do:**
  1. Negate the right-hand side: `b0 -= jx * residual`, and the same for `b1` and `b2`. Keep `jt = -1`. Add a one-line comment saying which Jacobian this is.
  2. Do all distance math in canvas proportion: `x` as stored, `y` multiplied by `640 / 1000`. Convert in one helper used by `exponent`, `section`, and `solvePlane`, and convert back before `placeTrack` scales to pixels. Keep the bounds check equivalent (`-0.25..1.25` in stored units).
  3. Do not change the one- and two-listener behaviour other than the aspect fix.
- **New harness checks:**
  - Three listeners at known places with no parent links (`n` = 2.7). Samples from `rssi = -45 - 27·log10(d)` (distance in canvas proportion) for a device at (0.3, 0.3). The dot is within 10 px of (300, 192).
  - The same with four listeners and a device at (0.5, 0.7): within 10 px of (500, 448).
  - Two listeners at equal RSSI: the dot is at the pixel midpoint. That check already exists. Keep it.
- **Done when:** `node tools/tests/traffic_page_harness.js` prints the new `ok` lines, `.venv/bin/python -m unittest tools/tests/test_traffic_page.py` passes, and `tools/mesh.sh check --fast` passes.
- **Stop if:** the harness cannot place nodes without a layout POST. Report how it seeds nodes now.

## D2 Let live follow now

**Done (2026-11-02).** The harness printed `ok a live refresh follows a device heard after the page opened`, `ok dragging the marker stops the live view from jumping ahead`, and `ok a failed clear says the log was not cleared`. The page test passed with the ok floor at 16. `tools/mesh.sh check --fast` passed.

- **Tier:** Grok.
- **Needs:** D1.
- **Files:** `app/traffic_page.html` (`render`, `load`, `drawTimeline`, the window buttons, the Clear handler), `tools/tests/traffic_page_harness.js`, `tools/tests/test_traffic_page.py`.
- **Why:**
  - `scrub` is set once, from the first payload's `to`. The 5 s live refresh replaces the data and never moves `scrub`. `readingsAt` ignores samples after `scrub` and older than 60 s before it, so after one minute the live map shows where things were when the page opened. A device first heard after that never gets a dot.
  - Switching window keeps the old `scrub`.
  - Clear calls `load()` whatever the response, so a 500 looks like success.
- **Do:**
  1. Keep a flag `following`, true at start. In `load`, when `following` is true, set `scrub = body.to` and, if the timeline exists, `timeline.setCustomTime(new Date(scrub), "scrub")`.
  2. The `timechange` handler sets `following = false` when the user drags the marker to more than 5 s before the payload's `to`, and `following = true` when they drag it to within 5 s of it.
  3. Each window button sets `following = true` before `load()`.
  4. Clear checks `response.ok`. On failure it shows "The log was not cleared." and does not reload.
- **New harness checks:**
  - Two live loads with `to` 30 s apart. A track whose only samples fall between the two `to` values has a dot after the second load.
  - After a simulated marker drag to an earlier time, a further live load does not move `scrub`.
  - A Clear that answers 500 sets the error text.
- **Done when:** the harness prints the new `ok` lines, the page test passes with its floor raised, and `tools/mesh.sh check --fast` passes.
- **Stop if:** the harness's `vis` stub has no `setCustomTime`. Add it to the stub, and stop only if the stub cannot fire `timechange`.

## D3 A bigger name space (only if the user says yes, U3)

**Skipped (2026-11-02).** U3 was settled as (c) leave the current 1,024 names. This task does not run.

- **Tier:** Grok.
- **Needs:** B2, and the user's yes on [U3](user-actions.md).
- **Files:** `host/tallowd/src/cluster.rs` (`ADJECTIVES`, `NOUNS`, `cute_name`), its tests.
- **Why:** 32 adjectives × 32 nouns is 1,024 names. The log saw 644 random addresses in 16 hours, so repeats on one screen are certain. Any change renames every device once.
- **Do:** whichever the user picks in U3: (a) grow each list to 64 gentle, distinct words (4,096 names), or (b) keep the lists and append the last MAC byte as two hex digits, `Calm Fox 3e`. Keep `cute_name` deterministic and case-insensitive on input.
- **Done when:** `the_same_mac_always_yields_the_same_name` passes. A new test, `names_are_well_spread`, takes 1,000 sequential MACs and finds at least 900 distinct names for (a), or at least 990 for (b). `tools/mesh.sh daemon check` and `check --fast` pass.
- **Stop if:** the user has not answered U3.
