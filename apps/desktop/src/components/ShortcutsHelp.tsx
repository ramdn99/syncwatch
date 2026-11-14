import { useEffect, useRef } from 'react';
import { HOST_ONLY, SHORTCUTS } from '../lib/shortcuts';
import { Icon } from '../ui/icons';

/** The keyboard shortcuts overlay (opened with "?" or the Shortcuts button). */
export function ShortcutsHelp({ isHost, onClose }: { isHost: boolean; onClose: () => void }) {
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    closeRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' || e.key === '?') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4" role="dialog" aria-modal="true" aria-labelledby="shortcuts-title">
      <div className="w-full max-w-md rounded-xl border border-border bg-elevated p-5 shadow-2xl">
        <div className="flex items-center justify-between">
          <h2 id="shortcuts-title" className="font-brand text-lg font-bold">
            Keyboard shortcuts
          </h2>
          <button ref={closeRef} type="button" aria-label="Close" title="Close" className="rounded p-1 text-muted hover:text-text" onClick={onClose}>
            <Icon name="close" />
          </button>
        </div>
        <table className="mt-3 w-full text-sm">
          <tbody>
            {SHORTCUTS.map((s) => {
              const hostOnly = HOST_ONLY.has(s.action);
              return (
                <tr key={s.label} className={hostOnly && !isHost ? 'text-muted' : ''} data-testid="shortcut-row">
                  <td className="py-1 pr-3 whitespace-nowrap">
                    {s.keys.map((k) => (
                      <kbd key={k} className="mr-1 rounded border border-border-control bg-surface px-1.5 py-0.5 font-sans text-xs">
                        {k}
                      </kbd>
                    ))}
                  </td>
                  <td className="py-1">
                    {s.label}
                    {hostOnly && <span className="ml-1 text-xs text-muted">{isHost ? '(host)' : '(host only)'}</span>}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        <p className="mt-3 text-xs text-muted">Shortcuts do nothing while you type in the chat or any text field.</p>
      </div>
    </div>
  );
}
