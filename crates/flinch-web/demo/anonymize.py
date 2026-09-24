#!/usr/bin/env python3
"""Turn a real FLINCH state dir into the demo snapshot flinch-demo embeds.

Usage: python3 crates/flinch-web/demo/anonymize.py <state-dir>

Reads status.json, items.json and history.json from <state-dir> and writes
anonymized copies next to this script. Every movie and show title becomes an
invented bird-themed name, stable per library id (a show keeps one name
across its seasons). Posters, *arr slugs and Plex GUIDs are dropped; Plex
ratingKeys stay, since they number items on one server and name nothing.
Everything else (sizes, days, decisions, forecasts, the run history) is kept.

The script then checks every string in the output against the real titles,
slugs, GUIDs and poster URLs it saw, case-insensitively, and exits non-zero if
any of them survived. Standard library only.
"""

import hashlib
import json
import re
import sys
from pathlib import Path

FILES = ("status.json", "items.json", "history.json")

# Fields that point at a real title in Plex, TMDB, TVDB or the *arr apps.
# `plex` (ratingKey, section) stays: those numbers are local to one server,
# and without them every kept item would read "not matched by id".
STRIPPED = ("poster_url", "title_slug", "play_keys")

NAMES = [
    "Starling Road", "Night Heron", "The Long Migration", "Wren at Low Tide",
    "Kestrel Point", "The Plover Winter", "Harrier Field", "Siskin Lane",
    "Under the Rookery", "The Grebe Affair", "Curlew Marsh", "The Bittern Tapes",
    "Shrike Season", "Oriole Street", "Petrel Light", "Linnet and the Lighthouse",
    "The Dunlin Line", "Godwit Flats", "Waxwing Hotel", "The Treecreeper Files",
    "Twite Hill", "Stonechat Summer", "The Fulmar Watch", "Gannet Rock Radio",
    "Avocet Bay", "Sandpiper County", "The Whimbrel Letters", "Pipit Park",
    "Bunting Hollow", "The Firecrest Protocol", "Chiffchaff Mornings",
    "Ouzel Gorge", "Dipper Creek", "The Garganey Code", "Scaup Harbour",
    "Eider Station", "Merganser Drive", "Pochard Row", "The Teal Hour",
    "Gadwall Heights", "Wigeon Flight", "Shoveler Marsh", "The Rail at Dusk",
    "Crake Hall", "Moorhen Lock", "The Coot Society", "Lapwing Downs",
    "Dotterel Ridge", "Snipe Country", "Woodcock Night", "The Skua Expedition",
    "Tern Island", "Guillemot Cliffs", "Puffin Station", "Shearwater Blues",
    "The Hoopoe Garden", "Bee-eater Valley", "Goshawk Mile", "Osprey Tower",
    "The Nightjar Hours", "Redpoll Junction", "Brambling Farm", "The Rosefinch Year",
    "Yellowhammer Days",
]

SEASON_ID = re.compile(r"^(sonarr-\d+)-s\d+$")
YEAR_SUFFIX = re.compile(r"\s*\(\d{4}\)$")


def fail(message):
    print(f"anonymize: {message}", file=sys.stderr)
    sys.exit(1)


def load(state_dir, name):
    path = state_dir / name
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        fail(f"cannot read {path}: {error}")


def title_key(item):
    """Movies are named per movie, seasons per show."""
    if item["kind"] == "season":
        match = SEASON_ID.match(item["id"])
        return match.group(1) if match else item["id"].rsplit("-", 1)[0]
    return item["id"]


def show_title(item):
    label = item.get("season_label")
    title = item["title"]
    if label and title.endswith(" " + label):
        return title[: -len(label) - 1]
    return title


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for entry in value:
            yield from strings(entry)
    elif isinstance(value, dict):
        for key, entry in value.items():
            yield key
            yield from strings(entry)


def secrets_of(items):
    """Everything that could name or look up a real title."""
    found = set()
    for item in items:
        found.add(item["title"])
        if item["kind"] == "season":
            show = show_title(item)
            found.add(show)
            found.add(YEAR_SUFFIX.sub("", show))
        found.add(item.get("poster_url") or "")
        found.add(item.get("title_slug") or "")
        keys = item.get("play_keys") or {}
        found.update(keys.get("plex_guids") or [])
        found.update(keys.get("episode_guids") or [])
    return {s for s in (raw.strip().lower() for raw in found) if checkable(s)}


def checkable(secret):
    """Guards against matching everything: skip empty and very short strings,
    and bare numbers under six digits (TMDB ids are longer than any count)."""
    return len(secret) >= 6 if secret.isdigit() else len(secret) >= 3


def fictional_names(items):
    keys = sorted({title_key(item) for item in items}, key=lambda k: hashlib.sha256(k.encode()).hexdigest())
    if len(keys) > len(NAMES):
        fail(f"{len(keys)} distinct titles but only {len(NAMES)} fictional names: extend NAMES")
    return dict(zip(keys, NAMES))


def anonymize_item(item, names):
    out = dict(item)
    name = names[title_key(item)]
    label = item.get("season_label")
    if item["kind"] == "season" and label and item["title"].endswith(" " + label):
        name = f"{name} {label}"
    out["title"] = name
    for field in STRIPPED:
        if field in out:
            out[field] = None
    return out


def main():
    if len(sys.argv) != 2:
        fail("usage: python3 crates/flinch-web/demo/anonymize.py <state-dir>")
    state_dir = Path(sys.argv[1])
    status, items, history = (load(state_dir, name) for name in FILES)
    if not isinstance(items, list) or not isinstance(history, list):
        fail("items.json and history.json must be JSON arrays")

    names = fictional_names(items)
    anonymized = [anonymize_item(item, names) for item in items]
    outputs = {"status.json": status, "items.json": anonymized, "history.json": history}

    secrets = secrets_of(items)
    leaks = sorted(
        {(file, secret) for file, data in outputs.items() for text in strings(data) for secret in secrets if secret in text.lower()}
    )
    if leaks:
        for file, secret in leaks:
            print(f"anonymize: {file} still contains {secret!r}", file=sys.stderr)
        fail(f"{len(leaks)} real title string(s) survived; nothing written")

    out_dir = Path(__file__).resolve().parent
    for file, data in outputs.items():
        (out_dir / file).write_text(json.dumps(data, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"anonymize: {len(names)} titles renamed ({len(anonymized)} items), {len(secrets)} real strings checked, no leaks")
    print(f"anonymize: wrote {', '.join(FILES)} to {out_dir}")


if __name__ == "__main__":
    main()
