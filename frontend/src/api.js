// Every call to the FLINCH API goes through `request`, so the bearer token is
// attached in one place and a refusal locks the whole app, not one panel.

const TOKEN_KEY = 'flinch-token';

// Storage can be unavailable (private windows, blocked site data); the app then
// asks for the token again on every load rather than failing to start.
export function storedToken() {
  try {
    return localStorage.getItem(TOKEN_KEY) || '';
  } catch {
    return '';
  }
}

export function storeToken(token) {
  try {
    localStorage.setItem(TOKEN_KEY, token);
  } catch {
    // Nothing to keep it in; the next 401 asks again.
  }
}

export function forgetToken() {
  try {
    localStorage.removeItem(TOKEN_KEY);
  } catch {
    // Nothing was stored.
  }
}

/** Thrown for a 401. `configured` is false when the server has no token at all. */
export class Locked extends Error {
  constructor(message, configured, refused) {
    super(message);
    this.configured = configured;
    this.refused = refused;
  }
}

const lockListeners = new Set();

/** Called with the `Locked` error on every 401; returns the unsubscribe. */
export function onLocked(listener) {
  lockListeners.add(listener);
  return () => lockListeners.delete(listener);
}

/** The server's reason for a refusal: `{"error": …}` when it sends JSON, else its text. */
async function reason(res) {
  const text = await res.text().catch(() => '');
  try {
    const body = JSON.parse(text);
    if (body && typeof body.error === 'string' && body.error) return body.error;
  } catch {
    // Plain text: shown as it is.
  }
  return text || `${res.url} -> ${res.status}`;
}

async function request(url, init = {}) {
  const token = storedToken();
  const headers = { ...init.headers };
  if (token) headers.Authorization = `Bearer ${token}`;
  const res = await fetch(url, { ...init, headers });
  if (res.status === 401) {
    const text = await res.text().catch(() => '');
    let body = {};
    try { body = JSON.parse(text) || {}; } catch { /* not JSON: treat as configured */ }
    // A token that was sent and refused is wrong or rotated; keeping it would
    // only fail again on every poll.
    if (token) forgetToken();
    const locked = new Locked(body.error || 'Unlock required', body.configured !== false, Boolean(token));
    for (const listener of lockListeners) listener(locked);
    throw locked;
  }
  return res;
}

export async function getJson(url) {
  const res = await request(url, { headers: { Accept: 'application/json' } });
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
  const res = await request('/api/run', { method: 'POST' });
  return res.ok;
}

/**
 * The settings the daemon runs with, every field present. Rejects with the
 * server's reason when settings.json cannot be read: the daemon then keeps its
 * last good settings, which the page cannot know, so it must not show defaults.
 */
export async function loadSettings() {
  const res = await request('/api/settings', { headers: { Accept: 'application/json' } });
  if (!res.ok) throw new Error(await reason(res));
  return res.json();
}

/** Rejects with the server's message exactly as sent, so a 400 names the field at fault. */
export async function saveSettings(settings) {
  const res = await request('/api/settings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(settings),
  });
  if (!res.ok) throw new Error(await reason(res));
  return true;
}
