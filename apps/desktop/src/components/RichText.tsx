import type { Rich } from '../strings/connection-help';

/** Renders help text with bold UI names (plain text only; never HTML). */
export function RichText({ value }: { value: Rich }) {
  return (
    <>
      {value.map((part, i) =>
        typeof part === 'string' ? (
          <span key={i}>{part}</span>
        ) : (
          <strong key={i} className="font-semibold text-text">
            {part.b}
          </strong>
        ),
      )}
    </>
  );
}
