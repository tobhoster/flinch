import React, { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { ChevronRight, Info } from 'lucide-react';
import { GLOSSARY, GLOSSARY_SECTIONS } from './glossary.js';

const WIDTH = 288;
const GAP = 6;
const MARGIN = 8;

/**
 * An (i) button that toggles a short definition from the glossary. Click, tap,
 * Enter or Space opens it; Esc, a second press or a press elsewhere closes it,
 * and Esc hands focus back to the button. The popover is portalled and fixed
 * so no `overflow-hidden` card or transformed sheet can clip it.
 */
export function Explain({ term }) {
  const entry = GLOSSARY[term];
  const id = useId();
  const button = useRef(null);
  const panel = useRef(null);
  const [pos, setPos] = useState(null);
  const open = pos !== null;

  // Centered under the button, kept inside the viewport, flipped above when
  // there is no room below.
  const place = () => {
    const r = button.current.getBoundingClientRect();
    const width = Math.min(WIDTH, window.innerWidth - 2 * MARGIN);
    const left = Math.min(Math.max(r.left + r.width / 2 - width / 2, MARGIN), window.innerWidth - width - MARGIN);
    const height = panel.current?.offsetHeight ?? 0;
    const below = r.bottom + GAP;
    const above = r.top - GAP - height;
    const top = below + height > window.innerHeight - MARGIN && above >= MARGIN ? above : below;
    setPos({ left, top, width });
  };

  // The first placement cannot know the popover's height; measure and re-place.
  useLayoutEffect(() => { if (open) place(); }, [open]);

  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e) => {
      if (e.key !== 'Escape') return;
      // Capture phase, so an enclosing sheet does not close on the same key.
      e.stopPropagation();
      setPos(null);
      button.current?.focus();
    };
    const onDown = (e) => {
      if (!button.current?.contains(e.target) && !panel.current?.contains(e.target)) setPos(null);
    };
    window.addEventListener('keydown', onKey, true);
    document.addEventListener('pointerdown', onDown);
    window.addEventListener('scroll', place, true);
    window.addEventListener('resize', place);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      document.removeEventListener('pointerdown', onDown);
      window.removeEventListener('scroll', place, true);
      window.removeEventListener('resize', place);
    };
  }, [open]);

  const toggle = (e) => {
    // Explainers sit inside clickable rows and cards; the press is ours.
    e.stopPropagation();
    if (open) setPos(null);
    else place();
  };

  return (
    <>
      <button ref={button} type="button" onClick={toggle}
        aria-label={`About ${entry.term}`} aria-expanded={open} aria-controls={open ? id : undefined}
        className={`relative inline-flex h-4 w-4 shrink-0 items-center justify-center self-center rounded-full align-middle transition-colors before:absolute before:-inset-3 before:content-[''] hover:text-fg sm:before:-inset-1 ${open ? 'text-fg' : 'text-fg-faint'}`}>
        <Info size={13} aria-hidden />
      </button>
      {open && createPortal(
        <div ref={panel} id={id} role="region" aria-label={entry.term} onClick={(e) => e.stopPropagation()}
          className="fixed z-[60] rounded-md border border-line bg-ink-800 px-3 py-2 text-left text-xs font-normal leading-snug text-fg-muted shadow-soft"
          style={{ left: pos.left, top: pos.top, width: pos.width }}>
          <p className="mb-0.5 font-medium text-fg">{entry.term}</p>
          <p>{entry.body}</p>
        </div>,
        document.body,
      )}
    </>
  );
}

/** "How FLINCH decides": every glossary entry, collapsed until asked for. */
export function GlossaryCard() {
  return (
    <details className="group rounded-lg border border-line bg-ink-900">
      <summary className="flex min-h-[40px] cursor-pointer list-none items-center gap-2 px-4 py-2.5 text-sm font-medium text-fg [&::-webkit-details-marker]:hidden">
        <ChevronRight size={14} aria-hidden className="text-fg-muted transition-transform group-open:rotate-90" />
        How FLINCH decides
      </summary>
      <div className="gap-8 border-t border-line-soft px-4 pb-1 pt-4 md:columns-2">
        {GLOSSARY_SECTIONS.map(([heading, keys]) => (
          <section key={heading} className="mb-5 min-w-0 break-inside-avoid">
            <h3 className="mb-1.5 text-xs text-fg-muted">{heading}</h3>
            <dl className="space-y-2">
              {keys.map((key) => (
                <div key={key}>
                  <dt className="text-fg">{GLOSSARY[key].term}</dt>
                  <dd className="leading-snug text-fg-muted">{GLOSSARY[key].body}</dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
      </div>
    </details>
  );
}
