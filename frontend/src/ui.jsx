// FLINCH UI primitives: flat, dense, neutral. Motion is reserved for the drawer.
import React, { useEffect, useRef, useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { ChevronDown, X } from 'lucide-react';
import { Explain } from './Explain.jsx';

/** Bytes as GiB (2^30), one decimal: the unit every size FLINCH shows is labelled in. */
export const GiB = (b) => ((b || 0) / 2 ** 30).toFixed(1);

export function Card({ className = '', children, ...rest }) {
  return <div className={`card ${className}`} {...rest}>{children}</div>;
}

/** `term` adds a glossary explainer next to the heading. */
export function SectionTitle({ children, hint, term }) {
  return (
    <div className="mb-2 flex flex-wrap items-baseline gap-x-2">
      <h2 className="text-sm font-medium text-fg">{children}</h2>
      {term && <Explain term={term} />}
      {hint && <span className="text-xs text-fg-muted">{hint}</span>}
    </div>
  );
}

/** Whole-unit relative time for a number of seconds in the past. */
export function ago(secs) {
  if (secs < 60) return `${secs} s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)} min ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} h ago`;
  return `${Math.floor(secs / 86400)} d ago`;
}

const DOT_TONES = { ok: 'bg-state-ok', warn: 'bg-state-warn', bad: 'bg-state-bad', info: 'bg-state-info' };

/** Static status marker. */
export function Dot({ tone = 'ok' }) {
  return <span aria-hidden className={`inline-block h-1.5 w-1.5 shrink-0 rounded-full ${DOT_TONES[tone] ?? DOT_TONES.ok}`} />;
}

export function Tooltip({ text, children }) {
  const [open, setOpen] = useState(false);
  const timer = useRef();
  useEffect(() => () => clearTimeout(timer.current), []);
  return (
    <span className="relative inline-flex"
      onMouseEnter={() => { timer.current = setTimeout(() => setOpen(true), 300); }}
      onMouseLeave={() => { clearTimeout(timer.current); setOpen(false); }}>
      {children}
      {open && text && (
        <span role="tooltip"
          className="pointer-events-none absolute bottom-full left-1/2 z-50 mb-1.5 w-max max-w-[16rem] -translate-x-1/2 rounded border border-line bg-ink-800 px-2 py-1 text-xs font-normal normal-case tracking-normal text-fg-muted shadow-soft">
          {text}
        </span>
      )}
    </span>
  );
}

/** Underlined text tabs. `tabs` is `[key, label, Icon?][]`; the icon is optional. */
export function Tabs({ tabs, value, onChange }) {
  return (
    <div role="tablist" className="flex items-center gap-5 border-b border-line">
      {tabs.map(([key, label, Icon]) => {
        const active = value === key;
        return (
          <button key={key} role="tab" aria-selected={active} onClick={() => onChange(key)}
            className={`-mb-px flex min-h-[40px] shrink-0 items-center gap-1.5 border-b-2 py-2 text-[13px] transition-colors sm:min-h-0 ${active ? 'border-fg font-medium text-fg' : 'border-transparent text-fg-muted hover:text-fg'}`}>
            {Icon && <Icon size={13} />}{label}
          </button>
        );
      })}
    </div>
  );
}

export function DropdownMenu({ items, label = 'Actions', align = 'right' }) {
  const [open, setOpen] = useState(false);
  const ref = useRef();
  useEffect(() => {
    const onDoc = (e) => { if (ref.current && !ref.current.contains(e.target)) setOpen(false); };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, []);
  return (
    <div className="relative inline-block" ref={ref}>
      <button className="btn min-w-[40px] justify-center px-1.5 py-1 sm:min-w-0" onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }} aria-label={label}>
        <ChevronDown size={14} />
      </button>
      {open && (
        <div className={`absolute z-50 mt-1 w-56 overflow-hidden rounded-md border border-line bg-ink-800 py-1 shadow-soft ${align === 'right' ? 'right-0' : 'left-0'}`}>
          {items.map((item, i) =>
            item.separator ? <div key={i} className="my-1 h-px bg-line-soft" /> : (
              <button key={i}
                onClick={(e) => { e.stopPropagation(); setOpen(false); item.onSelect?.(); }}
                className="flex min-h-[40px] w-full items-center gap-2 px-3 py-1.5 text-left text-[13px] text-fg-muted hover:bg-ink-700 hover:text-fg sm:min-h-0">
                {item.icon}{item.label}
              </button>
            ))}
        </div>
      )}
    </div>
  );
}

export function Sheet({ open, onClose, title, children }) {
  useEffect(() => {
    const onKey = (e) => e.key === 'Escape' && onClose();
    if (open) document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open, onClose]);
  return (
    <AnimatePresence>
      {open && (
        <>
          <motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={{ duration: 0.12 }}
            className="fixed inset-0 z-40 bg-black/50" onClick={onClose} />
          <motion.aside initial={{ x: 16, opacity: 0 }} animate={{ x: 0, opacity: 1 }} exit={{ x: 16, opacity: 0 }}
            transition={{ duration: 0.15, ease: 'easeOut' }}
            className="fixed inset-0 z-50 overflow-y-auto border-l border-line bg-ink-900 p-4 sm:inset-y-0 sm:left-auto sm:right-0 sm:h-full sm:w-full sm:max-w-md sm:p-5">
            {/* Phone: full-bleed panel with the close button pinned to a sticky header. From `sm` up
                the header is static. */}
            <div className="sticky top-0 z-10 -mx-4 -mt-4 mb-4 flex items-center justify-between gap-3 border-b border-line-soft bg-ink-900 px-4 py-2 sm:static sm:mx-0 sm:mt-0 sm:border-b-0 sm:bg-transparent sm:p-0">
              <h3 className="text-sm font-medium">{title}</h3>
              <button className="btn min-w-[40px] justify-center px-1.5 py-1 sm:min-w-0" onClick={onClose} aria-label="Close"><X size={14} /></button>
            </div>
            {children}
          </motion.aside>
        </>
      )}
    </AnimatePresence>
  );
}

export function EmptyState({ icon: Icon, title, body }) {
  return (
    <div className="flex flex-col items-center gap-1.5 rounded-md border border-dashed border-line px-6 py-10 text-center">
      {Icon && <Icon size={16} className="text-fg-faint" />}
      <p className="text-[13px] font-medium text-fg-muted">{title}</p>
      {body && <p className="max-w-sm text-xs text-fg-faint">{body}</p>}
    </div>
  );
}
