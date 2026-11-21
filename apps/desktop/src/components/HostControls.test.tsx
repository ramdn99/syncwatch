// Host controls render only from the room state (Milestone 9 finding: after collapsing and
// re-opening the section, the preset, "Wait for everyone" and the delays showed defaults).
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { EMPTY_ROOM, type RoomState } from '../lib/room';
import { HostControls } from './HostControls';

afterEach(cleanup);

const rs = (over: Partial<NonNullable<RoomState['hostSettings']>> = {}, extra: Partial<RoomState> = {}): RoomState => ({
  ...EMPTY_ROOM,
  active: true,
  isHost: true,
  chatState: { enabled: false, slowModeSecs: 30, muted: false },
  hostSettings: { preset: 'onlyMe', waitForEveryone: true, subDelayMs: 300, audioDelayMs: -200, locked: true, ...over },
  ...extra,
});

function show(room: RoomState, onAction = vi.fn()) {
  return render(
    <HostControls
      isHost
      room={room}
      tracks={[]}
      mediaKey="m"
      hasMissingFile={false}
      onSelectTrack={() => {}}
      onAddTrack={() => {}}
      onChangeSource={() => {}}
      onChooseFile={() => {}}
      onAction={onAction}
      noLimit={false}
    />,
  );
}

function shown() {
  return {
    everyone: screen.getByRole('radio', { name: 'Everyone' }).getAttribute('aria-checked'),
    onlyMe: screen.getByRole('radio', { name: 'Only me' }).getAttribute('aria-checked'),
    wait: (screen.getByRole('checkbox', { name: 'Wait for everyone before playing' }) as HTMLInputElement).checked,
    sub: (screen.getByLabelText('Subtitle delay in milliseconds') as HTMLInputElement).value,
    audio: (screen.getByLabelText('Audio delay in milliseconds') as HTMLInputElement).value,
    locked: (screen.getByRole('checkbox', { name: 'Lock the room (no new people)' }) as HTMLInputElement).checked,
    chat: (screen.getByRole('checkbox', { name: 'Chat enabled' }) as HTMLInputElement).checked,
    slow: (screen.getByRole('combobox') as HTMLSelectElement).value,
  };
}

describe('Host controls show the room state, before and after collapsing', () => {
  it('collapse (unmount) and re-open (mount) show exactly the same values', () => {
    const room = rs();
    const first = show(room);
    const before = shown();
    expect(before).toEqual({ everyone: 'false', onlyMe: 'true', wait: true, sub: '300', audio: '-200', locked: true, chat: false, slow: '30' });
    first.unmount(); // the section collapses
    show(room); // and opens again
    expect(shown()).toEqual(before);
  });

  it('a click sends the action; what is shown follows the room state, not the click', () => {
    const onAction = vi.fn();
    const v = show(rs(), onAction);
    fireEvent.click(screen.getByRole('radio', { name: 'Everyone' }));
    expect(onAction).toHaveBeenCalledWith({ type: 'preset', preset: 'everyone' });
    expect(shown().onlyMe).toBe('true'); // until the host state says otherwise
    fireEvent.click(screen.getByRole('checkbox', { name: 'Wait for everyone before playing' }));
    expect(onAction).toHaveBeenCalledWith({ type: 'waitForEveryone', on: false });
    v.rerender(
      <HostControls isHost room={rs({ preset: 'everyone', waitForEveryone: false })} tracks={[]} mediaKey="m" hasMissingFile={false} onSelectTrack={() => {}} onAddTrack={() => {}} onChangeSource={() => {}} onChooseFile={() => {}} onAction={onAction} />,
    );
    expect(shown()).toMatchObject({ everyone: 'true', onlyMe: 'false', wait: false });
  });

  it('a delay is a draft only while typing; Enter sends it; the field then shows the room value', () => {
    const onAction = vi.fn();
    show(rs(), onAction);
    const sub = screen.getByLabelText('Subtitle delay in milliseconds') as HTMLInputElement;
    fireEvent.focus(sub);
    fireEvent.change(sub, { target: { value: '500' } });
    expect(sub.value).toBe('500');
    fireEvent.keyDown(sub, { key: 'Enter' });
    expect(onAction).toHaveBeenCalledWith({ type: 'setDelay', kind: 'sub', ms: 500 });
    expect(sub.value).toBe('300'); // the room still says 300 until the host state changes
  });

  it('custom default permissions are named, no preset is highlighted', () => {
    show(rs({ preset: 'custom' }));
    expect(shown()).toMatchObject({ everyone: 'false', onlyMe: 'false' });
    expect(screen.getByTestId('preset-note').textContent).toContain('Custom default permissions.');
  });
});
