//! Script actions for the player extras round (see `script.rs` for the
//! runner): `@np-open-on-play=on|off`, `@np-click-empty=on|off`,
//! `@np-portrait=auto|always|never`, `@lyrics-glow=on|off`, `@mini-queue=on|off`,
//! `@turntable-spin=off|grooves|record` (`on` = record),
//! `@mini-pin=on|off` (saved), `@seek-by=SECS` (negative: back), `@lyrics-line=up|down|play`
//! (the arrow keys / Enter over Now Playing's lyrics), `@bar-compact=on|off`
//! (collapse / expand the narrow bar), `@play-episode=ID[:TITLE]` (plays an id as a
//! podcast episode), `@extras-status` (prints the state:
//! the autoplay marks in the queue, the lyrics' keyboard focus and hold, the
//! bar's layout, the mini player's options).

use gpui::{Context, Window};

use crate::app::MusicApp;
use crate::settings::prefs::PortraitLayout;

impl MusicApp {
    /// Returns `false` if `name` isn't one of these actions.
    pub(crate) fn extras_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let on = arg != Some("off");
        let panel = self.settings.clone();
        match name {
            "np-open-on-play" => panel.update(cx, |p, cx| p.set_np_open_on_play(on, cx)),
            "np-click-empty" => panel.update(cx, |p, cx| p.set_np_click_empty(on, cx)),
            "np-portrait" => match arg.and_then(PortraitLayout::parse) {
                Some(p) => panel.update(cx, |panel, cx| panel.set_np_portrait(p, cx)),
                None => eprintln!("music-gpui: @np-portrait wants auto|always|never"),
            },
            "lyrics-glow" => panel.update(cx, |p, cx| p.set_line_glow(on, cx)),
            "turntable-spin" => match arg.map_or(
                Some(crate::turntable::SpinLook::Record),
                crate::turntable::SpinLook::parse,
            ) {
                Some(look) => panel.update(cx, |p, cx| p.set_turntable_spin(look, cx)),
                None => eprintln!("music-gpui: @turntable-spin wants off|grooves|record|on"),
            },
            "mini-queue" => panel.update(cx, |p, cx| p.set_mini_queue(on, cx)),
            "mini-pin" => panel.update(cx, |p, cx| p.set_mini_on_top(on, cx)),
            "seek-by" => match arg.and_then(|a| a.parse::<f32>().ok()) {
                Some(seconds) => self.seek_by(seconds, cx),
                None => eprintln!("music-gpui: @seek-by wants seconds (-15, 15)"),
            },
            "lyrics-line" => match arg {
                Some("up") => self.lyrics_line_step(-1, cx),
                Some("down") => self.lyrics_line_step(1, cx),
                Some("play") => self.lyrics_line_play(cx),
                _ => eprintln!("music-gpui: @lyrics-line wants up|down|play"),
            },
            "bar-compact" => self.set_bar_expanded(!on, cx),
            // Plays an id as a podcast episode (the bar's skips and speed).
            "play-episode" => {
                let (id, title) = arg
                    .unwrap_or("")
                    .split_once(':')
                    .unwrap_or((arg.unwrap_or(""), ""));
                let track = music_core::Track {
                    video_id: id.to_owned(),
                    title: if title.is_empty() { id } else { title }.to_owned(),
                    artists: "Podcast".to_owned(), // i18n-ignore
                    music_video_type: Some(music_core::MusicVideoType::PodcastEpisode),
                    ..music_core::Track::default()
                };
                self.play_list(std::rc::Rc::new(vec![track]), 0, cx);
            }
            "extras-status" => self.print_extras_status(window, cx),
            _ => return false,
        }
        true
    }

    fn print_extras_status(&self, window: &Window, cx: &Context<Self>) {
        let np = self.settings.read(cx).settings().now_playing;
        let marks = self
            .queue
            .upcoming()
            .iter()
            .filter(|i| self.added_by_autoplay(i.uid))
            .count();
        let list = &self.np.list;
        let layout = crate::components::player_bar::bar_layout_with(
            f32::from(window.viewport_size().width),
            self.cast_device().is_some(),
            self.playback.sleep_timer.is_some(),
            true,
            self.settings.read(cx).bar_items(),
            crate::components::bar_compact::options(self.playing_episode()),
        );
        eprintln!(
            "music-gpui: extras: np open {} (open on play {}, click empty {}, portrait {}), \
             lyrics key line {:?}, manual hold {}, episode {}, autoplay marks {}/{}, \
             bar compact {} more {} skips {} speed {}, mini queue {} on top {} open {}, \
             spin {} (Now Playing's record turning {})",
            self.np.open,
            np.open_on_play,
            np.click_empty_closes,
            np.portrait.key(),
            list.key_line,
            list.manual_until
                .is_some_and(|t| t > std::time::Instant::now()),
            self.playing_episode(),
            marks,
            self.queue.upcoming().len(),
            layout.compact,
            layout.more,
            layout.skips,
            layout.speed,
            np.mini_queue,
            np.mini_on_top,
            self.mini.is_some(),
            np.turntable_spin.key(),
            self.np.spin.turning(),
        );
    }
}
