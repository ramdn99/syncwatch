# Change source: real click-through check

Run on 2026-11-14 between 14:00 and 14:10 Cairo time, inside the user's window for mouse and
keyboard input.

**Setup:**
- Two app windows (`scripts/dev/two-windows.ps1 -AutoRoom`): host A and member B in one room.
- qBittorrent as the only seeder (`scripts/dev/torrent-lab.ps1 -Manual`).

**Method:**
- **Input:** real mouse clicks (`SetCursorPos` + `mouse_event`) at positions read from window
  captures. Real keystrokes through `SendKeys`: Ctrl+V to paste, typing paths into the native
  file dialog, Enter.
- **Capture:** only the app window, by handle (`PrintWindow`), after each step. The screen was
  never read.

## Results

| Step | Result |
|---|---|
| Host: "Choose a source..." opens Change source on the Local file tab | Works |
| Torrent tab: magnet field and "Choose .torrent..." | Works |
| Paste a multi-file magnet (Ctrl+V) | Works: the name "SyncWatch Lab Pack" and the infohash show at once |
| Get file list, then the file picker for a multi-file torrent | Works: 3 files. `long.mkv` (77.4 MB) is preselected and the trailer is selectable. `readme.nfo` is greyed out (not video). "Only the selected file is downloaded." |
| Member: confirm prompt | Works: torrent name, chosen file and size, the responsibility notice, Decline and "Download and play" |
| Member declines | Works: the host shows Member B as "Declined" and the toast "A member declined the torrent." |
| Switch magnet to .torrent file (picked through the native Open dialog) | Works: "SyncWatch Test Film.mkv", 41.6 MB, 1 file, 1 tracker; the member is asked again |
| Member accepts | Works: Ready, the player opens (34:00), downloading with 2 peers |
| Switch .torrent to a local file (native dialog, `test.mkv`, "1.29 GB · Identity checked") | Works: the member gets "Choose your copy of the video" once; after choosing, Ready (28:28) with its tracks |
| Switch the local file back to the magnet | Works: the member is asked, accepts, Ready, downloading |
| Drag and drop of a .torrent onto the dialog | **Not checked by hand.** It needs a drag from Explorer, a window that may not be captured. The drop code path is the same one the picker feeds (`openTorrentFile`). |

## Finding (fixed in the same commit)

- The member's room panel kept showing "Choose a different file..." while the room's source was
  a torrent. That button only makes sense for a local file, so it is now shown only then.

## Not covered here

- Real drag and drop from Explorer.
- Screenshots of these steps are not stored in the repo.
