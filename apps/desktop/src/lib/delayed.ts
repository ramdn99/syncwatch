import { useEffect, useState } from 'react';

/**
 * `true` only once `flag` has stayed true for `ms` (false at once when it drops). The ready-gate
 * banner uses it so the short gate pass of every seek (members finishing their seek) never
 * flashes "Waiting for..." (ADR 0012).
 */
export function useDelayedTrue(flag: boolean, ms: number): boolean {
  const [on, setOn] = useState(false);
  useEffect(() => {
    if (!flag) {
      setOn(false);
      return;
    }
    const t = setTimeout(() => setOn(true), ms);
    return () => clearTimeout(t);
  }, [flag, ms]);
  return flag && on;
}

/** Show delay of the ready-gate banner. */
export const GATE_BANNER_DELAY_MS = 600;
