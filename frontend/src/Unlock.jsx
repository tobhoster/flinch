import React, { useState } from 'react';
import { KeyRound, Lock } from 'lucide-react';
import { Card } from './ui.jsx';

/**
 * Shown instead of the dashboard whenever the API answers 401. A server
 * without FLINCH_WEB_TOKEN refuses everyone, so it gets the fix, not an input.
 */
export default function Unlock({ configured, refused, onUnlock }) {
  const [token, setToken] = useState('');

  const submit = (event) => {
    event.preventDefault();
    const value = token.trim();
    if (value) onUnlock(value);
  };

  return (
    <div className="mx-auto flex min-h-screen max-w-sm flex-col justify-center px-4 py-12">
      <span className="mb-4 inline-flex items-center gap-2">
        <img src="/logo-dark.png" alt="" className="h-6 w-6" />
        <span className="text-sm font-semibold tracking-wide text-fg">FLINCH</span>
      </span>
      <Card className="p-5 text-[13px]">
        {configured ? (
          <form onSubmit={submit} className="flex flex-col gap-3">
            <div>
              <p className="inline-flex items-center gap-1.5 font-medium text-fg"><Lock size={13} /> Locked</p>
              <p className="mt-1 text-fg-muted">Enter the access token (FLINCH_WEB_TOKEN) to open the dashboard.</p>
            </div>
            <label htmlFor="flinch-token" className="sr-only">Access token</label>
            <input id="flinch-token" type="password" autoComplete="off" autoFocus
              className="input w-full" placeholder="Access token"
              value={token} onChange={(event) => setToken(event.target.value)} />
            {refused && <p className="text-[12px] text-state-bad">That token was refused. Check it and try again.</p>}
            <button type="submit" disabled={!token.trim()} className="btn btn-primary self-start">
              <KeyRound size={13} /> Unlock
            </button>
          </form>
        ) : (
          <div className="flex flex-col gap-2">
            <p className="inline-flex items-center gap-1.5 font-medium text-state-bad"><Lock size={13} /> No access token set</p>
            <p className="text-fg-muted">
              The server has no FLINCH_WEB_TOKEN, so it refuses every request. Add one to the
              {' '}<code className="text-fg">flinch-secrets</code> Secret, for example from
              {' '}<code className="text-fg">openssl rand -hex 32</code>, then restart flinch-web.
            </p>
          </div>
        )}
      </Card>
    </div>
  );
}
