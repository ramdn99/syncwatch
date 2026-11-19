# GPL and codec notes

**Not legal advice.** These notes record what the project does; check your own obligations
with someone qualified.

## GPL

- SyncWatch ships libmpv built with GPL components, so SyncWatch as a whole is distributed
  under the GPL-3.0 (`LICENSE`).
- The complete source of SyncWatch is public in this repository, and each release has a
  matching tag. The libmpv binary is the unmodified official shinchiro build named in
  `docs/libmpv-build.md`; its source is public at mpv-player/mpv and its build recipe at
  shinchiro/mpv-winbuild-cmake.
- Publishing the source this way is how the project intends to meet the GPL's source
  availability for what it distributes. Whether that fully covers the obligations for the
  bundled libmpv build (for example offering the exact corresponding source of every library
  inside it, or a written offer) is something the maintainer should double-check.
- Every installer includes the license texts (`licenses` folder) and `THIRD-PARTY-NOTICES.md`.

## Codecs and patents

- libmpv includes FFmpeg decoders for formats such as H.264, HEVC, AAC and AC-3. Some of these
  formats are, or were, covered by patents in some countries.
- SyncWatch only decodes media the user already has; it does not encode or sell media.
- Whether a license is needed depends on the country and the use. This project does not
  provide patent licenses.

## Torrents

Users are responsible for what they download and share. The app says so before the first
torrent, and the user guide explains what the swarm can see.
