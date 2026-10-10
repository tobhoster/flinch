import React, { useEffect, useRef, useState } from 'react';
import { KeyRound, Loader2, Lock, LogIn } from 'lucide-react';
import { logIn } from './api.js';
import { Card } from './ui.jsx';

// The single sign-on callback sends the browser back with `?sso=failed` or
// `?sso=denied`; the server's log has the details, never this page.
const SSO_OUTCOMES = {
  failed: 'Single sign-on did not finish. Try again; if it keeps failing, flinch-web\'s log says why.',
  denied: 'You signed in, but that account may not open FLINCH. Ask whoever runs FLINCH to add your email, '
    + 'group or subject to FLINCH_WEB_OIDC_ALLOWED_EMAILS, _GROUPS or _SUBJECTS.',
};

/** The `?sso=` outcome this page was opened with, read once and removed from the address bar. */
function takeSsoOutcome() {
  const params = new URLSearchParams(window.location.search);
  const outcome = SSO_OUTCOMES[params.get('sso')] || '';
  if (params.has('sso')) {
    params.delete('sso');
    const query = params.toString();
    window.history.replaceState(null, '', `${window.location.pathname}${query ? `?${query}` : ''}${window.location.hash}`);
  }
  return outcome;
}

/**
 * Shown until this browser has a session: in place of the dashboard, or over
 * it when a session ends mid-use, with the server's `reason`. It offers the
 * password form when FLINCH_WEB_USERNAME and FLINCH_WEB_PASSWORD are set, and
 * a "Sign in with …" button when single sign-on is (`sso`: `{ name, login }`).
 * A server with neither has no login to offer, so it gets the fix, not a form.
 */
export default function Login({ loginConfigured, sso = null, reason = '', onLogin }) {
  const [error, setError] = useState('');
  const [ssoOutcome] = useState(takeSsoOutcome);
  const [submitting, setSubmitting] = useState(false);
  const passwordRef = useRef(null);

  // After a refusal the password is cleared and focused, ready to type again.
  useEffect(() => {
    if (error) passwordRef.current?.focus();
  }, [error]);

  // The values are read from the form, not kept in state: a password manager
  // may fill the fields before any change event reaches React.
  const submit = async (event) => {
    event.preventDefault();
    if (submitting) return;
    const form = new FormData(event.currentTarget);
    setSubmitting(true);
    setError('');
    try {
      await logIn(String(form.get('username') || ''), String(form.get('password') || ''));
      onLogin();
    } catch (err) {
      // fetch rejects with a TypeError when nothing answered at all.
      setError(err instanceof TypeError ? 'FLINCH did not answer. Check the connection and try again.' : err.message);
      if (passwordRef.current) passwordRef.current.value = '';
      setSubmitting(false);
    }
  };

  const heading = (
    <div>
      <p className="inline-flex items-center gap-1.5 font-medium text-fg"><Lock size={13} /> Log in</p>
      {reason && <p className="mt-1 text-fg-muted">{reason}</p>}
    </div>
  );

  return (
    <div className="mx-auto flex min-h-screen max-w-sm flex-col justify-center px-4 py-12">
      <span className="mb-4 inline-flex items-center gap-2">
        <img src="/logo-dark.png" alt="" className="h-6 w-6" />
        <span className="text-sm font-semibold tracking-wide text-fg">FLINCH</span>
      </span>
      <Card className="p-5 text-[13px]">
        {loginConfigured || sso ? (
          <div className="flex flex-col gap-3">
            {heading}
            {sso && (
              <>
                {/* A full navigation, not fetch: the provider's page takes over the tab. */}
                <a href={sso.login} className={`btn ${loginConfigured ? '' : 'btn-primary'} self-start`}>
                  <KeyRound size={13} /> Sign in with {sso.name}
                </a>
                {ssoOutcome && <p role="alert" className="text-[12px] text-state-bad">{ssoOutcome}</p>}
              </>
            )}
            {sso && loginConfigured && <p className="text-[12px] text-fg-muted">or with the FLINCH login:</p>}
            {loginConfigured && (
              <form onSubmit={submit} className="flex flex-col gap-3">
                {/* Read-only, not disabled, while sending: a disabled field loses focus. */}
                <label className="flex flex-col gap-1">
                  <span className="text-fg-muted">Username</span>
                  <input name="username" type="text" autoComplete="username" autoCapitalize="none" spellCheck={false}
                    autoFocus={!sso} required readOnly={submitting} className="input w-full" />
                </label>
                <label className="flex flex-col gap-1">
                  <span className="text-fg-muted">Password</span>
                  <input ref={passwordRef} name="password" type="password" autoComplete="current-password" required
                    readOnly={submitting} className="input w-full" />
                </label>
                {error && <p role="alert" className="text-[12px] text-state-bad">{error}</p>}
                <button type="submit" disabled={submitting} className="btn btn-primary self-start">
                  {submitting ? <Loader2 size={13} className="animate-spin" /> : <LogIn size={13} />}
                  {submitting ? 'Logging in…' : 'Log in'}
                </button>
              </form>
            )}
          </div>
        ) : (
          <div className="flex flex-col gap-2">
            <p className="inline-flex items-center gap-1.5 font-medium text-state-bad"><Lock size={13} /> No login set</p>
            <p className="text-fg-muted">
              The server has no FLINCH_WEB_USERNAME and FLINCH_WEB_PASSWORD, so nobody can log in. Add both to the
              {' '}<code className="text-fg">flinch-secrets</code> Secret, apply the release's manifests (they pass
              both to flinch-web), then restart flinch-web. Single sign-on (FLINCH_WEB_OIDC_*) works instead of or
              beside them. Automations keep working with the API key (FLINCH_WEB_TOKEN) when one is set.
            </p>
          </div>
        )}
      </Card>
    </div>
  );
}
