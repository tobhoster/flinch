#!/usr/bin/env python3
"""Turn a real FLINCH state dir into the demo snapshot flinch-demo embeds.

Usage: python3 crates/flinch-web/demo/anonymize.py <state-dir>

Reads status.json, items.json and history.json from <state-dir> and writes
anonymized copies next to this script. Renaming titles is not enough: an exact
file size names the exact release, and years, genres, episode counts, air
dates and watch recency together name the title and the household's viewing.
So every such figure gets random noise:

- titles: an invented bird-themed name per movie or show, drawn at random;
- library ids (radarr-N, sonarr-S-sK) and Plex ratingKeys: renumbered to
  numbers the input never used, consistently across all three files;
- sizes: scaled by 0.6-1.6 and rounded to 0.1 GiB; every total built from
  item sizes (library, eligible, untracked, held bytes) is recomputed, disk
  totals are rounded to 1 GiB, and other byte totals get the same noise;
- years: shifted by -3..+3, never past the snapshot's year nor past the last
  air date; genre lists are shuffled among movies, and among shows;
- per-item timestamps and day counts: moved by up to 30% of their age, one
  factor per movie or show, so spans keep their order, the past stays past,
  and no value crosses the 30 and 90 day lines the daemon's reasons rely on;
- episode and file counts: +-30%; watched episodes stay within the total;
- errors, sync problems, unmatched roots and the benchmark endpoint: generic.

Randomness comes from the OS (random.SystemRandom), never from a seed: a
known seed would let anyone replay the noise and recover the real figures.
Each run therefore writes a different demo.

Posters, *arr slugs and Plex GUIDs are dropped. Titles in status.json
(deletions FLINCH did not make, held evictions) get the same names by id; a
deletion whose item is not in items.json is dropped, as there is no name for it.

Before writing, the script fails if the output still holds any real title,
slug, GUID or poster URL (case-insensitive), any input size in bytes, any
input library id or ratingKey, or anything that looks like a hostname or IP
address other than localhost. Standard library only.
"""

import datetime
import json
import random
import re
import sys
from pathlib import Path

FILES = ("status.json", "items.json", "history.json")

# Fields that point at a real title in Plex, TMDB, TVDB or the *arr apps.
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

RNG = random.SystemRandom()
GIB = 1 << 30
DAY = 86_400
SIZE_FACTOR = (0.6, 1.6)
# Timestamps and counts move by up to this share of their value.
JITTER = 0.3
YEAR_SHIFT = 3
# The daemon's reasons compare day counts with these lines ("watched within
# 30 d", the 90 day never-played dwell): noise never moves a value across one.
THRESHOLD_DAYS = (30.0, 90.0)

CARD_ID = re.compile(r"^(radarr|sonarr)-(\d+)(-s\d+)?$")
ANY_ID = re.compile(r"\b(?:radarr|sonarr)-\d+(?:-s\d+)?\b")
SEASON_ID = re.compile(r"^(sonarr-\d+)-s\d+$")
YEAR_SUFFIX = re.compile(r"\s*\(\d{4}\)$")
SEASON_SUFFIX = re.compile(r"\s+s\d+$")
FREES = re.compile(r"^frees [\d.]+ GiB$")
ON_DISK = re.compile(r"^on disk \d+ d$")
PLAYED = re.compile(r"^played \d+ d ago$")
IPV4 = re.compile(r"\b\d{1,3}(?:\.\d{1,3}){3}\b")
HOSTNAME = re.compile(r"\b(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}\b", re.IGNORECASE)
URL_HOST = re.compile(r"://([^/:\s]+)")
LOCAL_HOSTS = {"127.0.0.1", "localhost"}

ITEM_TIMES = ("handed_at", "leaves_at")
BENCHMARK_ENDPOINT = "http://127.0.0.1:8000"
BENCHMARK_MODEL = "example-taste-model"
GENERIC_ERROR = "Sonarr did not answer: connection refused"
GENERIC_PROBLEM = "Tautulli did not answer: plays since the last run may be missing"


def fail(message):
    print(f"anonymize: {message}", file=sys.stderr)
    sys.exit(1)


def load(state_dir, name):
    path = state_dir / name
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        fail(f"cannot read {path}: {error}")


def card_key(card_id):
    """Movies are named and timed per movie, seasons per show."""
    match = SEASON_ID.match(card_id)
    return match.group(1) if match else card_id


def title_key(item):
    return card_key(item["id"])


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


def status_titled(status):
    """status.json entries that carry an id and title: deletions FLINCH did
    not make, and held evictions per volume."""
    yield from status.get("outside_deletions") or []
    for volume in (status.get("capacity") or {}).get("volumes") or []:
        yield from volume.get("held") or []


# ---- what must not survive ------------------------------------------------

def secrets_of(items, status):
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
    for entry in status_titled(status):
        found.add(entry.get("title") or "")
    return {s for s in (raw.strip().lower() for raw in found) if checkable(s) and not fictional(s)}


def checkable(secret):
    """Guards against matching everything: skip empty and very short strings,
    and bare numbers under six digits (TMDB ids are longer than any count)."""
    return len(secret) >= 6 if secret.isdigit() else len(secret) >= 3


def fictional(title):
    """True for a name this script invented, so an earlier demo can be fed
    back in: its names are not secrets, and new ones may reuse them."""
    base = YEAR_SUFFIX.sub("", SEASON_SUFFIX.sub("", title))
    return base in {name.lower() for name in NAMES}


def input_ids(items, status):
    ids = {item["id"] for item in items} | {entry["id"] for entry in status_titled(status)}
    return ids | {card_key(card_id) for card_id in ids}


def rating_keys_of(items):
    keys = set()
    for item in items:
        plex = item.get("plex") or {}
        keys.update(str(plex[field]) for field in ("rating_key", "season_rating_key") if plex.get(field) is not None)
    return keys


def sizes_of(items, status):
    sizes = [item.get("size_bytes") or 0 for item in items]
    sizes += [held.get("bytes") or 0 for held in status_titled(status) if "bytes" in held]
    return {size for size in sizes if size}


def leaks(outputs, items, status):
    """Every way the output could still point back at the input."""
    found = []
    secrets = secrets_of(items, status)
    ids, rating_keys, sizes = input_ids(items, status), rating_keys_of(items), sizes_of(items, status)
    for file, data in outputs.items():
        for text in set(strings(data)):
            lowered = text.lower()
            found += [f"{file}: real title string {secret!r}" for secret in secrets if secret in lowered]
            for token in ANY_ID.findall(text):
                if token in ids or card_key(token) in ids:
                    found.append(f"{file}: input id {token!r}")
            hosts = set(IPV4.findall(text)) | set(HOSTNAME.findall(text)) | set(URL_HOST.findall(text))
            found += [f"{file}: host or address {host!r} in {text!r}" for host in hosts - LOCAL_HOSTS]
    out_items, out_status = outputs["items.json"], outputs["status.json"]
    found += [f"items.json: {key!r} is an input ratingKey" for key in rating_keys_of(out_items) & rating_keys]
    out_sizes = {item["size_bytes"] for item in out_items} | {held["bytes"] for held in status_titled(out_status) if "bytes" in held}
    found += [f"input size {size} bytes survived" for size in sorted(out_sizes & sizes)]
    return sorted(set(found)), len(secrets) + len(ids) + len(rating_keys) + len(sizes)


# ---- noise ----------------------------------------------------------------

def fictional_names(items):
    keys = sorted({title_key(item) for item in items})
    if len(keys) > len(NAMES):
        fail(f"{len(keys)} distinct titles but only {len(NAMES)} fictional names: extend NAMES")
    return dict(zip(keys, RNG.sample(NAMES, len(keys))))


def fresh_numbers(used, first=1):
    """Maps each number in `used` to a random one `used` does not contain."""
    taken = set(used)
    pool = [n for n in range(first, max(taken, default=first) + 3 * len(taken) + 2) if n not in taken]
    return dict(zip(sorted(taken), RNG.sample(pool, len(taken))))


def id_renumbering(ids):
    numbers = {}
    for app in ("radarr", "sonarr"):
        numbers[app] = fresh_numbers({int(m.group(2)) for m in map(CARD_ID.match, ids) if m and m.group(1) == app})

    def new_id(old):
        match = CARD_ID.match(old)
        if not match:
            fail(f"unexpected library id {old!r}")
        return f"{match.group(1)}-{numbers[match.group(1)][int(match.group(2))]}{match.group(3) or ''}"

    return new_id


def rating_key_renumbering(items):
    keys = rating_keys_of(items)
    if not all(key.isdigit() for key in keys):
        fail("a Plex ratingKey is not a number")
    numbers = fresh_numbers({int(key) for key in keys}, first=100)
    return {key: str(numbers[int(key)]) for key in keys}


def noisy_size(size):
    """Scaled by SIZE_FACTOR and rounded to 0.1 GiB; never the input size,
    nor the same size to 0.1 GiB."""
    if not size:
        return size
    while True:
        gib = max(0.1, round(size * RNG.uniform(*SIZE_FACTOR) / GIB, 1))
        noisy = round(gib * GIB)
        if noisy != size and gib != round(size / GIB, 1):
            return noisy


def noisy_count(count):
    return max(1, round(count * RNG.uniform(1 - JITTER, 1 + JITTER)))


def time_factor(days):
    """A factor within 1 +- JITTER that keeps each day count on its side of
    every THRESHOLD_DAYS line."""
    low, high = 1 - JITTER, 1 + JITTER
    for value in days:
        for line in THRESHOLD_DAYS:
            if 0 < value < line:
                high = min(high, line / value)
            elif value > line:
                low = max(low, line / value)
    if low >= high:
        fail(f"no time noise keeps {sorted(days)} days on the same side of {THRESHOLD_DAYS}")
    return RNG.uniform(low, high)


def time_factors(items, status):
    """One factor per movie or show: its timeline stretches as a whole."""
    days = {}
    for item in items:
        values = days.setdefault(title_key(item), [])
        values += [item[field] for field in ("age_days", "last_watched_days") if item.get(field) is not None]
    for entry in status_titled(status):
        days.setdefault(card_key(entry["id"]), [])
    return {key: time_factor(values) for key, values in days.items()}


class Clock:
    """Moves timestamps by a share of their age at the snapshot's run."""

    def __init__(self, ran_at, factors):
        self.ran_at = ran_at
        self.factors = factors

    def age(self, key, days):
        return None if days is None else float(f"{days * self.factors[key]:.8g}")

    def time(self, key, unix):
        if unix is None:
            return None
        return self.ran_at - round((self.ran_at - unix) * self.factors[key])

    def date(self, key, unix):
        """An air date: moved, then back to midnight UTC like the input."""
        moved = self.time(key, unix)
        return None if moved is None else moved - moved % DAY


def shuffled_genres(items):
    """Genre lists dealt out again among movies, and among shows."""
    by_kind = {}
    for item in items:
        if item.get("genres") is not None:
            by_kind.setdefault(item["kind"], {}).setdefault(title_key(item), item["genres"])
    dealt = {}
    for lists in by_kind.values():
        keys, values = list(lists), list(lists.values())
        RNG.shuffle(values)
        dealt.update(zip(keys, values))
    return dealt


def shifted_year(year, latest):
    if year is None:
        return None
    choices = [year + shift for shift in range(-YEAR_SHIFT, YEAR_SHIFT + 1) if year + shift <= latest]
    return RNG.choice(choices) if choices else min(year, latest)


def utc_year(unix):
    return datetime.datetime.fromtimestamp(unix, datetime.timezone.utc).year


def noisy_episodes(item):
    """(episodes, watched_fraction): the total moves +-30%, a partial watch
    stays partial, and watched never exceeds the total."""
    episodes, fraction = item.get("episodes"), item.get("watched_fraction")
    if not episodes:
        return episodes, fraction
    total = noisy_count(episodes)
    if fraction is None or fraction <= 0.0 or fraction >= 1.0:
        return total, fraction
    total = max(total, 2)
    watched = min(noisy_count(round(fraction * episodes)), total - 1)
    return total, float(f"{watched / total:.8g}")


def reworded(reasons, size, age_days, watched_days):
    """The daemon's reason chips quote size and days: quote the new ones."""
    out = []
    for reason in reasons or []:
        if FREES.match(reason) and size:
            reason = f"frees {size / GIB:.1f} GiB"
        elif ON_DISK.match(reason) and age_days is not None:
            reason = f"on disk {age_days:.0f} d"
        elif PLAYED.match(reason) and watched_days is not None:
            reason = f"played {watched_days:.0f} d ago"
        out.append(reason)
    return out


class Anonymizer:
    def __init__(self, status, items):
        self.names = fictional_names(items)
        self.new_id = id_renumbering(input_ids(items, status))
        self.rating_keys = rating_key_renumbering(items)
        self.clock = Clock(status["ran_at_unix"], time_factors(items, status))
        self.genres = shuffled_genres(items)
        self.latest_year = utc_year(status["ran_at_unix"])
        self.years = {}
        self.blurred = {}
        self.by_id = {}

    def blur(self, size):
        """A byte total that is not a sum this script can redo: the same noise
        as an item size, the same output for the same input."""
        return self.blurred.setdefault(size, noisy_size(size)) if size else size

    def name_for(self, card_id):
        return self.names.get(card_key(card_id))

    def show_year(self, key, year, aired):
        if key not in self.years:
            latest = self.latest_year if aired is None else min(self.latest_year, max(utc_year(aired), year or 0))
            self.years[key] = shifted_year(year, latest)
        return self.years[key]

    def item(self, item):
        key = title_key(item)
        out = dict(item)
        out["id"] = self.new_id(item["id"])
        name = self.names[key]
        label = item.get("season_label")
        if item["kind"] == "season" and label and item["title"].endswith(" " + label):
            name = f"{name} {label}"
        out["title"] = name
        for field in STRIPPED:
            if field in out:
                out[field] = None
        out["size_bytes"] = noisy_size(item["size_bytes"])
        # One factor per show, so its seasons keep one air date.
        out["last_aired_epoch"] = self.clock.date(key, item.get("last_aired_epoch"))
        out["year"] = self.show_year(key, item.get("year"), out["last_aired_epoch"])
        if "genres" in item and item["genres"] is not None:
            out["genres"] = self.genres[key]
        out["age_days"] = self.clock.age(key, item.get("age_days"))
        out["last_watched_days"] = self.clock.age(key, item.get("last_watched_days"))
        for field in ITEM_TIMES:
            out[field] = self.clock.time(key, item.get(field))
        if item.get("on_disk") is not None:
            out["on_disk"] = [{field: self.clock.time(key, unix) for field, unix in span.items()} for span in item["on_disk"]]
        out["episodes"], out["watched_fraction"] = noisy_episodes(item)
        out["reasons"] = reworded(item.get("reasons"), out["size_bytes"], out["age_days"], out["last_watched_days"])
        if item.get("plex"):
            plex = dict(item["plex"])
            for field in ("rating_key", "season_rating_key"):
                if plex.get(field) is not None:
                    plex[field] = self.rating_keys[str(plex[field])]
            out["plex"] = plex
        self.by_id[item["id"]] = (item, out)
        return out

    def deletion(self, entry):
        name = self.name_for(entry["id"])
        if not name:
            return None
        key = card_key(entry["id"])
        known = self.by_id.get(entry["id"])
        files = entry.get("files")
        if known and files and known[0].get("episodes") == files:
            files = known[1]["episodes"]
        elif files:
            files = noisy_count(files)
        return {**entry, "id": self.new_id(entry["id"]), "title": name, "files": files, "at_unix": self.clock.time(key, entry.get("at_unix"))}

    def held(self, entry):
        key = card_key(entry["id"])
        known = self.by_id.get(entry["id"])
        since = self.clock.time(key, entry["held_since"])
        return {
            **entry,
            "id": self.new_id(entry["id"]),
            "title": self.name_for(entry["id"]) or "",
            "bytes": known[1]["size_bytes"] if known else self.blur(entry["bytes"]),
            "held_since": since,
            "until": since + (entry["until"] - entry["held_since"]),
        }


# ---- status and history ---------------------------------------------------

def round_gib(size):
    return round(size / GIB) * GIB if size else size


def ratio(part, whole):
    return float(f"{part / whole:.7g}") if whole else 0.0


def eligible_ids(items, volume, expected):
    """Items counted in a volume's eligible reserve: delete candidates and
    items eligible but held. Checked against the daemon's own total, so a
    change in its accounting stops the script instead of skewing the demo."""
    ids = [
        item["id"] for item in items
        if item.get("volume") == volume and (item.get("decision") == "delete" or item.get("reason", "").startswith("Eligible \u2014"))
    ]
    got = sum(item["size_bytes"] for item in items if item["id"] in ids)
    if got != expected:
        fail(f"eligible items on {volume} add up to {got} bytes, the status says {expected}: update eligible_ids")
    return ids


def rebuild_volume(volume, anon, items, old_total):
    path = volume.get("path")
    held = [anon.held(entry) for entry in volume.get("held") or []]
    library = sum(out["size_bytes"] for item, out in anon.by_id.values() if item.get("volume") == path)
    eligible = sum(anon.by_id[card_id][1]["size_bytes"] for card_id in eligible_ids(items, path, volume.get("eligible_bytes", 0)))
    total, used = round_gib(volume["total_bytes"]), round_gib(volume["used_bytes"])
    if library > used:
        fail(f"noisy library on {path} ({library} bytes) exceeds the disk's used bytes: run again")
    old_total[path] = volume["total_bytes"]
    return {
        **volume,
        "total_bytes": total,
        "used_bytes": used,
        "utilization": ratio(used, total),
        "deficit_bytes": round_gib(volume.get("deficit_bytes", 0)),
        "release_gap_bytes": round_gib(volume.get("release_gap_bytes", 0)),
        "goal_bytes": round_gib(volume.get("goal_bytes", 0)),
        "reclaimed_bytes": anon.blur(volume.get("reclaimed_bytes", 0)),
        "eligible_bytes": eligible,
        "pending_bytes": anon.blur(volume.get("pending_bytes", 0)),
        "handed_bytes": anon.blur(volume.get("handed_bytes", 0)),
        "held_bytes": sum(entry["bytes"] for entry in held),
        "held": held,
        "library_bytes": library,
        "untracked_bytes": used - library,
    }


def generic_roots(roots):
    """`app:/path` roots the daemon could not place: the app stays, the path goes."""
    return [f"{root.split(':', 1)[0]}:/media/library-{index}" for index, root in enumerate(roots or [], 1)]


def rebuild_capacity(capacity, anon, items):
    old_total = {}
    volumes = [rebuild_volume(volume, anon, items, old_total) for volume in capacity.get("volumes") or []]
    total, used = round_gib(capacity["total_bytes"]), round_gib(capacity["used_bytes"])
    out = {
        **capacity,
        "total_bytes": total,
        "used_bytes": used,
        "ceiling_bytes": round(total * capacity["ceiling"]),
        "release_bytes": round(total * capacity["release"]),
        "deficit_bytes": round_gib(capacity.get("deficit_bytes", 0)),
        "release_gap_bytes": round_gib(capacity.get("release_gap_bytes", 0)),
        "utilization": ratio(used, total),
        "goal_bytes": round_gib(capacity.get("goal_bytes", 0)),
        "volumes": volumes,
        "unmatched_roots": generic_roots(capacity.get("unmatched_roots")),
        "pending_bytes": anon.blur(capacity.get("pending_bytes", 0)),
        "handed_bytes": anon.blur(capacity.get("handed_bytes", 0)),
        "held_bytes": sum(volume["held_bytes"] for volume in volumes) if volumes else anon.blur(capacity.get("held_bytes", 0)),
        "untracked_bytes": sum(volume["untracked_bytes"] for volume in volumes) if volumes else round_gib(capacity.get("untracked_bytes", 0)),
    }
    if isinstance(capacity.get("covered"), int):
        out["covered"] = round_gib(capacity["covered"])
    for volume in volumes:
        if isinstance(volume.get("covered"), int):
            volume["covered"] = round_gib(volume["covered"])
    return out


def utilization_map(capacity):
    """A past run's utilization, as the rounded disk would have shown it."""
    total = capacity["total_bytes"]
    new_total = round_gib(total)
    return lambda value: value if value is None else ratio(round_gib(round(value * total)), new_total)


def anonymize_status(status, anon, items):
    out = json.loads(json.dumps(status))
    deletions = [anon.deletion(entry) for entry in out.get("outside_deletions") or []]
    deletions = [entry for entry in deletions if entry]
    deletions.sort(key=lambda entry: (-(entry.get("at_unix") or 0), id_order(entry["id"])))
    out["outside_deletions"] = deletions
    if out.get("capacity"):
        out["capacity"] = rebuild_capacity(out["capacity"], anon, items)
        eligible = sum(volume["eligible_bytes"] for volume in out["capacity"]["volumes"])
        out["eligible_bytes"] = eligible if out["capacity"]["volumes"] else anon.blur(out.get("eligible_bytes", 0))
    else:
        out["eligible_bytes"] = anon.blur(out.get("eligible_bytes", 0))
    out["reclaimed_bytes"] = anon.blur(out.get("reclaimed_bytes", 0))
    if out.get("shadow_gib"):
        out["shadow_gib"] = round(anon.blur(round(out["shadow_gib"] * GIB)) / GIB, 1)
    if out.get("last_error"):
        out["last_error"] = GENERIC_ERROR
    out["evidence_problems"] = [GENERIC_PROBLEM for _ in out.get("evidence_problems") or []]
    sync = out.get("sync")
    if sync:
        if sync.get("error"):
            sync["error"] = GENERIC_ERROR
        for field in ("problems", "warnings"):
            sync[field] = [GENERIC_PROBLEM for _ in sync.get(field) or []]
        for field in ("scheduled_bytes", "announced_bytes"):
            if field in sync:
                sync[field] = anon.blur(sync[field])
    benchmark = out.get("benchmark")
    if benchmark:
        benchmark["endpoint"] = BENCHMARK_ENDPOINT
        benchmark["model"] = BENCHMARK_MODEL
    return out


def anonymize_history(history, anon, capacity):
    utilization = utilization_map(capacity) if capacity else (lambda value: value)
    return [
        {**point, "reclaimed_bytes": anon.blur(point.get("reclaimed_bytes", 0)), "utilization": utilization(point.get("utilization"))}
        for point in history
    ]


def id_order(card_id):
    match = CARD_ID.match(card_id)
    season = int(match.group(3)[2:]) if match.group(3) else -1
    return (match.group(1), int(match.group(2)), season)


def item_order(item):
    """The daemon's order: items with files first, each group by id."""
    return (item["size_bytes"] == 0, id_order(item["id"]))


def main():
    if len(sys.argv) != 2:
        fail("usage: python3 crates/flinch-web/demo/anonymize.py <state-dir>")
    state_dir = Path(sys.argv[1])
    status, items, history = (load(state_dir, name) for name in FILES)
    if not isinstance(items, list) or not isinstance(history, list):
        fail("items.json and history.json must be JSON arrays")

    anon = Anonymizer(status, items)
    anonymized = sorted((anon.item(item) for item in items), key=item_order)
    outputs = {
        "status.json": anonymize_status(status, anon, items),
        "items.json": anonymized,
        "history.json": anonymize_history(history, anon, status.get("capacity")),
    }

    found, checked = leaks(outputs, items, status)
    if found:
        for leak in found:
            print(f"anonymize: {leak}", file=sys.stderr)
        fail(f"{len(found)} link(s) back to the input survived; nothing written")

    out_dir = Path(__file__).resolve().parent
    for file, data in outputs.items():
        (out_dir / file).write_text(json.dumps(data, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"anonymize: {len(anon.names)} titles renamed ({len(anonymized)} items), sizes, ids, dates and counts noised")
    print(f"anonymize: {checked} real titles, ids, ratingKeys and sizes checked, none survived")
    print(f"anonymize: wrote {', '.join(FILES)} to {out_dir}")


if __name__ == "__main__":
    main()
