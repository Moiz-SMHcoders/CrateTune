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

## Zuno and Limusic

The queue sections and tabs, multi-select, the output-device filter, the log viewer and
the Music Together suggestion approval follow ideas from Zuno (Apache-2.0) and Limusic
(GPL-3.0); the ported pieces are listed in
[`../music-core/THIRD_PARTY.md`](../music-core/THIRD_PARTY.md#zuno-and-limusic-featqueue-ux).
