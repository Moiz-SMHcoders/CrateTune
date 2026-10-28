//! Script actions (`MUSIC_SCRIPT` / `MUSIC_GPUI_SCREENSHOT`) for the integrations;
//! `script.rs` falls back to these.
//!
//! | action | does |
//! |---|---|
//! | `@sb=on\|off`, `@sb-mode=auto\|ask`, `@sb-category=CAT:on\|off` | SponsorBlock settings (`music_offtopic`, `sponsor`, …) |
//! | `@sb-skip` | accept the offered skip ("ask" mode) |
//! | `@sb-status` | print the current song's segments |
//! | `@play-id=VIDEO_ID[:TITLE]` | play one YouTube video id (e.g. a music video with segments) |
//! | `@set=FIELD:VALUE` | type into `sb-url`, `together-name`, `together-port`, `together-relay`, `spotify-client`, `spotify-port`, `ai-endpoint`, `ai-model`, `ai-custom`, `ai-key` |
//! | `@together-host[=FILE]` | start a session (and write the join link to FILE) |
//! | `@together-host-relay[=FILE]` | start a session through the relay from the settings |
//! | `@together-transfer=NAME` | hand the session to a guest |
//! | `@together-join=LINK` / `@together-join=file:PATH` | join a session |
//! | `@together-leave` | leave / end the session |
//! | `@together-approve`, `@together-deny` | decide on every pending guest |
//! | `@together-kick=NAME`, `@together-ban=NAME` | remove a guest |
//! | `@together-allow=add\|control\|approval:on\|off` | room settings |
//! | `@together-suggest=N` | as a guest: suggest search result N (play next) |
//! | `@together-control=play\|pause\|next\|prev\|seek:SECS` | as a guest: ask the host |
//! | `@together-status` | print role, participants, room and clock |
//! | `@import-file=PATH`, `@spotify-playlist=URL`, `@spotify-liked` | start an import |
//! | `@import-accept=N`, `@import-save[=NAME]`, `@import-cancel`, `@import-status` | the import job |
//! | `@spotify-playlists`, `@spotify-connect` | Spotify account |
//! | `@export=PLAYLIST:FORMAT:PATH` | export a library playlist (`csv`, `m3u`, `json`) |
//! | `@ai-filter=on\|off`, `@ai-category=KEY:on\|off` | AI content filter (`explicit`, `holiday`, …) |
//! | `@ai-filter-status`, `@ai-filter-clear` | print / forget its verdicts |
//! | `@cast-scan`, `@cast=NAME\|IP:PORT[:NAME]`, `@cast-stop[=here\|off]`, `@cast-status` | Chromecast |

use std::path::PathBuf;
use std::time::Instant;

use gpui::{Context, Window};
use music_core::ai_filter::AiCategory;
use music_core::library::PlaylistSort;
use music_core::playlist_import::{Format, Resolution};
use music_core::sponsorblock::Category;
use music_core::together::{AddTrackMode, ControlAction};

use super::config::SkipMode;
use super::import::{ImportSource, Phase};
use super::together::Role;
use crate::app::MusicApp;

impl MusicApp {
    /// Returns whether `name` was one of these actions.
    pub fn integrations_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let on = arg != Some("off");
        let panel = self.settings.clone();
        match name {
            "sb" => panel.update(cx, |p, cx| p.set_sponsorblock(|s| s.enabled = on, cx)),
            "sb-mode" => {
                let mode = if arg == Some("ask") {
                    SkipMode::Ask
                } else {
                    SkipMode::Auto
                };
                panel.update(cx, |p, cx| p.set_sponsorblock(|s| s.mode = mode, cx));
            }
            "sb-category" => {
                let (cat, state) = arg
                    .unwrap_or("")
                    .split_once(':')
                    .unwrap_or((arg.unwrap_or(""), "on"));
                match Category::from_api_value(cat) {
                    Some(c) => panel.update(cx, |p, cx| {
                        p.set_sponsorblock_category(c, state != "off", cx)
                    }),
                    None => eprintln!("music-gpui: unknown SponsorBlock category {cat:?}"),
                }
            }
            "sb-skip" => self.sponsorblock_skip_offer(cx),
            "play-id" => {
                let (id, title) = arg
                    .unwrap_or("")
                    .split_once(':')
                    .unwrap_or((arg.unwrap_or(""), ""));
                let track = music_core::Track {
                    video_id: id.to_owned(),
                    title: if title.is_empty() { id } else { title }.to_owned(),
                    ..music_core::Track::default()
                };
                self.play_list(std::rc::Rc::new(vec![track]), 0, cx);
            }
            "sb-status" => {
                let sb = &self.integrations.sponsorblock;
                eprintln!(
                    "music-gpui: sponsorblock loading={} error={:?} offer={:?} last_skip={:?} segments={:?}",
                    sb.loading,
                    sb.error,
                    sb.offer
                        .map(|s| (s.start.as_secs_f32(), s.end.as_secs_f32())),
                    sb.last_skip
                        .map(|(s, at)| (s.category, at.elapsed().as_secs_f32())),
                    sb.segments
                        .iter()
                        .map(|s| (s.category, s.start.as_secs_f32(), s.end.as_secs_f32()))
                        .collect::<Vec<_>>()
                );
            }
            "set" => {
                let (field, value) = arg.unwrap_or("").split_once(':').unwrap_or(("", ""));
                let ok = panel.update(cx, |p, cx| p.set_extras_field(field, value, window, cx));
                if !ok {
                    eprintln!("music-gpui: unknown field {field:?}");
                }
            }
            "together-host" => {
                let link = self.together_host(window, cx);
                eprintln!("music-gpui: together link {link:?}");
                if let (Some(link), Some(path)) = (link, arg)
                    && let Err(e) = std::fs::write(path, link)
                {
                    eprintln!("music-gpui: writing {path}: {e}");
                }
            }
            "together-host-relay" => {
                let path = arg.map(str::to_owned);
                self.together_host_relay(window, cx, move |link| {
                    eprintln!("music-gpui: together link {link:?}");
                    if let Some(path) = path
                        && let Err(e) = std::fs::write(&path, link)
                    {
                        eprintln!("music-gpui: writing {path}: {e}");
                    }
                });
            }
            "together-transfer" => {
                let wanted = arg.unwrap_or("");
                let id = self
                    .integrations
                    .together
                    .participants()
                    .into_iter()
                    .find(|p| !p.is_host && !p.is_pending && p.name == wanted)
                    .map(|p| p.id);
                match id {
                    Some(id) => self.together_transfer(&id, cx),
                    None => eprintln!("music-gpui: no admitted guest called {wanted:?}"),
                }
            }
            "together-join" => {
                let raw = arg.unwrap_or("");
                let link = match raw.strip_prefix("file:") {
                    Some(path) => std::fs::read_to_string(path).unwrap_or_else(|e| {
                        eprintln!("music-gpui: reading {path}: {e}");
                        String::new()
                    }),
                    None => raw.to_owned(),
                };
                self.together_join(&link, window, cx);
            }
            "together-leave" => self.together_leave(cx),
            "together-approve" | "together-deny" => {
                let approve = name == "together-approve";
                let pending: Vec<String> = self
                    .integrations
                    .together
                    .participants()
                    .into_iter()
                    .filter(|p| p.is_pending)
                    .map(|p| p.id)
                    .collect();
                for id in pending {
                    self.together_approve(&id, approve, cx);
                }
            }
            "together-kick" | "together-ban" => {
                let wanted = arg.unwrap_or("");
                let id = self
                    .integrations
                    .together
                    .participants()
                    .into_iter()
                    .find(|p| !p.is_host && p.name == wanted)
                    .map(|p| p.id);
                match id {
                    Some(id) => self.together_kick(&id, name == "together-ban", cx),
                    None => eprintln!("music-gpui: no guest called {wanted:?}"),
                }
            }
            "together-allow" => {
                let (what, state) = arg.unwrap_or("").split_once(':').unwrap_or(("", "on"));
                let on = state != "off";
                panel.update(cx, |p, cx| {
                    p.set_together(
                        |t| match what {
                            "add" => t.allow_guests_to_add_tracks = on,
                            "control" => t.allow_guests_to_control_playback = on,
                            "approval" => t.require_host_approval = on,
                            other => eprintln!("music-gpui: unknown together setting {other:?}"),
                        },
                        cx,
                    )
                });
            }
            "together-suggest" => {
                let ix = arg.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
                match self.last_results.get(ix).cloned() {
                    Some(track) => {
                        if !self.together_intercept_enqueue(
                            &[track],
                            AddTrackMode::PlayNext,
                            window,
                            cx,
                        ) {
                            eprintln!("music-gpui: not a guest in a session");
                        }
                    }
                    None => eprintln!("music-gpui: no search result {ix}"),
                }
            }
            "together-control" => {
                let arg = arg.unwrap_or("");
                let action = match arg {
                    "play" => Some(ControlAction::Play),
                    "pause" => Some(ControlAction::Pause),
                    "next" => Some(ControlAction::SkipNext),
                    "prev" => Some(ControlAction::SkipPrevious),
                    other => other
                        .strip_prefix("seek:")
                        .and_then(|s| s.parse::<f64>().ok())
                        .map(|s| ControlAction::SeekTo {
                            position_ms: (s.max(0.) * 1000.) as u64,
                        }),
                };
                match action {
                    Some(action) => self.together_request(action, window, cx),
                    None => eprintln!("music-gpui: unknown control {arg:?}"),
                }
            }
            "together-status" => self.print_together_status(),
            "cast-scan" => self.cast_scan(window, cx),
            "cast" => {
                let raw = arg.unwrap_or("");
                // `IP:PORT[:NAME]` connects to that address (e.g. the fake receiver).
                let mut parts = raw.splitn(3, ':');
                let direct = match (parts.next(), parts.next()) {
                    (Some(ip), Some(port)) => format!("{ip}:{port}").parse().ok().map(|addr| {
                        music_core::cast::CastDevice {
                            name: parts.next().unwrap_or(raw).to_owned(),
                            model: None,
                            id: None,
                            addr,
                        }
                    }),
                    _ => None,
                };
                let device = direct.or_else(|| {
                    self.integrations
                        .cast
                        .devices
                        .iter()
                        .find(|d| d.name.eq_ignore_ascii_case(raw))
                        .cloned()
                });
                match device {
                    Some(device) => self.cast_to(device, window, cx),
                    None => eprintln!("music-gpui: no Cast device {raw:?} (try @cast-scan)"),
                }
            }
            "cast-stop" => self.cast_stop(arg != Some("off"), cx),
            "cast-status" => {
                let c = &self.integrations.cast;
                eprintln!(
                    "music-gpui: cast devices={:?} scanning={} connecting={:?} error={:?} casting={:?}",
                    c.devices
                        .iter()
                        .map(|d| format!("{} ({})", d.name, d.addr))
                        .collect::<Vec<_>>(),
                    c.scanning,
                    c.connecting,
                    c.error,
                    self.cast_device().map(|d| d.name.clone())
                );
                eprintln!(
                    "music-gpui: cast   player: {:?} {:?} at {:.1} s / {:?} on {:?}",
                    self.playback.track.as_ref().map(|t| t.title.as_str()),
                    self.playback.status,
                    self.playback.position.as_secs_f64(),
                    self.playback.duration,
                    self.playback.output_device
                );
            }
            "ai-filter" => panel.update(cx, |p, cx| p.set_ai_filter(|a| a.enabled = on, cx)),
            "ai-category" => {
                let (key, state) = arg.unwrap_or("").split_once(':').unwrap_or(("", "on"));
                match AiCategory::from_key(key) {
                    Some(c) => panel.update(cx, |p, cx| {
                        p.set_ai_filter(
                            |a| {
                                a.categories.retain(|x| *x != c);
                                if state != "off" {
                                    a.categories.push(c);
                                }
                            },
                            cx,
                        )
                    }),
                    None => eprintln!("music-gpui: unknown AI filter category {key:?}"),
                }
            }
            "ai-filter-clear" => self.ai_filter_clear_cache(window, cx),
            "ai-filter-status" => {
                let (count, error) = self.ai_filter_status();
                let hidden: Vec<&str> = self
                    .last_results
                    .iter()
                    .filter(|t| super::ai_filter::hidden(&t.video_id))
                    .map(|t| t.title.as_str())
                    .collect();
                eprintln!(
                    "music-gpui: ai filter active={} classified={count} error={error:?} hidden in results: {hidden:?}",
                    super::ai_filter::active().is_some()
                );
            }
            "import-file" => {
                let path = PathBuf::from(arg.unwrap_or(""));
                self.import_start(ImportSource::File(path), window, cx);
            }
            "spotify-playlist" => {
                let url = arg.unwrap_or("").to_owned();
                self.import_start(ImportSource::SpotifyPlaylist(url), window, cx);
            }
            "spotify-liked" => self.import_start(ImportSource::SpotifyLiked, window, cx),
            "spotify-playlists" => self.spotify_load_playlists(window, cx),
            "spotify-connect" => panel.update(cx, |p, cx| p.spotify_connect(cx)),
            "import-accept" => {
                if let Some(ix) = arg.and_then(|a| a.parse::<usize>().ok()) {
                    self.import_toggle_accept(ix, cx);
                }
            }
            "import-save" => self.import_save(arg.map(str::to_owned), window, cx),
            "import-cancel" => self.import_cancel(),
            "import-status" => self.print_import_status(),
            "export" => {
                let mut parts = arg.unwrap_or("").splitn(3, ':');
                let (Some(name), Some(format), Some(path)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    eprintln!("music-gpui: @export wants PLAYLIST:FORMAT:PATH");
                    return true;
                };
                let Some(format) = Format::parse(format) else {
                    eprintln!("music-gpui: unknown format {format:?}");
                    return true;
                };
                let found = self.library().and_then(|lib| {
                    lib.playlists(PlaylistSort::Name, false)
                        .ok()?
                        .into_iter()
                        .find(|p| p.name == name)
                });
                match found {
                    Some(p) => {
                        self.export_playlist(p.id, p.name, format, Some(path.into()), window, cx)
                    }
                    None => eprintln!("music-gpui: no library playlist {name:?}"),
                }
            }
            _ => return false,
        }
        true
    }

    fn print_together_status(&self) {
        let t = &self.integrations.together;
        let role = match &t.role {
            Role::Idle => "idle".to_owned(),
            Role::Joining(host) => format!("joining {host}"),
            Role::Hosting(h) => format!("hosting {}", h.join.to_link()),
            Role::Guest(g) => format!(
                "guest of {}:{} pending={} clock={:?}",
                g.join.host,
                g.join.port,
                g.pending,
                g.client.clock()
            ),
        };
        eprintln!("music-gpui: together {role}; error {:?}", t.error);
        for p in t.participants() {
            eprintln!(
                "music-gpui: together   participant {} host={} pending={}",
                p.name, p.is_host, p.is_pending
            );
        }
        if let Role::Guest(g) = &t.role
            && let Some(room) = &g.room
        {
            eprintln!(
                "music-gpui: together   room: {} tracks, current {:?}, playing {}, host position {} ms",
                room.queue.len(),
                room.current().map(|c| c.title.as_str()),
                room.is_playing,
                room.position_ms
            );
        }
        eprintln!(
            "music-gpui: together   local: {:?} {:?} at {:.1} s (queue next: {:?})",
            self.playback.track.as_ref().map(|t| t.title.as_str()),
            self.playback.status,
            self.clock.position(Instant::now()).as_secs_f64(),
            self.queue.peek_next().map(|t| t.title.as_str())
        );
        for line in &t.log {
            eprintln!("music-gpui: together   log: {line}");
        }
    }

    fn print_import_status(&self) {
        let Some(job) = &self.integrations.import.job else {
            eprintln!("music-gpui: import: none");
            return;
        };
        let (found, missing) = job.counts();
        eprintln!(
            "music-gpui: import “{}” {:?} progress {:?}: {found} found, {missing} not found",
            job.name,
            job.phase,
            job.progress()
        );
        for (ix, (source, result)) in job.sources.iter().zip(&job.results).enumerate() {
            let line = match result {
                Resolution::Direct(t) => format!("direct {}", t.video_id),
                Resolution::Matched { track, score } => {
                    format!("matched {} “{}” ({score:.2})", track.video_id, track.title)
                }
                Resolution::Unmatched { best, error } => format!(
                    "unmatched; best {:?} error {:?}",
                    best.as_ref().map(|(t, s)| format!("{} ({s:.2})", t.title)),
                    error
                ),
            };
            eprintln!("music-gpui: import   {ix}: {} → {line}", source.display());
        }
        if matches!(job.phase, Phase::Failed(_)) {
            eprintln!("music-gpui: import failed");
        }
    }
}
