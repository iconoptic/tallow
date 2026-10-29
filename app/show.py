"""Playback and the in-process schedule clock.

A sequence step holds for the seconds stored on it, and those seconds are at
least one. The scheduler fires the latest due slot once per local day.
"""

from __future__ import annotations

import asyncio
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

import govee_h7020 as light  # noqa: E402

from library import Library  # noqa: E402


class Show:
    """Turns saved documents into writes on one light session."""

    def __init__(self, session: Any, library: Library, sleeper: Any = None) -> None:
        self.session = session
        self.library = library
        self._sleep = sleeper or asyncio.sleep
        self._generation = 0
        self._task: asyncio.Task[None] | None = None
        self._scheduler: asyncio.Task[None] | None = None
        self._fired: tuple[str, str] | None = None
        self._firing = False

    def start(self) -> None:
        self._scheduler = asyncio.create_task(self._schedule_loop())

    async def stop(self) -> None:
        self.cancel_playback()
        if self._scheduler is not None:
            self._scheduler.cancel()
            try:
                await self._scheduler
            except asyncio.CancelledError:
                pass

    def cancel_playback(self) -> None:
        self._generation += 1
        task = self._task
        self._task = None
        if task is not None and not task.done():
            task.cancel()
        self.session.state["playing"] = False

    async def apply_power(self, on: bool) -> dict[str, Any]:
        self.cancel_playback()
        fields: dict[str, Any] = {"on": on, "playing": False}
        if not on:
            fields["mode"] = "off"
        return await self.session.submit(replace_script=True, **fields)

    async def apply_brightness(self, level: int) -> dict[str, Any]:
        return await self.session.submit(brightness=level)

    async def apply_color(self, rgb: dict[str, int]) -> dict[str, Any]:
        self.cancel_playback()
        return await self.session.submit(
            replace_script=True,
            color=rgb,
            mode="color",
            scene=None,
            paint=None,
            pattern_id=None,
            playing=False,
        )

    async def apply_scene(self, name: str, speed: int | None, pattern_id: str | None = None) -> dict[str, Any]:
        self.cancel_playback()
        info, packets = light.build_scene_script(name, speed)
        return await self.session.submit_script(
            self._power_prefix(packets),
            mode="scene",
            scene={"name": info["name"], "speed": info["speed"]},
            paint=None,
            pattern_id=pattern_id,
            playing=False,
            on=True,
        )

    async def apply_paint(self, groups: list[dict[str, Any]], pattern_id: str | None = None) -> dict[str, Any]:
        self.cancel_playback()
        packets = light.build_paint_script(groups, self.session.profile)
        return await self.session.submit_script(
            self._power_prefix(packets),
            mode="paint",
            paint=groups,
            scene=None,
            pattern_id=pattern_id,
            playing=False,
            on=True,
        )

    async def apply_pattern(self, pattern: dict[str, Any]) -> dict[str, Any]:
        kind = pattern["kind"]
        if kind == "scene":
            scene = pattern["scene"]
            return await self.apply_scene(scene["name"], scene.get("speed"), pattern["id"])
        if kind == "paint":
            return await self.apply_paint(pattern["paint"], pattern["id"])
        self.cancel_playback()
        generation = self._generation
        self.session.state.update(
            mode="sequence",
            pattern_id=pattern["id"],
            playing=True,
            scene=None,
            paint=None,
        )
        self._task = asyncio.create_task(self._run_sequence(pattern, generation))
        return self.session.snapshot()

    async def apply_action(self, action: dict[str, Any]) -> dict[str, Any]:
        if "power" in action:
            return await self.apply_power(False)
        if "pattern" in action:
            pattern = self.library.get_pattern(action["pattern"])
            if pattern is None:
                raise ValueError(f"unknown pattern {action['pattern']}")
            return await self.apply_pattern(pattern)
        scene = action["scene"]
        return await self.apply_scene(scene["name"], scene.get("speed"))

    async def fire_due(self, now: datetime | None = None) -> dict[str, Any] | None:
        """Apply the latest due schedule once for that local date."""
        moment = datetime.now().astimezone() if now is None else now
        if moment.tzinfo is None:
            raise ValueError("now must be timezone-aware")
        chosen = self.library.due_schedule(moment)
        if chosen is None:
            return None
        key = (chosen["id"], moment.date().isoformat())
        if key == self._fired or self._firing:
            return None
        self._firing = True
        try:
            await self.apply_action(chosen["action"])
        finally:
            self._firing = False
        self._fired = key
        return chosen

    async def _schedule_loop(self) -> None:
        while True:
            try:
                await self.fire_due()
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001 — keep the clock running
                self.session.state["error"] = f"{type(exc).__name__}: {exc}"
            await self._sleep(5)

    async def _run_sequence(self, pattern: dict[str, Any], generation: int) -> None:
        try:
            while generation == self._generation:
                for step in pattern["steps"]:
                    if generation != self._generation:
                        return
                    await self._step(pattern, step)
                    if generation != self._generation:
                        return
                    await self._sleep(step["hold"])
                if not pattern.get("repeat"):
                    if generation == self._generation:
                        self.session.state["playing"] = False
                    return
        except asyncio.CancelledError:
            raise
        except Exception as exc:  # noqa: BLE001 — show the failure and stop this play
            self.session.state["error"] = f"{type(exc).__name__}: {exc}"
            self.session.state["playing"] = False

    async def _step(self, pattern: dict[str, Any], step: dict[str, Any]) -> None:
        if "scene" in step:
            scene = step["scene"]
            info, packets = light.build_scene_script(scene["name"], scene.get("speed"))
            await self.session.submit_script(
                self._power_prefix(packets),
                mode="sequence",
                scene={"name": info["name"], "speed": info["speed"]},
                paint=None,
                pattern_id=pattern["id"],
                playing=True,
                on=True,
            )
            return
        packets = light.build_paint_script(step["paint"], self.session.profile)
        await self.session.submit_script(
            self._power_prefix(packets),
            mode="sequence",
            paint=step["paint"],
            scene=None,
            pattern_id=pattern["id"],
            playing=True,
            on=True,
        )

    def _power_prefix(self, packets: list[bytes]) -> list[bytes]:
        if self.session.state.get("on") is True:
            return packets
        profile = self.session.profile
        if profile is None:
            raise ValueError("the light has no colour profile yet")
        return [light.build_power(True, light.MODEL, profile=profile), *packets]
