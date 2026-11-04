//! Which elements each Now Playing design and each mini player layout shows
//! (`now_playing.elements.DESIGN` and `now_playing.mini_elements.LAYOUT` in
//! `settings.json`): a switch per button, line or tag the view can show.
//!
//! Every view offers only what makes sense for it ([`ElementView::offered`]) and has
//! its own defaults ([`ElementView::defaults`]): what it showed before these
//! switches existed, so an untouched view looks exactly as it did. The file keeps
//! only the changes from those defaults (`{"turntable": {"like": true}}`), so a view
//! nobody customized follows its defaults, and missing or unknown entries load as
//! defaults.
//!
//! Where a newly shown element goes is each design's own rule (`np_elements_ui.rs`):
//! buttons that have a spot in the design use it (the heart beside the title, shuffle
//! and repeat beside the transport); the others line up in one row of round buttons
//! (`extras`) at a fixed place per design, which hands what doesn't fit its width to
//! a "More" menu, so nothing overlaps or leaves the screen. Header items (close, the
//! source tag, sleep timer, sound, Cast, lyrics and queue) always sit in the header.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::prefs::{MiniPlayerSkin, NowPlayingConfig, NowPlayingLayout};

/// One thing a Now Playing design or the mini player can show or hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NpElement {
    /// The artist (and album) line under the title.
    Artist,
    /// "Playing from …" (album, playlist, "Your queue").
    Source,
    /// The source / quality tag ("YouTube · AAC 128 kbps").
    Quality,
    Like,
    Dislike,
    Shuffle,
    Repeat,
    /// The previous and next buttons (play / pause always stays).
    Skip,
    SeekBar,
    /// Elapsed / remaining time.
    Times,
    Volume,
    /// The line being sung, as plain text.
    LyricLine,
    /// "Up next": the next song (the mini player's button and list).
    UpNext,
    AddToPlaylist,
    Download,
    /// Header: Now Playing with the lyrics (designs without a lyrics pane).
    Lyrics,
    /// Header: Now Playing with the queue (the With queue design until it closes).
    Queue,
    SleepTimer,
    /// The sound menu (effects, EQ).
    Sound,
    /// Cast to a device, and the device hand-off button once a device is paired.
    Cast,
    /// "Visualizer in Now Playing" when it's on (Settings → Player bar).
    Visualizer,
    /// Header: the close button (Esc still closes); mini player: close / back to
    /// the main window.
    Close,
}

impl NpElement {
    /// Settings order.
    pub const ALL: [Self; 22] = [
        Self::Artist,
        Self::Source,
        Self::Quality,
        Self::Like,
        Self::Dislike,
        Self::Shuffle,
        Self::Repeat,
        Self::Skip,
        Self::SeekBar,
        Self::Times,
        Self::Volume,
        Self::LyricLine,
        Self::UpNext,
        Self::AddToPlaylist,
        Self::Download,
        Self::Lyrics,
        Self::Queue,
        Self::SleepTimer,
        Self::Sound,
        Self::Cast,
        Self::Visualizer,
        Self::Close,
    ];

    const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// Stable, untranslated name: the field in `settings.json` and the scripts'
    /// `@np-elements=DESIGN:KEY=on|off`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Artist => "artist_album",
            Self::Source => "source_line",
            Self::Quality => "quality_tag",
            Self::Like => "like",
            Self::Dislike => "dislike",
            Self::Shuffle => "shuffle",
            Self::Repeat => "repeat",
            Self::Skip => "previous_next",
            Self::SeekBar => "seek_bar",
            Self::Times => "times",
            Self::Volume => "volume",
            Self::LyricLine => "lyric_line",
            Self::UpNext => "up_next",
            Self::AddToPlaylist => "add_to_playlist",
            Self::Download => "download",
            Self::Lyrics => "lyrics",
            Self::Queue => "queue",
            Self::SleepTimer => "sleep_timer",
            Self::Sound => "sound",
            Self::Cast => "cast",
            Self::Visualizer => "visualizer",
            Self::Close => "close",
        }
    }

    /// Accepts the key with `_` or `-`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().replace('-', "_");
        Self::ALL.into_iter().find(|e| e.key() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Artist => t!("np_elements.item.artist_album"),
            Self::Source => t!("np_elements.item.source_line"),
            Self::Quality => t!("np_elements.item.quality_tag"),
            Self::Like => t!("np_elements.item.like"),
            Self::Dislike => t!("np_elements.item.dislike"),
            Self::Shuffle => t!("np_elements.item.shuffle"),
            Self::Repeat => t!("np_elements.item.repeat"),
            Self::Skip => t!("np_elements.item.previous_next"),
            Self::SeekBar => t!("np_elements.item.seek_bar"),
            Self::Times => t!("np_elements.item.times"),
            Self::Volume => t!("np_elements.item.volume"),
            Self::LyricLine => t!("np_elements.item.lyric_line"),
            Self::UpNext => t!("np_elements.item.up_next"),
            Self::AddToPlaylist => t!("np_elements.item.add_to_playlist"),
            Self::Download => t!("np_elements.item.download"),
            Self::Lyrics => t!("np_elements.item.lyrics"),
            Self::Queue => t!("np_elements.item.queue"),
            Self::SleepTimer => t!("np_elements.item.sleep_timer"),
            Self::Sound => t!("np_elements.item.sound"),
            Self::Cast => t!("np_elements.item.cast"),
            Self::Visualizer => t!("np_elements.item.visualizer"),
            Self::Close => t!("np_elements.item.close"),
        }
    }
}

/// A set of [`NpElement`]s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Elements(u32);

impl Elements {
    pub const NONE: Self = Self(0);

    pub const fn of(list: &[NpElement]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < list.len() {
            bits |= list[i].bit();
            i += 1;
        }
        Self(bits)
    }

    pub const fn has(self, e: NpElement) -> bool {
        self.0 & e.bit() != 0
    }

    pub fn with(self, e: NpElement, on: bool) -> Self {
        if on {
            Self(self.0 | e.bit())
        } else {
            Self(self.0 & !e.bit())
        }
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub const fn and(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    /// In settings order.
    pub fn iter(self) -> impl Iterator<Item = NpElement> {
        NpElement::ALL.into_iter().filter(move |e| self.has(*e))
    }

    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// The keys, comma-separated (status lines, tests).
    pub fn keys(self) -> String {
        self.iter()
            .map(NpElement::key)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// A Now Playing design or a mini player layout: what has its own elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementView {
    /// The split design covers its stacked arrangement too.
    Np(NowPlayingLayout),
    Mini(MiniPlayerSkin),
}

use NpElement as E;

/// In the header of every design, on by default.
const HEADER: Elements = Elements::of(&[E::Close, E::Quality, E::SleepTimer, E::Sound]);
/// Header extras every design offers (off by default).
const HEADER_EXTRA: Elements = Elements::of(&[E::Cast]);
/// Buttons every design can add (off by default unless it had them).
const ACTIONS: Elements = Elements::of(&[
    E::Like,
    E::Dislike,
    E::Shuffle,
    E::Repeat,
    E::AddToPlaylist,
    E::Download,
    E::Volume,
]);

impl ElementView {
    pub fn all() -> impl Iterator<Item = Self> {
        NowPlayingLayout::ALL
            .into_iter()
            .map(Self::Np)
            .chain(MiniPlayerSkin::ALL.into_iter().map(Self::Mini))
    }

    pub fn title(self) -> String {
        match self {
            Self::Np(layout) => layout.title().to_owned(),
            Self::Mini(skin) => t!("np_elements.mini_view", layout = skin.title()),
        }
    }

    /// What the view showed before these switches existed (an untouched view looks
    /// exactly as it did).
    pub fn defaults(self) -> Elements {
        use NowPlayingLayout as L;
        let np = |own: &[NpElement]| {
            HEADER
                .union(Elements::of(&[E::Visualizer]))
                .union(Elements::of(own))
        };
        match self {
            Self::Np(L::Split) => np(&[
                E::Artist,
                E::Dislike,
                E::Like,
                E::SeekBar,
                E::Times,
                E::Skip,
                E::UpNext,
            ]),
            Self::Np(L::Centered) => np(&[E::Artist, E::SeekBar, E::Times, E::Skip, E::LyricLine]),
            Self::Np(L::Lyrics) => np(&[E::Artist, E::SeekBar, E::Times, E::Skip]),
            Self::Np(L::Immersive) => np(&[
                E::Artist,
                E::Like,
                E::LyricLine,
                E::SeekBar,
                E::Times,
                E::Shuffle,
                E::Skip,
                E::Repeat,
            ]),
            Self::Np(L::Card) => np(&[
                E::Source,
                E::Artist,
                E::Shuffle,
                E::Like,
                E::SeekBar,
                E::Times,
                E::Skip,
                E::Volume,
            ]),
            // The window filling with colour is its seek bar.
            Self::Np(L::Minimal) => np(&[E::Artist, E::SeekBar, E::Times, E::Skip, E::Like]),
            Self::Np(L::Editorial) => np(&[
                E::Source,
                E::Artist,
                E::Skip,
                E::SeekBar,
                E::Times,
                E::Like,
                E::Shuffle,
                E::Repeat,
            ]),
            Self::Np(L::Queue) => np(&[E::Artist, E::Like, E::SeekBar, E::Times, E::Skip]),
            Self::Np(L::Turntable) => np(&[E::Artist, E::LyricLine, E::SeekBar, E::Times, E::Skip]),
            // The compact strip has no times (its progress is a hairline).
            Self::Mini(MiniPlayerSkin::Compact) => Elements::of(&[
                E::Artist,
                E::SeekBar,
                E::Skip,
                E::Shuffle,
                E::Repeat,
                E::Like,
                E::Volume,
                E::UpNext,
                E::Close,
            ]),
            Self::Mini(_) => Elements::of(&[
                E::Artist,
                E::SeekBar,
                E::Times,
                E::Skip,
                E::Shuffle,
                E::Repeat,
                E::Like,
                E::Volume,
                E::UpNext,
                E::Close,
            ]),
        }
    }

    /// What the view can show: its defaults plus what makes sense to add.
    pub fn offered(self) -> Elements {
        use NowPlayingLayout as L;
        let extra = match self {
            // Lyrics are its right half; the column has "Up next" already.
            Self::Np(L::Split) => ACTIONS.union(Elements::of(&[E::Source, E::Queue])),
            Self::Np(L::Centered | L::Immersive | L::Card | L::Editorial | L::Turntable) => {
                ACTIONS.union(Elements::of(&[E::Source, E::UpNext, E::Lyrics, E::Queue]))
            }
            // The strip has no room for lines of text.
            Self::Np(L::Lyrics) => ACTIONS.union(Elements::of(&[E::Queue])),
            Self::Np(L::Minimal) => ACTIONS.union(Elements::of(&[E::Source, E::Lyrics, E::Queue])),
            // Its right half is the queue.
            Self::Np(L::Queue) => ACTIONS.union(Elements::of(&[E::Source, E::Lyrics])),
            Self::Mini(_) => Elements::of(&[E::Dislike, E::AddToPlaylist, E::Download]),
        };
        let header = match self {
            Self::Np(_) => HEADER_EXTRA,
            Self::Mini(_) => Elements::NONE,
        };
        self.defaults().union(extra).union(header)
    }
}

/// A view's changes from its defaults: elements turned on and turned off.
/// Serialized as `{"KEY": true|false}`; unknown keys and non-booleans are dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Overrides {
    on: Elements,
    off: Elements,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.on.is_empty() && self.off.is_empty()
    }

    fn apply(self, view: ElementView) -> Elements {
        view.defaults()
            .union(self.on)
            .minus(self.off)
            .and(view.offered())
    }

    fn set(&mut self, view: ElementView, e: NpElement, on: bool) {
        self.on = self.on.with(e, false);
        self.off = self.off.with(e, false);
        if !view.offered().has(e) || view.defaults().has(e) == on {
            return;
        }
        if on {
            self.on = self.on.with(e, true);
        } else {
            self.off = self.off.with(e, true);
        }
    }
}

impl Serialize for Overrides {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = s.serialize_map(Some(self.on.len() + self.off.len()))?;
        for e in NpElement::ALL {
            if self.on.has(e) {
                map.serialize_entry(e.key(), &true)?;
            } else if self.off.has(e) {
                map.serialize_entry(e.key(), &false)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Overrides {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let mut out = Self::default();
        if let Some(map) = value.as_object() {
            for (key, value) in map {
                let (Some(e), Some(on)) = (NpElement::parse(key), value.as_bool()) else {
                    continue;
                };
                if on {
                    out.on = out.on.with(e, true);
                } else {
                    out.off = out.off.with(e, true);
                }
            }
        }
        Ok(out)
    }
}

/// `now_playing.elements`: per design.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NpElementsConfig {
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub split: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub centered: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub lyrics: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub immersive: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub card: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub minimal: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub editorial: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub queue: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub turntable: Overrides,
}

/// `now_playing.mini_elements`: per mini player layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MiniElementsConfig {
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub bar: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub compact: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub card: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub lyrics: Overrides,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    pub turntable: Overrides,
}

impl NowPlayingConfig {
    fn overrides(&mut self, view: ElementView) -> &mut Overrides {
        use NowPlayingLayout as L;
        let (np, mini) = (&mut self.elements, &mut self.mini_elements);
        match view {
            ElementView::Np(L::Split) => &mut np.split,
            ElementView::Np(L::Centered) => &mut np.centered,
            ElementView::Np(L::Lyrics) => &mut np.lyrics,
            ElementView::Np(L::Immersive) => &mut np.immersive,
            ElementView::Np(L::Card) => &mut np.card,
            ElementView::Np(L::Minimal) => &mut np.minimal,
            ElementView::Np(L::Editorial) => &mut np.editorial,
            ElementView::Np(L::Queue) => &mut np.queue,
            ElementView::Np(L::Turntable) => &mut np.turntable,
            ElementView::Mini(MiniPlayerSkin::Bar) => &mut mini.bar,
            ElementView::Mini(MiniPlayerSkin::Compact) => &mut mini.compact,
            ElementView::Mini(MiniPlayerSkin::Card) => &mut mini.card,
            ElementView::Mini(MiniPlayerSkin::Lyrics) => &mut mini.lyrics,
            ElementView::Mini(MiniPlayerSkin::Turntable) => &mut mini.turntable,
        }
    }

    /// What `view` shows.
    pub fn shown(&self, view: ElementView) -> Elements {
        let mut copy = *self;
        copy.overrides(view).apply(view)
    }

    pub fn set_element(&mut self, view: ElementView, e: NpElement, on: bool) {
        self.overrides(view).set(view, e, on);
    }

    pub fn set_elements(&mut self, view: ElementView, shown: Elements) {
        for e in view.offered().iter() {
            self.set_element(view, e, shown.has(e));
        }
    }

    pub fn reset_elements(&mut self, view: ElementView) {
        *self.overrides(view) = Overrides::default();
    }

    /// `view` shows its defaults.
    pub fn elements_default(&self, view: ElementView) -> bool {
        let mut copy = *self;
        copy.overrides(view).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use NowPlayingLayout as L;

    fn set(list: &[NpElement]) -> Elements {
        Elements::of(list)
    }

    /// What each design drew before the switches, read off its render code
    /// (`now_playing.rs`, `np_designs.rs`, `np_extras.rs`, `mini_player.rs` at
    /// 41f669f): the defaults must be exactly this.
    fn todays_look(view: ElementView) -> Elements {
        // Every design's header: close, the source tag, sleep timer, sound menu;
        // the visualizer when "Visualizer in Now Playing" is on.
        let header = set(&[E::Close, E::Quality, E::SleepTimer, E::Sound, E::Visualizer]);
        let own = match view {
            // `render_player_column` / `render_stacked_top`: title, artists,
            // dislike + like, `progress_view` (bar + times), `transport_view`,
            // `up_next_view`.
            ElementView::Np(L::Split) => set(&[
                E::Artist,
                E::Dislike,
                E::Like,
                E::SeekBar,
                E::Times,
                E::Skip,
                E::UpNext,
            ]),
            // `render_centered`: artists, progress, transport, the sung line.
            ElementView::Np(L::Centered) => {
                set(&[E::Artist, E::SeekBar, E::Times, E::Skip, E::LyricLine])
            }
            // `render_strip`: artists, progress, transport.
            ElementView::Np(L::Lyrics) => set(&[E::Artist, E::SeekBar, E::Times, E::Skip]),
            // `render_immersive`: the source label (= the tag), artists, heart, the
            // sung line, progress + times, `icon_transport` (shuffle, skip, repeat).
            ElementView::Np(L::Immersive) => set(&[
                E::Artist,
                E::Like,
                E::LyricLine,
                E::SeekBar,
                E::Times,
                E::Shuffle,
                E::Skip,
                E::Repeat,
            ]),
            // `render_card`: "Playing from", artists, shuffle + heart, progress,
            // format between the times (= the tag), transport, volume.
            ElementView::Np(L::Card) => set(&[
                E::Source,
                E::Artist,
                E::Shuffle,
                E::Like,
                E::SeekBar,
                E::Times,
                E::Skip,
                E::Volume,
            ]),
            // `render_minimal`: artists, the fill (`design_backdrop`), the big time,
            // previous / next, heart.
            ElementView::Np(L::Minimal) => {
                set(&[E::Artist, E::SeekBar, E::Times, E::Skip, E::Like])
            }
            // `render_editorial`: the album header, artists, next / previous,
            // progress + times, heart, shuffle, repeat.
            ElementView::Np(L::Editorial) => set(&[
                E::Source,
                E::Artist,
                E::Skip,
                E::SeekBar,
                E::Times,
                E::Like,
                E::Shuffle,
                E::Repeat,
            ]),
            // `render_with_queue`: artists, heart, progress + times, transport.
            ElementView::Np(L::Queue) => set(&[E::Artist, E::Like, E::SeekBar, E::Times, E::Skip]),
            // `render_turntable`: artists, the sung line, progress, times, transport.
            ElementView::Np(L::Turntable) => {
                set(&[E::Artist, E::LyricLine, E::SeekBar, E::Times, E::Skip])
            }
            // The mini player: title strip ("Up next", pin, back to the main window /
            // close), artists, progress with times (a hairline in compact),
            // previous (not compact) / next, the tools row (shuffle, repeat, like,
            // volume).
            ElementView::Mini(skin) => {
                let base = set(&[
                    E::Artist,
                    E::SeekBar,
                    E::Skip,
                    E::Shuffle,
                    E::Repeat,
                    E::Like,
                    E::Volume,
                    E::UpNext,
                    E::Close,
                ]);
                return if skin == MiniPlayerSkin::Compact {
                    base
                } else {
                    base.with(E::Times, true)
                };
            }
        };
        header.union(own)
    }

    #[test]
    fn defaults_are_todays_look() {
        for view in ElementView::all() {
            assert_eq!(
                view.defaults(),
                todays_look(view),
                "{view:?}: {} vs {}",
                view.defaults().keys(),
                todays_look(view).keys()
            );
            let np = NowPlayingConfig::default();
            assert_eq!(np.shown(view), view.defaults(), "{view:?}");
            assert!(np.elements_default(view));
            // Everything shown by default is offered (can be switched off).
            assert_eq!(view.offered().and(view.defaults()), view.defaults());
        }
        // The example that started it: a heart can be added to the turntable.
        let turntable = ElementView::Np(L::Turntable);
        assert!(!turntable.defaults().has(E::Like) && turntable.offered().has(E::Like));
    }

    #[test]
    fn only_sensible_elements_are_offered() {
        for view in ElementView::all() {
            let offered = view.offered();
            match view {
                ElementView::Np(layout) => {
                    // A lyrics toggle only where the design has no lyrics pane; a
                    // queue toggle everywhere but the queue design.
                    assert_eq!(
                        offered.has(E::Lyrics),
                        !matches!(layout, L::Split | L::Lyrics),
                        "{layout:?}"
                    );
                    assert_eq!(offered.has(E::Queue), layout != L::Queue);
                    assert!(offered.has(E::Cast) && offered.has(E::Visualizer));
                    // The sung line only where the design has a spot for it.
                    assert_eq!(
                        offered.has(E::LyricLine),
                        matches!(layout, L::Centered | L::Immersive | L::Turntable)
                    );
                    assert!(!offered.has(E::UpNext) || !matches!(layout, L::Queue | L::Lyrics));
                }
                ElementView::Mini(skin) => {
                    for e in [E::Lyrics, E::Queue, E::Cast, E::Sound, E::Visualizer] {
                        assert!(!offered.has(e), "{skin:?} {e:?}");
                    }
                    assert_eq!(offered.has(E::Times), skin != MiniPlayerSkin::Compact);
                }
            }
        }
    }

    #[test]
    fn keys_parse_and_overrides_round_trip() {
        for e in NpElement::ALL {
            assert_eq!(NpElement::parse(e.key()), Some(e));
            assert_eq!(NpElement::parse(&e.key().replace('_', "-")), Some(e));
        }
        assert_eq!(NpElement::parse("laser"), None);
        let turntable = ElementView::Np(L::Turntable);
        let mut np = NowPlayingConfig::default();
        np.set_element(turntable, E::Like, true);
        np.set_element(turntable, E::Times, false);
        // Not offered: ignored.
        np.set_element(turntable, E::Close, true);
        np.set_element(ElementView::Mini(MiniPlayerSkin::Compact), E::Times, true);
        let shown = np.shown(turntable);
        assert!(shown.has(E::Like) && !shown.has(E::Times) && shown.has(E::SeekBar));
        assert!(
            !np.shown(ElementView::Mini(MiniPlayerSkin::Compact))
                .has(E::Times)
        );
        // Only the changes are saved.
        let json = serde_json::to_value(np).unwrap();
        assert_eq!(
            json["elements"],
            serde_json::json!({"turntable": {"like": true, "times": false}})
        );
        assert_eq!(json["mini_elements"], serde_json::json!({}));
        let back: NowPlayingConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, np);
        // Setting an element back to its default drops the change.
        np.set_element(turntable, E::Like, false);
        np.set_element(turntable, E::Times, true);
        assert!(np.elements_default(turntable));
        assert_eq!(np, NowPlayingConfig::default());
        // Reset.
        np.set_element(ElementView::Mini(MiniPlayerSkin::Card), E::Like, false);
        assert!(!np.elements_default(ElementView::Mini(MiniPlayerSkin::Card)));
        np.reset_elements(ElementView::Mini(MiniPlayerSkin::Card));
        assert_eq!(np, NowPlayingConfig::default());
    }

    #[test]
    fn older_and_odd_files_load_with_defaults() {
        // Before the switches: no `elements`.
        let old: NowPlayingConfig = serde_json::from_str(r#"{"layout":"card"}"#).unwrap();
        for view in ElementView::all() {
            assert_eq!(old.shown(view), view.defaults());
        }
        // Unknown designs, keys and values are dropped; the rest is kept.
        let odd: NowPlayingConfig = serde_json::from_str(
            r#"{"elements": {"vinyl": {"like": true}, "card": {"laser": true, "like": "yes",
                "volume": false}, "split": 3}, "mini_elements": {"bar": {"download": true}}}"#,
        )
        .unwrap();
        let card = odd.shown(ElementView::Np(L::Card));
        assert!(!card.has(E::Volume) && card.has(E::Like));
        assert_eq!(
            odd.shown(ElementView::Np(L::Split)),
            ElementView::Np(L::Split).defaults()
        );
        assert!(
            odd.shown(ElementView::Mini(MiniPlayerSkin::Bar))
                .has(E::Download)
        );
        // Part of profiles and the settings export (the `now_playing` section).
        let mut np = NowPlayingConfig::default();
        np.set_element(ElementView::Np(L::Turntable), E::Like, true);
        let settings = crate::settings::Settings {
            now_playing: np,
            ..Default::default()
        };
        let export = crate::settings::portable::export_settings(&settings, 0);
        let doc: serde_json::Value = serde_json::from_str(&export).unwrap();
        assert_eq!(
            doc["settings"]["now_playing"]["elements"]["turntable"]["like"],
            true
        );
        let profile = crate::settings::profiles::snapshot(&settings);
        assert_eq!(
            profile["now_playing"]["elements"]["turntable"]["like"],
            true
        );
    }
}
