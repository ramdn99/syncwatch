// Icon-only buttons have an accessible name and a tooltip (Milestone 8 icon rules).
import { cleanup, render } from '@testing-library/react';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { ChangeSourceDialog } from '../components/ChangeSourceDialog';
import { ChatPanel } from '../components/ChatPanel';
import { TrackList } from '../components/TrackList';
import { ICONS, Icon } from './icons';

afterEach(cleanup);

const SRC = join(__dirname, '..');
function tsx(dir: string, out: string[] = []): string[] {
  for (const n of readdirSync(dir)) {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) tsx(p, out);
    else if (n.endsWith('.tsx') && !n.includes('.test.')) out.push(p);
  }
  return out;
}

describe('icon-only buttons', () => {
  it('in the source: every <button> whose only content is an icon has aria-label and title', () => {
    const bad: string[] = [];
    let iconOnly = 0;
    for (const f of tsx(SRC)) {
      const s = readFileSync(f, 'utf8');
      for (const m of s.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)) {
        const [, attrs, inner] = m;
        if (!inner!.includes('<Icon')) continue;
        // Strip the icons and whitespace: anything left is visible text (or a text expression).
        const rest = inner!.replace(/<Icon\b[^>]*\/>/g, '').replace(/\{\s*\}/g, '').trim();
        if (rest !== '') continue;
        iconOnly++;
        if (!/aria-label=/.test(attrs!) || !/title=/.test(attrs!)) bad.push(`${f.replace(SRC, 'src')}: <button${attrs!.slice(0, 80)}...>`);
      }
    }
    expect(bad).toEqual([]);
    expect(iconOnly, 'icon-only buttons found').toBeGreaterThan(3);
  });

  it('rendered: every button has an accessible name', () => {
    const tracks = [{ id: 1, kind: 'audio' as const, title: null, lang: 'eng', codec: 'aac', selected: true, external: false }];
    const views = [
      render(<ChangeSourceDialog inRoom onClose={() => {}} onLocalChosen={() => {}} />),
      render(<ChatPanel messages={[]} onCollapse={() => {}} onChipClick={() => {}} onSend={() => {}} disabledReason={null} />),
      render(<TrackList kind="audio" tracks={tracks as never} isHost mediaKey="a" onSelect={() => {}} onAdd={() => {}} />),
    ];
    for (const v of views) {
      for (const b of v.container.querySelectorAll('button')) {
        const name = (b.getAttribute('aria-label') ?? b.textContent ?? '').trim();
        expect(name, b.outerHTML.slice(0, 120)).not.toBe('');
      }
    }
  });

  it('icons are decorative by default and named when they carry meaning alone', () => {
    const { container } = render(
      <>
        <Icon name="play" />
        <Icon name="lock" label="Only the host can change tracks" />
      </>,
    );
    const [a, b] = Array.from(container.querySelectorAll('svg'));
    expect(a!.getAttribute('aria-hidden')).toBe('true');
    expect(b!.getAttribute('role')).toBe('img');
    expect(b!.getAttribute('aria-label')).toBe('Only the host can change tracks');
    // currentColor: icons follow the text color tokens.
    expect(a!.getAttribute('stroke')).toBe('currentColor');
    expect(Object.keys(ICONS).length).toBeGreaterThan(20);
  });
});
