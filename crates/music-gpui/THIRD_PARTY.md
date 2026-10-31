# Third-party material in music-gpui

## Translations

The UI translations in `locales/<locale>/*.lang` are partly from **ArchiveTune**'s
Weblate project (the community translations of the Android app ArchiveTune, © Rukamori
and ArchiveTune contributors, `app/src/main/res/values-*/strings.xml`), licensed under
the GNU General Public License v3.0 — the license of this project as a whole.

Where one of our English strings has the same meaning as an ArchiveTune string, its
translation was reused (adapted to our placeholders, punctuation and terminology); all
other strings were translated for this project. No ArchiveTune name or branding is used:
the app's name comes from `packaging/app.env` through the `{app}` placeholder.

ArchiveTune's code ported into this project is listed in
[`../music-core/THIRD_PARTY.md`](../music-core/THIRD_PARTY.md).

## Zuno and Limusic (queue round)

The queue sections and tabs, multi-select, the output-device filter, the log viewer and
the Music Together suggestion approval follow ideas from Zuno (Apache-2.0) and Limusic
(GPL-3.0); the ported pieces are listed in
[`../music-core/THIRD_PARTY.md`](../music-core/THIRD_PARTY.md#zuno-and-limusic-featqueue-ux).

## Zuno and Limusic (lyrics sources and timing)

Written for this project after the approach of **Zuno** (<https://github.com/noFAYZ/zuno>,
© noFAYZ, Apache License 2.0 — compatible with GPL-3.0) and **Limusic**
(<https://github.com/SimoHypers/limusic>, © SimoHypers, GPL-3.0); no file was copied. See
[`../music-core/THIRD_PARTY.md`](../music-core/THIRD_PARTY.md) for the lyrics service.

| music-gpui file | Upstream file(s) | What was adapted |
|---|---|---|
| `src/lyrics.rs` (`TrackLyricsStore`, `choose_lyrics_source`, `nudge_lyrics`) | Zuno `src/ui/settings/lyricsOffset.ts` (per-track offset, clamped and snapped to its step, oldest entries pruned), `src/internal/lyricsSourcePreference.ts`; Limusic `src-tauri/src/lyrics.rs` (`choose_source`, `set_offset`: a hand-picked source pinned per song) | One JSON file next to the settings for both; 0.1 s steps up to ±10 s; 2,000 tracks kept; a picked source is asked in the first wave and stored in the lyrics cache. |
| `src/np_lyrics.rs` (`source_button`, `nudge_controls`) | Zuno `src/ui/pages/LyricsView.tsx` (`LyricsSourcePanel`, `LyricsOffsetControl`); Limusic `ui/src/lib/components/LyricsView.svelte` (source picker in the lyrics footer) | A GPUI dropdown menu on the "Lyrics from …" label: every provider in the order asked, with result, sync kind, length fit and time; found ones switch at once, others are asked on click. |
