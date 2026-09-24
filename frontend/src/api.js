export async function getJson(url) {
  const res = await fetch(url, { headers: { Accept: 'application/json' } });
  if (!res.ok) throw new Error(`${url} -> ${res.status}`);
  return res.json();
}

export function loadStatus() {
  return getJson('/api/status').catch(() => null);
}

// Before the daemon's first cycle the state files are absent and the API
// answers `null`: anything but a list reads as empty, never as a crash.
const list = (value) => (Array.isArray(value) ? value : []);

export function loadItems() {
  return getJson('/api/items').then(list).catch(() => []);
}

export function loadHistory() {
  return getJson('/api/history').then(list).catch(() => []);
}
export async function triggerRun() {
  const res = await fetch('/api/run', { method: 'POST' });
  return res.ok;
}

/**
 * The settings the daemon runs with, every field present. Rejects with the
 * server's reason when settings.json cannot be read: the daemon then keeps its
 * last good settings, which the page cannot know, so it must not show defaults.
 */
export async function loadSettings() {
  const res = await fetch('/api/settings', { headers: { Accept: 'application/json' } });
  if (!res.ok) throw new Error((await res.text()) || `settings unavailable (${res.status})`);
  return res.json();
}

export async function saveSettings(settings) {
  const res = await fetch('/api/settings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(settings),
  });
  if (!res.ok) throw new Error(`save failed (${res.status}): ${await res.text()}`);
  return true;
}
