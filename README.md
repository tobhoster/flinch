<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="frontend/public/logo-dark.png">
    <img src="frontend/public/logo.png" alt="FLINCH" width="180">
  </picture>
</p>

<h1 align="center">FLINCH</h1>

<p align="center">
  <b>Keeps each Plex library disk under 80% full by freeing what your household will miss least.</b><br>
  A storage planner for the *arr stack. It forecasts every disk, picks the evictions with the
  least expected regret, and leaves the deleting to Maintainerr.
</p>

<p align="center">
  <a href="https://github.com/tobhoster/flinch/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/tobhoster/flinch/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/tobhoster/flinch/actions/workflows/security.yml"><img alt="Security" src="https://github.com/tobhoster/flinch/actions/workflows/security.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-f28c28"></a>
  <img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-Rust-1b2a4a?logo=rust">
  <img alt="Runs on any CPU" src="https://img.shields.io/badge/runs%20on-any%20CPU-1b2a4a">
  <img alt="Works with Radarr, Sonarr, Plex and Maintainerr" src="https://img.shields.io/badge/works%20with-Radarr%20%C2%B7%20Sonarr%20%C2%B7%20Plex%20%C2%B7%20Maintainerr-f28c28">
</p>

<p align="center">
  <img src="docs/screenshots/overview.png" alt="The FLINCH Overview: each disk now and 14 days ahead against the 80% target, what the plan frees and at what regret, and what isn't library media" width="900">
</p>

---

## ✨ What it does

- **Plans ahead, per disk.** Nothing is deleted while a disk's two-week
  forecast stays 50 GiB under 80%.
- **Frees what nobody will miss, first.** It picks the set of evictions with
  the least expected regret, from your household's plays, requests and
  watchlists, and how hard each title is to download again.
- **Checks its forecast every day.** A daily fit replays the household's own
  past ("given what was known then, did anyone play this within 90 days?") and
  adopts a fitted P(watch) only when it beats the hand-set priors on titles it
  never saw.
- **Warns before anything unwatched goes** (one exception: see
  [Known limits](docs/how-it-works.md#known-limits)). A title nobody finished
  goes to a *Leaving Soon* row on the Plex home screen first. Play it and
  FLINCH takes it back.
- **Never touches what you protect.** Favorites, your keep tag, keep
  collections and your own Maintainerr exclusions are off limits, and so is
  anything on disk less than 30 days. Missing evidence means keep.
- **Says where the space went.** Each disk shows how much of it isn't library
  media, and any space an eviction should have freed that something still holds.
- **Keeps Maintainerr tidy.** It releases its own exclusions for items that are
  gone, and lists deletions it did not make and which will download again.

FLINCH decides *what* goes. [Maintainerr](https://github.com/Maintainerr/Maintainerr)
does the deleting, on its own schedule.

<p align="center">
  <img src="docs/screenshots/cleanup.png" alt="The Maintainerr sync and the deletions FLINCH did not make: what Radarr and Sonarr removed without it, and which titles will download again" width="500">
</p>

## 🧠 How it works

```mermaid
flowchart LR
    ARR["Radarr + Sonarr<br/>files · disks · imports · queue"] --> ID["match by catalogue id<br/>TMDB · TVDB · IMDb"]
    PX["Plex + Tautulli<br/>who played what, when"] --> ID
    ID --> R["regret per item"]
    EXT["Seerr · Prowlarr · SABnzbd"] --> R
    ARR --> FC["forecast per disk<br/>14 days ahead"]
    R --> S{"solver: least total regret<br/>that frees each target"}
    FC --> S
    S -- "selected" --> LS["Leaving Soon<br/>or delete collection"]
    LS --> M["Maintainerr deletes<br/>after the window"]
```

1. **Match.** Every title is joined across Radarr, Sonarr, Plex and Tautulli by
   catalogue id, never by name. An item that does not match is left alone.
2. **Forecast.** Each disk's use is projected 14 days ahead. The target is how
   far that lands over 80%, plus 50 GiB. A target of zero on every disk means
   nothing happens this cycle.
3. **Regret.** Each item's regret is P(played within 90 days) × the cost to
   download it again × how much the household claims it.
4. **Exclude.** Pinned items, items in their 30-day grace period, items Plex
   cannot address and items with no watch evidence never compete. Items nobody
   played compete only when you allow it.
5. **Solve.** An exact solver picks the cheapest set that covers each disk's
   target, taking a show's seasons in order. At 95% full it switches to a fast
   greedy pick.
6. **Hand off.** The plan goes to `eviction-plan.json` every cycle. With dry
   run off, selected items join a Maintainerr collection and Maintainerr
   deletes them after its window. Every write is read back.

The long version, with the formulas, is in
[docs/how-it-works.md](docs/how-it-works.md).

## 🚀 Try the demo

No homelab needed. The demo is a snapshot of a real library with every title
renamed and every size, date and id randomized:

```bash
docker run --rm -p 127.0.0.1:7911:7911 -e FLINCH_STATE_DIR=/tmp/demo \
  -e FLINCH_WEB_USERNAME=demo -e FLINCH_WEB_PASSWORD=demo \
  ghcr.io/tobhoster/flinch:0.2.0 sh -c 'flinch-demo && flinch-web'
```

Then open <http://localhost:7911> and log in as `demo` with the password `demo`.

<p align="center">
  <img src="docs/screenshots/movies.png" alt="The Movies table with one title open: its P(watch), regret, decision and the reasons behind it" width="900">
</p>

## ⬆️ Upgrading from 0.1.x

0.2.0 replaces the UI's token with a username and password. Before you change
the image tag in production (if you run `latest`, pin `0.1.1` first: `latest`
moves to 0.2.0 when it is published, and any restart pulls it):

1. **Add the login** to `flinch-secrets`: `FLINCH_WEB_USERNAME` and
   `FLINCH_WEB_PASSWORD`
   ([how, without a password in your shell history](deploy/README.md#upgrading-from-the-access-token)).
2. **Apply the 0.2.0 manifests**, which pass the two new keys to `flinch-web`:
   update your checkout to the release (`git fetch --tags && git checkout
   v0.2.0`), set `newTag: "0.2.0"` in your overlay if you have one, and run
   `kubectl apply -k`. An overlay reads `../base` from the checkout it sits in,
   so moving `newTag` alone runs 0.2.0 on 0.1.1's manifests, which never pass
   the login to the pod.
3. **Log in.** Until the Secret has both keys and `flinch-web` runs the 0.2.0
   manifests, the UI says "No login set" (added the keys after the rollout?
   `kubectl -n media rollout restart deploy/flinch-web`). Nothing is left open
   meanwhile, and the API key keeps working.

Automations keep working unchanged: `FLINCH_WEB_TOKEN` is now the API key,
still accepted as `Authorization: Bearer` and now as `X-Api-Key` too. No state
migration. Every behaviour change is toward keeping; these are the ones you
may notice on the first cycle:

- With no *Leaving Soon* collection named, unwatched items and never-played
  reclaim are **held**, not sent to a delete collection.
- A movie read as watched only from Plex's view stamp (no counted view), or
  only from Tautulli streams with no readable percentage, now counts as
  started: it waits for never-played reclaim and a *Leaving Soon* warning.
- An item no watch source reported on is kept; never-played reclaim no longer
  reaches it.
- An unwatched item FLINCH put in a delete collection earlier is taken back
  out and moves to *Leaving Soon*, whose window starts then. One in a delete
  collection FLINCH has no record of adding it to gets a FLINCH exclusion
  until you take it out.
- Adopting a fitted model can no longer make an item eligible that the
  hand-set priors hold.

The full list is in the [0.2.0 release notes](https://github.com/tobhoster/flinch/releases/tag/v0.2.0).

## 📦 Install

You need Radarr, Sonarr, Plex and Maintainerr 3.10 or newer; Tautulli, Seerr,
Prowlarr and SABnzbd are optional. FLINCH runs as two small pods on Kubernetes
from one published image (`ghcr.io/tobhoster/flinch`, amd64 and arm64, about
50 MB, no GPU).

1. **Configure** the namespace and service URLs in
   [`deploy/kustomization.yaml`](deploy/kustomization.yaml).
2. **Create the Secret** `flinch-secrets` from a file only you can read, so no
   key lands in your shell history:
   ```bash
   install -m 600 /dev/null flinch.env                              # readable only by you
   echo "FLINCH_WEB_TOKEN=$(openssl rand -hex 32)" >> flinch.env   # the API key, for automations
   $EDITOR flinch.env   # add FLINCH_WEB_USERNAME=..., FLINCH_WEB_PASSWORD=...,
                        # RADARR_API_KEY=..., SONARR_API_KEY=..., one per line
   kubectl -n media create secret generic flinch-secrets --from-env-file=flinch.env
   rm flinch.env
   ```
   `FLINCH_WEB_USERNAME` and `FLINCH_WEB_PASSWORD` are the UI's login; write
   every value without quotes. Plex goes in the same Secret or in
   **Settings → Plex** later; the other services' keys are optional. See
   [Configure](deploy/README.md#configure). Already running FLINCH with only
   the token? See [Upgrading from the access token](deploy/README.md#upgrading-from-the-access-token).
3. **Install:** `kubectl apply -k deploy/`
4. **Open the UI:** `kubectl -n media port-forward svc/flinch-web 7911:7911`,
   then <http://localhost:7911>, and log in with that username and password
   (see [Reach the UI](deploy/README.md#reach-the-ui)).
   Dry run starts **on**: FLINCH writes its plan and logs every write it would
   make, and sends nothing.
5. **Set up Maintainerr** (two delete collections and two *Leaving Soon*
   collections), then turn off **Settings → Planner → Dry run**.

The [install guide](deploy/README.md) covers each step, the exact Maintainerr
settings, publishing the UI over HTTPS, running as a CronJob, building your
own image, and keeping your own values out of git.

## 🛡️ Safety

- **Off until you say so.** Dry run is on by default: a fresh install only
  writes its plan and logs.
- **Pins beat the plan.** Favorites, the keep tag (`flinch-keep` by default: a
  Radarr/Sonarr tag, a Plex label or a Plex collection), keep collections and
  your own Maintainerr exclusions are never evicted. Neither is anything on
  disk less than 30 days.
- **Missing evidence keeps.** An item FLINCH cannot match, an item no watch
  source reported on, a watch source it could not read in full, or a disk it
  cannot measure means *keep*, never *delete*.
- **No warning, no unwatched deletion.** Anything nobody finished leaves only
  through *Leaving Soon* (one exception: see
  [Known limits](docs/how-it-works.md#known-limits)). With no *Leaving Soon*
  collection named, such items are held rather than sent to a delete
  collection.
- **Nothing unplayed unless you allow it.** Items nobody has played compete
  only once you turn on **Settings → Planner → Never played**.
- **Seasons leave in order.** A show's watched seasons go from the first,
  unplayed ones from the last, and a season that must stay keeps every season
  due to leave after it.
- **Deletes only through Maintainerr.** FLINCH never deletes a file itself and
  never writes to Radarr or Sonarr. Every Maintainerr write is read back; a
  failed one is retried, never assumed.
- **Per disk, never pooled.** Freeing the TV disk never counts toward a full
  movie disk, and bytes still in a recycle bin are credited, so nothing is
  deleted twice for the same gap.
- **Space that never frees is reported, not chased.** Bytes a seeding torrent
  or a snapshot still holds after an eviction are reported and stay credited
  for up to 14 days, so FLINCH evicts nothing more for them.

The UI asks for a username and password (`FLINCH_WEB_USERNAME`,
`FLINCH_WEB_PASSWORD`), because whoever can save Settings can turn off dry
run; automations use the API key (`FLINCH_WEB_TOKEN`) instead. Serve
it over HTTPS and keep it off the internet; see
[Security](deploy/README.md#security) and [SECURITY.md](SECURITY.md).

<p align="center">
  <img src="docs/screenshots/phone.png" alt="The Overview on a phone" width="300">
</p>

## 🔌 Extras

- **Quality advice.** Per item: keep the original, downgrade to a compact
  release, or eligible for eviction. Published only; FLINCH never changes a
  quality profile.
  [Details](docs/how-it-works.md#quality-advice-keep-downgrade-or-evict).

## 🚧 Status

- **Tested** with table-driven cases and property tests on everything that
  decides a deletion: the disk forecast, regret, the solver and its season
  order, recycle-bin credit and the freed-bytes check, identity joins,
  Maintainerr sync and exclusion releases, Leaving Soon routing and the
  forecast's no-leakage rules, plus the API's login, session and API key
  checks, the settings bounds and the response-size limit.
- **Checked on every change**: nothing merges into `main` without the tests,
  CodeQL and the Security workflow (secrets, dependency advisories and
  licenses, workflow linting, and a scan and smoke test of the image).
- **In production on one homelab**, handing items to Maintainerr since
  2026-09-22 (on 0.1.x; 0.2.0's changes merged on 2026-09-27). The regret
  planner is newer and unreleased. Every exclusion FLINCH writes is read back
  from Maintainerr each cycle. The Watch model card on the Overview shows
  whether a fitted P(watch) or the hand-set priors run: the daily fit adopts
  one only while it beats the priors out of fold.
- **TV on NFS can go ungoverned.** On that homelab, Sonarr's disk report left
  out all three of its NFS mounts, so FLINCH cannot measure those disks and never
  evicts from them. The Storage card says so.
- **One known limit can delete without a warning.** A season whose watched
  episodes were deleted outside FLINCH (by hand, or by Plex's "Delete episodes
  after playing") can read as completed and leave with no Leaving Soon warning,
  its unwatched episodes included. Until that is fixed, keep watched episodes
  on disk, or keep such a season with the keep tag on its show or a keep
  collection. The rest fail closed. One Radarr and one Sonarr instance; Plex is
  required to match items. The full list is in
  [docs/how-it-works.md](docs/how-it-works.md#known-limits).

## 🧑‍💻 Development

Building needs CMake, make, a C++ compiler and libclang: the HiGHS solver
compiles from source.

```bash
cargo fmt --all                            # the style in rustfmt.toml
cargo test --workspace --locked            # the whole suite
cd frontend && npm ci && npm run build     # the UI, into frontend/dist
cd ..
FLINCH_STATE_DIR=/tmp/flinch-demo cargo run -p flinch-web --bin flinch-demo
FLINCH_STATE_DIR=/tmp/flinch-demo FLINCH_WEB_DIR=frontend/dist \
  FLINCH_WEB_USERNAME=demo FLINCH_WEB_PASSWORD=demo cargo run -p flinch-web --bin flinch-web
```

[CONTRIBUTING.md](CONTRIBUTING.md) has the rules for changes; report security
problems privately, as [SECURITY.md](SECURITY.md) describes.

| Path | What it is |
| --- | --- |
| `crates/flinch-archive` | the daemon (`flinch-arrd`), the forecast and its daily fit (`flinch-fit`), and the offline planner (`flinch-archive`) |
| `crates/flinch-web` | the UI server, its JSON API and `flinch-demo` |
| `frontend/` | the React UI, built into the image |
| `deploy/` | the Dockerfile, kustomize manifests (with an optional HTTPS Ingress), install guide and Recyclarr config |
| `docs/` | how it works, and the screenshots above |

## License

MIT, see [LICENSE](LICENSE).
