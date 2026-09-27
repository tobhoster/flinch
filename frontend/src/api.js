// Every call to the FLINCH API goes through `request`, so the session cookie
// and the UI's own header go out in one place and a refusal locks the whole
// app, not one panel.

// Versions before the login kept an access token here; the session cookie,
// which scripts cannot read, replaced it.
try {
  localStorage.removeItem('flinch-token');
} catch {
  // Storage unavailable: nothing was kept.
}

/** Thrown for a 401. `loginConfigured` is false when the server has no login set. */
export class Locked extends Error {
  constructor(message, loginConfigured) {
    super(message);
    this.loginConfigured = loginConfigured;
  }
}

const lockListeners = new Set();

/** Called with the `Locked` error on every 401; returns the unsubscribe. */
export function onLocked(listener) {
  lockListeners.add(listener);
  return () => lockListeners.delete(listener);
}

/**
 * The server's reason for a refusal: `{"error": …}` when it sends JSON, else
 * its text. An HTML page is a proxy's (a 502 while flinch-web restarts), not
 * FLINCH's, and never shown as markup.
 */
async function reason(res) {
  const text = await res.text().catch(() => '');
  try {
    const body = JSON.parse(text);
    if (body && typeof body.error === 'string' && body.error) return body.error;
  } catch {
    // Plain text: shown as it is.
  }
  if ((res.headers.get('Content-Type') || '').includes('text/html')) {
    return `FLINCH answered ${res.status}; try again in a moment.`;
  }
  return text || `${res.url} -> ${res.status}`;
}

/**
 * Sends the session cookie, to this origin only, and `X-Flinch-Request: 1`:
 * the server refuses a write made with the cookie without it, and a page on
 * another site cannot add it.
 */
function send(url, init = {}) {
  return fetch(url, { ...init, credentials: 'same-origin', headers: { 'X-Flinch-Request': '1', ...init.headers } });
}

async function request(url, init = {}) {
  const res = await send(url, init);
  if (res.status === 401) {
    const text = await res.text().catch(() => '');
    let body = {};
    try { body = JSON.parse(text) || {}; } catch { /* not JSON: treat the login as set up */ }
    const locked = new Locked(body.error || 'Log in required', body.login_configured !== false);
    for (const listener of lockListeners) listener(locked);
    throw locked;
  }
  return res;
}

/**
 * `{ authenticated, login_configured }`: whether to show the dashboard, the
 * login, or how to set one up. Gives up after 10 s, so a hung server cannot
 * keep the page blank.
 */
export async function loadSession() {
  const res = await send('/api/session', { headers: { Accept: 'application/json' }, signal: AbortSignal.timeout(10000) });
  if (!res.ok) throw new Error(`/api/session -> ${res.status}`);
  return res.json();
}

/** Resolves once the server set the session cookie; rejects with its reason (a wrong login, or too many tries). */
export async function logIn(username, password) {
  const res = await send('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
  if (!res.ok) throw new Error(await reason(res));
}

/** Ends the session on the server and clears its cookie. */
export async function logOut() {
  const res = await send('/api/logout', { method: 'POST' });
  if (!res.ok) throw new Error(await reason(res));
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
