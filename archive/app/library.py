"""Saved patterns and schedules for the H7020 page.

Documents live under data/panel/. The scheduler asks due_schedule for the
single latest slot that has already come due, instead of replaying the day.
"""

from __future__ import annotations

import json
import re
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable

SLUG = re.compile(r"^[a-z][a-z0-9-]{0,62}$")
TIME = re.compile(r"^(?:[01][0-9]|2[0-3]):[0-5][0-9]$")
WEEKDAYS = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")
HOLD_MAX = 24 * 60 * 60
STEPS_MAX = 32
DUE_WINDOW = timedelta(hours=12)

SceneInfo = Callable[[str], dict[str, Any]]


class WriteLimiter:
    """About thirty control calls a minute for the one radio."""

    def __init__(self, limit: int = 30, window: float = 60.0) -> None:
        self.limit = limit
        self.window = window
        self.hits: list[float] = []

    def allow(self, now: float | None = None) -> bool:
        moment = time.monotonic() if now is None else now
        self.hits = [hit for hit in self.hits if moment - hit < self.window]
        if len(self.hits) >= self.limit:
            return False
        self.hits.append(moment)
        return True


def _unknown(doc: dict[str, Any], allowed: set[str]) -> None:
    extra = set(doc) - allowed
    if extra:
        raise ValueError("unknown fields: " + ", ".join(sorted(extra)))


def _slug(value: Any, label: str) -> str:
    if not isinstance(value, str) or SLUG.fullmatch(value) is None:
        raise ValueError(f"{label} must be a short lowercase id")
    return value


def _name(value: Any) -> str:
    if not isinstance(value, str):
        raise ValueError("name must be text")
    text = " ".join(value.split())
    if not text or len(text) > 80:
        raise ValueError("name must be 1..80 characters")
    return text


def _int(value: Any, label: str, low: int, high: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise ValueError(f"{label} must be an integer {low}..{high}")
    return value


def _rgb(value: Any) -> list[int]:
    if not isinstance(value, list) or len(value) != 3:
        raise ValueError("rgb must be three integers")
    return [_int(part, "rgb", 0, 255) for part in value]


def _bulbs(value: Any, count: int) -> list[int]:
    if not isinstance(value, list) or not value:
        raise ValueError("a paint group needs bulbs")
    bulbs = [_int(bulb, "bulb", 1, count) for bulb in value]
    if len(bulbs) != len(set(bulbs)):
        raise ValueError("a bulb is repeated in one group")
    return sorted(bulbs)


def _paint(value: Any, count: int) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value:
        raise ValueError("paint needs at least one group")
    if len(value) > count:
        raise ValueError("too many paint groups")
    seen: set[int] = set()
    groups: list[dict[str, Any]] = []
    for group in value:
        if not isinstance(group, dict):
            raise ValueError("a paint group must be an object")
        _unknown(group, {"bulbs", "rgb"})
        bulbs = _bulbs(group.get("bulbs"), count)
        overlap = seen.intersection(bulbs)
        if overlap:
            raise ValueError(f"bulb {min(overlap)} is in two groups")
        seen.update(bulbs)
        groups.append({"bulbs": bulbs, "rgb": _rgb(group.get("rgb"))})
    return groups


def _scene(value: Any, scene_info: SceneInfo) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("scene must be an object")
    _unknown(value, {"name", "speed"})
    if not isinstance(value.get("name"), str):
        raise ValueError("scene name is required")
    try:
        info = scene_info(value["name"])
    except ValueError:
        raise
    except Exception as exc:
        raise ValueError(f"unknown scene {value['name']}") from exc
    speed = value.get("speed", None)
    if speed is None:
        resolved = info.get("default_speed")
    elif info.get("speeds", 0) == 0:
        raise ValueError(f"{info['name']} has no speed control")
    else:
        resolved = _int(speed, "speed", 0, int(info["speeds"]) - 1)
    scene: dict[str, Any] = {"name": info["name"]}
    if resolved is not None:
        scene["speed"] = resolved
    return scene


def validate_paint(groups: Any, bulbs: int) -> list[dict[str, Any]]:
    return _paint(groups, bulbs)


def validate_pattern(doc: Any, bulbs: int, scene_info: SceneInfo) -> dict[str, Any]:
    """Return a canonical pattern document."""
    if not isinstance(doc, dict):
        raise ValueError("pattern must be an object")
    kind = doc.get("kind")
    if kind not in {"scene", "paint", "sequence"}:
        raise ValueError("kind must be scene, paint, or sequence")
    allowed = {"id", "name", "kind", "scene"} if kind == "scene" else {"id", "name", "kind", "paint"}
    if kind == "sequence":
        allowed = {"id", "name", "kind", "repeat", "steps"}
    _unknown(doc, allowed)
    pattern: dict[str, Any] = {
        "id": _slug(doc.get("id"), "id"),
        "name": _name(doc.get("name")),
        "kind": kind,
    }
    if kind == "scene":
        pattern["scene"] = _scene(doc.get("scene"), scene_info)
    elif kind == "paint":
        pattern["paint"] = _paint(doc.get("paint"), bulbs)
    else:
        repeat = doc.get("repeat", False)
        if not isinstance(repeat, bool):
            raise ValueError("repeat must be a boolean")
        steps = doc.get("steps")
        if not isinstance(steps, list) or not steps or len(steps) > STEPS_MAX:
            raise ValueError(f"a sequence needs 1..{STEPS_MAX} steps")
        cleaned: list[dict[str, Any]] = []
        for step in steps:
            if not isinstance(step, dict):
                raise ValueError("a step must be an object")
            _unknown(step, {"hold", "paint", "scene"})
            if ("paint" in step) == ("scene" in step):
                raise ValueError("a step needs a paint or a scene")
            hold = _int(step.get("hold"), "hold", 1, HOLD_MAX)
            if "paint" in step:
                cleaned.append({"hold": hold, "paint": _paint(step["paint"], bulbs)})
            else:
                cleaned.append({"hold": hold, "scene": _scene(step["scene"], scene_info)})
        pattern["repeat"] = repeat
        pattern["steps"] = cleaned
    return pattern


def validate_schedule(doc: Any, patterns: dict[str, Any], scene_info: SceneInfo) -> dict[str, Any]:
    """Return a canonical schedule. A pattern action must name a saved pattern."""
    if not isinstance(doc, dict):
        raise ValueError("schedule must be an object")
    _unknown(doc, {"id", "enabled", "days", "time", "action"})
    enabled = doc.get("enabled", True)
    if not isinstance(enabled, bool):
        raise ValueError("enabled must be a boolean")
    days = doc.get("days")
    if not isinstance(days, list) or not days:
        raise ValueError("days need at least one weekday")
    if any(not isinstance(day, str) or day not in WEEKDAYS for day in days):
        raise ValueError("days must be mon, tue, wed, thu, fri, sat, sun")
    if len(days) != len(set(days)):
        raise ValueError("days are repeated")
    time_text = doc.get("time")
    if not isinstance(time_text, str) or TIME.fullmatch(time_text) is None:
        raise ValueError("time must be HH:MM")
    action = _action(doc.get("action"), patterns, scene_info)
    return {
        "id": _slug(doc.get("id"), "id"),
        "enabled": enabled,
        "days": [day for day in WEEKDAYS if day in days],
        "time": time_text,
        "action": action,
    }


def _action(value: Any, patterns: dict[str, Any], scene_info: SceneInfo) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("action must be an object")
    keys = set(value)
    if keys == {"power"}:
        if value["power"] is not False:
            raise ValueError("power action is only off")
        return {"power": False}
    if keys == {"pattern"}:
        pattern_id = value["pattern"]
        if not isinstance(pattern_id, str) or pattern_id not in patterns:
            raise ValueError("action pattern is not saved")
        return {"pattern": pattern_id}
    if keys == {"scene"}:
        return {"scene": _scene(value["scene"], scene_info)}
    raise ValueError("action must be a pattern, a scene, or power off")


class Library:
    """JSON files for patterns and schedules."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.patterns_path = root / "patterns.json"
        self.schedules_path = root / "schedules.json"
        self.audit_path = root / "audit.jsonl"

    def ensure(self) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        self.root.chmod(0o700)

    def list_patterns(self) -> list[dict[str, Any]]:
        return self._read(self.patterns_path, "patterns")

    def get_pattern(self, pattern_id: str) -> dict[str, Any] | None:
        return next((item for item in self.list_patterns() if item["id"] == pattern_id), None)

    def save_pattern(self, pattern: dict[str, Any], *, create: bool) -> dict[str, Any]:
        items = self.list_patterns()
        found = next((index for index, item in enumerate(items) if item["id"] == pattern["id"]), None)
        if create and found is not None:
            raise FileExistsError(pattern["id"])
        if not create and found is None:
            raise FileNotFoundError(pattern["id"])
        if found is None:
            items.append(pattern)
        else:
            items[found] = pattern
        self._write(self.patterns_path, "patterns", items)
        return pattern

    def delete_pattern(self, pattern_id: str) -> None:
        items = self.list_patterns()
        kept = [item for item in items if item["id"] != pattern_id]
        if len(kept) == len(items):
            raise FileNotFoundError(pattern_id)
        self._write(self.patterns_path, "patterns", kept)

    def list_schedules(self) -> list[dict[str, Any]]:
        return self._read(self.schedules_path, "schedules")

    def get_schedule(self, schedule_id: str) -> dict[str, Any] | None:
        return next((item for item in self.list_schedules() if item["id"] == schedule_id), None)

    def save_schedule(self, schedule: dict[str, Any], *, create: bool) -> dict[str, Any]:
        items = self.list_schedules()
        found = next((index for index, item in enumerate(items) if item["id"] == schedule["id"]), None)
        if create and found is not None:
            raise FileExistsError(schedule["id"])
        if not create and found is None:
            raise FileNotFoundError(schedule["id"])
        if found is None:
            items.append(schedule)
        else:
            items[found] = schedule
        self._write(self.schedules_path, "schedules", items)
        return schedule

    def delete_schedule(self, schedule_id: str) -> None:
        items = self.list_schedules()
        kept = [item for item in items if item["id"] != schedule_id]
        if len(kept) == len(items):
            raise FileNotFoundError(schedule_id)
        self._write(self.schedules_path, "schedules", kept)

    def due_schedule(self, now: datetime) -> dict[str, Any] | None:
        """Latest enabled slot that came due on this clock, within 12 hours.

        ``now`` is used as its own wall clock. The caller passes local time.
        """
        if now.tzinfo is None:
            raise ValueError("now must be timezone-aware")
        today = WEEKDAYS[now.weekday()]
        best: dict[str, Any] | None = None
        best_at: datetime | None = None
        for schedule in self.list_schedules():
            if not schedule.get("enabled") or today not in schedule["days"]:
                continue
            hour, minute = (int(part) for part in schedule["time"].split(":"))
            slot = now.replace(hour=hour, minute=minute, second=0, microsecond=0)
            if slot > now or now - slot > DUE_WINDOW:
                continue
            if best_at is None or slot > best_at or (slot == best_at and schedule["id"] > best["id"]):
                best = schedule
                best_at = slot
        return best

    def audit(self, token_name: str, method: str, route: str) -> None:
        self.ensure()
        line = json.dumps(
            {
                "time": datetime.now().astimezone().isoformat(timespec="seconds"),
                "token": token_name,
                "method": method,
                "route": route,
            },
            separators=(",", ":"),
        )
        with self.audit_path.open("a", encoding="utf-8") as handle:
            handle.write(line + "\n")

    def _read(self, path: Path, key: str) -> list[dict[str, Any]]:
        if not path.is_file():
            return []
        data = json.loads(path.read_text(encoding="utf-8"))
        items = data.get(key) if isinstance(data, dict) else None
        if not isinstance(items, list):
            raise ValueError(f"{path.name} is missing {key}")
        return [dict(item) for item in items]

    def _write(self, path: Path, key: str, items: list[dict[str, Any]]) -> None:
        self.ensure()
        temp = path.with_suffix(path.suffix + ".tmp")
        temp.write_text(json.dumps({key: items}, indent=2) + "\n", encoding="utf-8")
        temp.replace(path)
