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

## Zuno, Limusic and Deviload (Now Playing, mini player and sound extras)

Written for this project after the apps below; no file was copied. Upstream commits:
Zuno `1fa9a9669e4a526ebe53c0275153feda0c4fcefa` (Apache-2.0), Limusic
`d82372cdd3016e3c5e28ea8022596b7fefdf681a` (GPL-3.0), Deviload
(<https://github.com/Pronexsteam/deviload>, © Samvel Avetisyan)
`70d1d951147d8882996c544f4061faea9100752f` (MIT). refryt
(`e9685b0b49ff2fd094d60abc565a57494963755d`) was surveyed; nothing came from it.

| music-gpui file | Upstream file(s) | What was adapted |
|---|---|---|
| `src/mini_player.rs` (`MiniPlayerSkin` in `settings/prefs.rs`) | Zuno `src/ui/settings/miniPlayer.ts`, `src/ui/components/mini-player/**` (skins with their own window sizes, the wheel changes the volume); Limusic `ui/src/lib/components/MiniPlayer.svelte`, `src-tauri/src/mini.rs` (compact size, lyrics in the mini player) | Five layouts (bar, compact, artwork card, lyrics, turntable), each a fixed window size; switching reopens the window where it was; the wheel changes the volume as over the bar's volume control (4 % a notch). |
| `src/turntable.rs` | Zuno `src/ui/components/mini-player/skins/VinylSkin.tsx` (a tonearm from about 3° to 26° showing the progress, lifted while paused); Deviload `src/music-picture-disc.js` / `.css` (the artwork as the record's label, grooves) | The arm angle from the pivot / record geometry (law of cosines) instead of a fixed range; no spinning (GPUI cannot rotate images and it would redraw continuously); used by the mini player and a Now Playing design. |
| `src/settings/prefs.rs` (`SoundPreset`) | Limusic `ui/src/lib/effect-presets.ts` | Its speed and pitch values for Slowed + reverb (0.85×, −3 st), Nightcore (1.2×, +3 st), Practice (0.75×) and Concert hall, mapped to our reverb rooms and mixes. |
| `src/components/sound_menu.rs` | Zuno `src/ui/components/player/PlaybackOptions.tsx` (one popover for the playback options and the EQ presets); Limusic's effects button that lights up while an effect is on (`PlayerBar.svelte`) | Submenus for the EQ presets, reverb rooms and sound presets in the player bar's quality menu and a Now Playing header button that is highlighted while the sound is changed. |
| `src/components/np_visualizer.rs` (Ambient lights) | Deviload `src/music-ambient.js`, `src/music-ambient-light.js`, `docs/music-ambient.md` | Three lights in the artwork's colours for the lows, mids and highs, opacity 0.17 + level × 0.76 and a little growth, drawn from the analyzer's 16 bands with the pre-rendered glow images instead of CSS blur. |

## Zuno, Limusic and Deviload (sound round)

Written for this project after the apps below; no file was copied. Upstream commits: Zuno
(<https://github.com/noFAYZ/zuno>, © noFAYZ) `1fa9a9669e4a526ebe53c0275153feda0c4fcefa`
(Apache-2.0), Limusic (<https://github.com/SimoHypers/limusic>, © SimoHypers)
`d82372cdd3016e3c5e28ea8022596b7fefdf681a` (GPL-3.0), Deviload
(<https://github.com/Pronexsteam/deviload>, © Samvel Avetisyan)
`7348689cf292e175769f1a7a3d7000d3fb697178` (MIT; the night-mode values below are the only
numbers taken from it, and the permission notice of Deviload's `LICENSE` applies to them).
refryt (`e9685b0b49ff2fd094d60abc565a57494963755d`, GPL-3.0) was surveyed; nothing came
from it.

| music-gpui file | Upstream file(s) | What was adapted |
|---|---|---|
| `src/settings/eq_graph.rs`, `src/settings/sound.rs` (`EqCurve`, `band_name`) | Zuno `src/ui/components/Equalizer.tsx` (`EqualizerGraph`: the real filter response drawn over ±15 dB, grid at ±12 / ±6 / 0, a press on a column sets that band, the part above 0 dB marked; `BAND_ROLES`), `src/ui/settings/equalizerCurve.ts` (`responseCurveDb`, `autoPreampDb`) | The curve from `music_core::equalizer::config_response_db` (the DSP's own biquads) at 160 log-spaced points, recomputed only when a gain changes; the over-0-dB part tinted only while automatic pre-gain is off; band names Sub, Bass, Punch, Warmth, Low mids, Mids, Upper mids, Presence, Brilliance, Air (some of Zuno's roles, some ours). |
| `src/settings/file.rs` (`SavedEq`), `src/settings/sound.rs` (`save_eq_preset`, …) | Zuno `src/ui/settings/equalizer.ts` (user presets beside the built-ins, saving under an existing name updates it, rename, delete) | Stored in `settings.json`, names unique ignoring case, at most 50; listed in the Sound menu. |
| `src/settings/prefs.rs` (`UserSoundPreset`) | Limusic `ui/src/lib/effect-presets.ts`, `src-tauri/src/state.rs` (`playback_params`: speed, semitones and reverb saved together) | Plus the reverb's fine values or impulse file and the equalizer; listed with the built-in presets in Settings and the Sound menu. |
| `src/settings/sound.rs` (reverb fine controls), `src/settings/equalizer.rs` | Limusic `ui/src/lib/components/TempoPitchDialog.svelte`, `crates/player/src/lib.rs` (`Reverb`: decay, pre-delay 0–200 ms, damping, reflections, dry level; impulse responses through FFmpeg's `afir`) | Mapped onto `music_core::ReverbParams`; the four rooms are presets of them; an impulse file is decoded by Symphonia and convolved in our own engine (`music_core::convolver`). |
| `src/settings/sound.rs` (night mode), `src/components/sound_menu.rs` | Deviload `src/music-sound.js` ("volume smoothing": a compressor with threshold −24 dB, ratio 3) | A switch in the Sound menu and Settings with threshold, ratio, attack, release and makeup sliders (`music_core::compressor`). |
| `src/clock.rs` (`Engine::set_volume`), `src/settings/file.rs` (`migrate`) | Deviload's squared volume (`p²`); Limusic's `10^(−(1−p)^1.5)` compared | The square, applied where the slider's value reaches the player; settings files of version 1 have their volume converted to the slider position of the same gain. |

## refryt, Deviload and Limusic (look round)

Written for this project after ideas from **refryt** (<https://github.com/gridoe/refryt> at
`e9685b0b49ff2fd094d60abc565a57494963755d`, © gridoe, GPL-3.0-or-later), **Deviload**
(<https://github.com/Pronexsteam/deviload> at `70d1d951147d8882996c544f4061faea9100752f`,
© 2026 Samvel Avetisyan, MIT) and **Limusic** (<https://github.com/SimoHypers/limusic> at
`d82372cdd3016e3c5e28ea8022596b7fefdf681a`, © SimoHypers, GPL-3.0). No file was copied;
Deviload's envelope constants were ported (MIT notice: the permission notice of
Deviload's `LICENSE` applies to them).

| music-gpui file | Upstream file(s) | What was adapted |
|---|---|---|
| `src/settings/theme_files.rs`, `src/settings/theme_library.rs` | refryt `crates/refryt/src/theme/custom.rs`, `docs/THEMES.md` (themes as files in the config folder, an "Open folder" button, errors shown with the file and line, a contrast warning under 4.5:1, built-ins written once and deleted ones staying deleted) | TOML instead of CSS; the file names a base theme, the Customize-theme options and any of gpui-component's colour tokens; the folder is watched (`notify`) instead of re-read on focus. |
| `src/theme.rs` (`lifted_muted`, `readable_window_alpha`) | refryt `crates/refryt/src/theme.rs` (`with_opacity`: faint text pulled towards the text colour as the window thins) | Our own curve (25 % of the way at 50 % opacity) plus a floor: the opacity is raised until text keeps 4.5:1 (muted 3:1) over a white desktop. |
| `src/components/ambient.rs` (`Envelope`) | Deviload `src/music-ambient-model.js`, `src/music-ambient.js` (bass onset over a 0.24 s baseline; targets `bass·0.95 + onset·2.65`, `mid·1.2`, `high·1.3`; attack 25 ms, release 260 ms; ~30 fps, stopped when paused, hidden or with reduced motion) | Bands from our spectrum tap (35 Hz–9 kHz in three log bands); the energy scales the app backdrop's opacity (55–100 % of the look's own) and the glow's reach. |
| `src/app_icon.rs` | Limusic `src-tauri/src/appicon.rs` (a user PNG copied to the app's data folder and applied at runtime) | Plus bundled colour variants of our own logo; macOS Dock via `setApplicationIconImage:`, Windows `WM_SETICON`, X11 at window open. |
| `src/zoom.rs` | Limusic `ui/src/lib/zoom.svelte.ts` (fixed zoom levels, Ctrl ± / 0 and Ctrl + wheel, persisted) | Steps the existing text size (GPUI's rem size) through 75–150 %. |

The starter theme files (`themes/*.toml`) use the published colour palettes of **Nord**
(<https://github.com/nordtheme/nord> at `1cef71605416a222e57225b544540ce0fcec18d4`, MIT),
**Catppuccin** (<https://github.com/catppuccin/palette> at
`07d02aa110ef9eb7e7427afca5c73ba9cf7f8ebd`, MIT), **Gruvbox**
(<https://github.com/morhetz/gruvbox> at `ef8864bb42bf244f0295d1c5a403b27e3d139695`, MIT)
and **Dracula** (<https://github.com/dracula/dracula-theme> at
`b361e717c85fdc0cdc2e7a0adae392613b3d2117`, MIT): hex values only, mapped to our tokens,
with secondary text lightened where needed to reach 3:1.

## refryt and Zuno (sidebar and translucency round)

Written for this project after **refryt** (<https://github.com/gridoe/refryt> at
`e9685b0b49ff2fd094d60abc565a57494963755d`, © gridoe, GPL-3.0-or-later) and **Zuno**
(<https://github.com/noFAYZ/zuno> at `1fa9a9669e4a526ebe53c0275153feda0c4fcefa`,
Apache-2.0). No file was copied; the rules below were re-implemented in Rust.

| music-gpui file | Upstream file(s) | What was adapted |
|---|---|---|
| `src/components/sidebar_playlists.rs` | refryt `crates/refryt/src/ui/sidebar.rs` (`playlists`, `rail_list`, `rail_row_compact`: the library's playlists under the nav with a "+" for a new one, covers only with a tooltip in the rail, a drag to reorder, the playlist page's menu on right-click); Zuno `src/ui/components/Sidebar.tsx` (the library rail in collapsed / expanded / hover modes) | GPUI rows of one fixed height in the rail and the full sidebar; the library database as the source (YouTube mirrors while signed in, local playlists, Liked songs); pins; "Show all" with a cap instead of a nested scroll area. |
| `src/settings/sidebar_prefs.rs` (`arrange`, `SidebarPlaylists::drag`) | Zuno `src/ui/components/sidebarLibrary.ts` (`sortLibraryEntries`: Recent / Name / Custom, Liked pinned first in the computed orders but not in the custom one, stable ties, numeric and accent-insensitive names; `canReorderLibrary`) | A drag switches to the custom order (instead of being refused in the others); pins of our own come before every order. |
| `src/glass.rs` (`NativeMaterial::BlurRadius`) | refryt `crates/refryt/src/blur.rs` (macOS: `CGSSetWindowBackgroundBlurRadius` on the window number, radius 30; Windows: `DWMSBT_TRANSIENTWINDOW`) | Light / Medium / Strong radii (8 / 24 / 60) next to GPUI's own blur view; Windows 11's acrylic as a background of its own after GPUI's Mica. |
| `src/glass.rs` (`resolve`) | refryt `crates/refryt/src/theme.rs` (`Palette::with_opacity`: surfaces solved so each composites to its own level over the window) | Per-surface opacities painted straight over the desktop, each raised by our readable floor, with a tint. |
