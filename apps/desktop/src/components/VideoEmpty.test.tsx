import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { PlaybackState } from '../lib/player';
import { EMPTY_ROOM, type RoomState } from '../lib/room';
import { roomEmptyState } from '../RoomView';
import { VideoArea } from './VideoArea';

const NOTHING_LOADED: PlaybackState = {
  paused: true,
  timePos: null,
  duration: null,
  speed: 1,
  pausedForCache: false,
  eofReached: false,
  volume: 100,
  mute: false,
  fileLoaded: false,
  cacheAhead: null,
};

const noop = () => {};
const area = (state: PlaybackState, rs: RoomState) =>
  render(
    <VideoArea
      state={state}
      fullscreen={false}
      markers={[]}
      onTogglePause={noop}
      onSeek={noop}
      onVolume={noop}
      onToggleMute={noop}
      onToggleFullscreen={noop}
      onOpenFile={noop}
      emptyState={roomEmptyState(rs)}
    />,
  );

afterEach(cleanup);

const member: RoomState = { ...EMPTY_ROOM, active: true, isHost: false };

describe('empty player in a new room', () => {
  it('a joiner sees "No video yet" and no file name, and cannot pick one without permission', () => {
    area(NOTHING_LOADED, member);
    expect(screen.getByTestId('video-empty').textContent).toContain('No video yet');
    expect(screen.getByText('The host has not picked a video yet.')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /source|open/i })).toBeNull();
  });

  it('the host is offered to choose a source', () => {
    area(NOTHING_LOADED, { ...EMPTY_ROOM, active: true, isHost: true });
    expect(screen.getByText('No video yet')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Choose a source...' })).toBeTruthy();
  });

  it('a member allowed to change the source gets the button too', () => {
    area(NOTHING_LOADED, { ...member, myPermissions: { ...member.myPermissions, canChangeMedia: true } });
    expect(screen.getByRole('button', { name: 'Choose a source...' })).toBeTruthy();
  });

  it('once a video is loaded the empty state is gone', () => {
    area({ ...NOTHING_LOADED, fileLoaded: true, duration: 10, timePos: 0 }, member);
    expect(screen.queryByTestId('video-empty')).toBeNull();
  });
});
