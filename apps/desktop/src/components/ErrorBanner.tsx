import { ERRORS, type ErrorAction, type ErrorId } from '../strings/errors';
import { Icon } from '../ui/icons';

/** One error state with its one-click action (Milestone 8). */
export function ErrorBanner({ id, onAction, onDismiss }: { id: ErrorId; onAction: (a: ErrorAction) => void; onDismiss?: () => void }) {
  const e = ERRORS[id];
  return (
    <div role="alert" className="flex flex-wrap items-center gap-3 bg-warning/15 px-3 py-1.5 text-sm text-warning" data-testid={`error-${id}`}>
      <Icon name="warning" />
      <span className="flex-1">
        <span className="font-medium">{e.title}.</span> {e.text}
      </span>
      {e.action && (
        <button type="button" className="text-text underline" onClick={() => onAction(e.action!.id)}>
          {e.action.label}
        </button>
      )}
      {onDismiss && (
        <button type="button" className="rounded p-0.5 text-muted hover:text-text" aria-label="Dismiss" title="Dismiss" onClick={onDismiss}>
          <Icon name="close" size="xs" />
        </button>
      )}
    </div>
  );
}
