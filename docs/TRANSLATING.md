# Translating CrateTune

CrateTune's interface is in 18 languages: en, de, es, fr, hi, id, it, ja, ko, nl, pl, pt-BR,
ru, tr, uk, vi, zh-CN and zh-TW. English is the source; every other language must cover every
English key (a test enforces it, see [Checking](#checking)).

## Where the strings are

`crates/music-gpui/locales/<language>/<area>.lang`: one folder per language, the same 30 file
names in each (`update.lang`, `library.lang`, `now_playing.lang`, …). The English files'
comments explain each string (where it appears, what a placeholder holds); they are the place
to look when a string is unclear.

## The `.lang` format

```text
# A comment: where the string appears, what {name} is.
update.managed = Updates come from {manager}.
library.songs.one = {count} song
library.songs.other = {count} songs
```

* One `key = value` per line. Keep the key exactly as in English; translate the value only.
* `{name}` is a placeholder filled in by the app. Keep every placeholder of the English value,
  spelled the same, anywhere in the sentence. `{app}` is the app's name. `{{` and `}}` are
  literal braces.
* **Plurals:** a key ending in `.zero`, `.one`, `.two`, `.few`, `.many` or `.other` is one
  form of a plural group. English has `one` and `other`; give the forms your language uses
  (CLDR rules: Russian and Ukrainian need `one`, `few`, `many`, `other`; Japanese only
  `other`). A missing form falls back to `other`.
* Escapes: `\n` line break, `\t` tab, `\s` a space at the start or end, `\\` a backslash.
* Values are trimmed. An empty value means "not translated yet": the English text shows.
* Files are UTF-8.

Don't translate brand and service names (YouTube Music, Last.fm, Discord, Flatpak, Homebrew),
and leave the app's name to `{app}`.

## Adding a language

1. Copy `crates/music-gpui/locales/en` to `crates/music-gpui/locales/<code>` (a BCP 47 code
   such as `sv` or `pt-PT`) and translate the values.
2. The build picks up the new folder. Add its name (`language.name.<code>`) to every
   language's `languages.lang`, and check that `crates/music-gpui/src/i18n/plural.rs` (plural
   rules) and `src/i18n/format.rs` (digit grouping) know the language; many already do.
3. Run the checks below and look at the app in the language
   (`MUSIC_LANG=<code> cargo run --release -p music-gpui`).

Right-to-left languages are not supported yet (see the README, "Translations").

## Checking

```sh
cargo test -p music-gpui i18n                      # keys, placeholders, plural forms
cargo test -p music-gpui coverage -- --nocapture   # per-language coverage table
```

`every_locale_matches_english` fails on a key English doesn't have, a placeholder that differs
from English, or a missing plural form; `lookups_fall_back_and_fill` checks every key resolves.
A pull request must keep every language at 100 %: when you add an English string, add it to all
18 languages (a careful first translation is better than none; native speakers can improve it).

## Weblate

There is no Weblate project yet. The `.lang` files are a subset of **Java Properties (UTF-8)**,
so Weblate can host them without conversion: `key = value` lines, `#` comments, the same `\n` and
`\t` escapes, and keys without `:` or `=`. (`\s` is the one escape Properties doesn't share; no
catalog uses it, so avoid it in strings meant for Weblate.)

To set it up, a maintainer would (on [Hosted Weblate](https://hosted.weblate.org/hosting/),
free for libre projects):

1. Create the project and one component with these settings, then add the **Component
   discovery** add-on so every `.lang` file becomes a component:

   | Setting | Value |
   |---|---|
   | Repository | `https://github.com/Moiz-SMHcoders/CrateTune.git`, branch `main` |
   | File format | Java Properties (UTF-8) |
   | Monolingual base language file | `crates/music-gpui/locales/en/update.lang` |
   | File mask | `crates/music-gpui/locales/*/update.lang` |
   | Language code style | BCP style using hyphen as a separator (`pt-BR`, `zh-CN`) |
   | Source language | English |

   Component discovery:

   | Setting | Value |
   |---|---|
   | Regular expression | `crates/music-gpui/locales/(?P<language>[^/]+)/(?P<component>[^/]+)\.lang` |
   | Component name | `{{ component }}` |
   | Monolingual base language file | `crates/music-gpui/locales/en/{{ component }}.lang` |
   | Define the new base file | (empty) |
   | File format | Java Properties (UTF-8) |

2. Give Weblate push access the way it suggests (its GitHub app or a deploy key) so it opens
   pull requests, and add the GitHub webhook so it sees new English strings.
3. Turn on the **Cleanup translation files** and **Squash Git commits** add-ons, and the
   "same placeholders" (`{name}` / Python brace format) check.
4. Link the project here and in the README.

Weblate writes `key=value` without the spaces around `=`; the app reads both.
