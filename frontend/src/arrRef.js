// Card ids and subjects name their Radarr or Sonarr instance: the default
// instance as `radarr-7`, `sonarr-3-s2`, `sonarr-3`; a named one as
// `radarr@4k-7`, `sonarr@anime-3-s2`, `sonarr@anime-3`. Names are 1–24 of
// [a-z0-9_], never `-`, so the first `-` ends the instance key.

const REF = /^(radarr|sonarr)(?:@([a-z0-9_]{1,24}))?-(\d+)(?:-s(\d+))?$/;

/** An instance name as settings and numbered env vars accept it. */
export const INSTANCE_NAME = /^[a-z0-9_]{1,24}$/;

/**
 * `{ app, instance, id, season }` for a card id or subject; `instance` is ''
 * for the default. `season` is null for a movie or a show subject. Null when
 * `ref` names no *arr item.
 */
export function parseArrRef(ref) {
  const m = REF.exec(ref || '');
  if (!m) return null;
  const [, app, instance = '', id, season] = m;
  return { app, instance, id: Number(id), season: season === undefined ? null : Number(season) };
}

/** `Radarr`, `Sonarr 4k`: an instance as the UI names it. */
export function instanceLabel(app, instance = '') {
  const name = app === 'radarr' ? 'Radarr' : app === 'sonarr' ? 'Sonarr' : app;
  return instance ? `${name} ${instance}` : name;
}

/** The label for an instance key such as `radarr` or `radarr@4k`. */
export function instanceKeyLabel(key) {
  const [app, instance = ''] = String(key || '').split('@');
  return instanceLabel(app, instance);
}
