# SyncWatch user guide

Status: started in Milestone 7 with the torrent section. The full guide is written in
Milestone 9.

## Watching a torrent together

- **Host:** open Change source, then **Torrent**. There are two ways to add one:
  - **Magnet link:** paste it and choose **Get file list**. SyncWatch asks peers for the
    torrent's file list. This can take a moment, and it fails after 90 s if nobody shares the
    torrent; the message names the likely cause (nobody sharing it, a firewall, DHT off).
  - **Torrent file:** choose **Choose .torrent...** or drop a `.torrent` file on the dialog. Its
    name, infohash, total size and file list show at once, with no network.

  Then pick the video file. Only that file is downloaded, by everyone.
- **Members** see the torrent's name, the file the host chose and its size, and choose
  **Download and play** or **Decline**. Nothing is downloaded before they agree. Members never
  look for the file list themselves: they get it from the host. If the host is still getting it,
  they see "Waiting for the host to get the file list" and start as soon as it arrives.
- Everyone downloads their own copy, starting with the part the room is playing. A member who
  joins while the video plays starts right there, within seconds, not from the beginning of the
  file. Only the few hundred KB the player needs to open the file are taken from elsewhere.
  Playback stays in sync while downloading.
- **What counts is the part being played, not the total.** A member at 40% whose current part
  is downloaded is ready and never makes the room wait. Every member's download always fetches
  the part the room is playing first, also while it is still loading or waiting.
- **Seeking to a part that is not downloaded yet** pauses the room until that part has
  arrived. Then everyone continues together.
  - The member who is missing it sees "Buffering 60%: at 12:30, needs ~3 MB more to continue".
  - The host sees "Waiting for Bo: buffering at 12:30 (needs ~3 MB more)", with **Resume
    without them**. The members list shows "buffering at 12:30, 60% of the window".
  - The other members see "Waiting for Bo to catch up".

  Seeking back to downloaded parts is instant.
- The buffering message appears only when your own video really stops for missing data (after
  a short moment), and disappears as soon as it plays again.
- **No peers:** SyncWatch shows "waiting for peers" and retries by itself. What is already
  downloaded keeps playing.

### What you see while waiting

- "Waiting for the host to get the file list": a member is waiting for the host.
- "Looking for peers (DHT on, 3 trackers)": the host is fetching the file list.
- "4 peers found": downloading.
- "The torrent file was loaded, but no peers are sharing it yet": nobody shares the torrent.
  SyncWatch retries by itself.

SyncWatch cannot use **web seeds** (HTTP download addresses inside some torrents, common on the
Internet Archive). A torrent that relies only on web seeds and has no peers will not download.
The message then says so.

### When the download is complete

SyncWatch says: **"Downloading and sharing have stopped. No further traffic."**

At that moment it disconnects from every peer, tells the trackers it stopped, and shuts the
torrent engine down. That means no DHT, no listening port and no open connection. The video
keeps playing from the finished file, without a reload.

Other DHT nodes may keep your address listed for about 30 minutes; the DHT protocol has no way
to take a listing back. Nobody can connect to you during that time: every socket is closed and
the DHT is shut down.

When you join the room again, or restart SyncWatch, the finished file plays directly.
SyncWatch never joins the torrent's swarm again for it. There is no "keep seeding" option.

### Settings (Settings, then Torrents)

- Cache folder and size limit.
- Whether finished files are kept after leaving a room.
- Incoming port and bind address.
- DHT on or off.

**Your responsibility:** you are responsible for what you stream. Only use torrents you have
the right to download and share. While a torrent downloads, other people in its swarm can see
your IP address.
