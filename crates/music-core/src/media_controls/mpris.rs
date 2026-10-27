//! Linux: MPRIS (`org.mpris.MediaPlayer2`) on the D-Bus session bus.
//!
//! Ported from Hummingbird `src/services/controllers/mpris.rs` (upstream commit
//! e15e8ba0ef6ff5445294030d373e3c8ccc7d780c). Copyright Hummingbird contributors,
//! Apache-2.0, modified: no tokio (zbus' own executor thread serves calls, updates
//! are sent with `zbus::block_on`), std `Mutex` state, event callback instead of
//! the playback-thread bridge, track ids, artwork URLs passed through, no
//! shuffle/repeat, configurable bus name and identity.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mpris_server::zbus::{self, fdo};
use mpris_server::{
    LoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface, Property, RootInterface,
    Server, Signal, Time, TrackId, Volume,
};

use super::{
    Artwork, Backend, BackendResult, Handler, MediaControlEvent, MediaControlsConfig,
    MediaControlsError, MediaMetadata, MediaPlayback,
};

fn time(d: Duration) -> Time {
    Time::from_micros(d.as_micros().min(i64::MAX as u128) as i64)
}

fn err(e: zbus::Error) -> MediaControlsError {
    MediaControlsError(format!("mpris: {e}"))
}

#[derive(Default)]
struct Data {
    meta: Option<MediaMetadata>,
    track_id: Option<TrackId>,
    art_url: Option<String>,
    /// Temp file holding artwork passed as bytes.
    art_file: Option<PathBuf>,
    playback: Option<MediaPlayback>,
    /// Last known position and when it was observed.
    position: (Duration, Option<Instant>),
}

impl Data {
    fn status(&self) -> PlaybackStatus {
        match self.playback {
            Some(MediaPlayback::Playing) => PlaybackStatus::Playing,
            Some(MediaPlayback::Paused) => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        }
    }

    fn can_control(&self) -> bool {
        self.meta.is_some() && self.playback != Some(MediaPlayback::Stopped)
    }

    /// Position now, extrapolated while playing.
    fn position(&self) -> Duration {
        let (pos, at) = self.position;
        match (self.playback, at) {
            (Some(MediaPlayback::Playing), Some(at)) => pos + at.elapsed(),
            _ => pos,
        }
    }

    fn metadata(&self) -> Metadata {
        let mut out = Metadata::new();
        if let Some(meta) = &self.meta {
            out.set_trackid(self.track_id.clone());
            out.set_title(Some(meta.title.clone()));
            out.set_artist(Some([meta.artists.clone()]));
            out.set_album(meta.album.clone());
            out.set_length(meta.duration.map(time));
            out.set_art_url(self.art_url.clone());
        }
        out
    }
}

struct Imp {
    handler: Handler,
    identity: String,
    desktop_entry: Option<String>,
    data: Arc<Mutex<Data>>,
}

impl Imp {
    fn data(&self) -> std::sync::MutexGuard<'_, Data> {
        self.data.lock().unwrap()
    }
}

impl RootInterface for Imp {
    async fn raise(&self) -> fdo::Result<()> {
        Ok(())
    }
    async fn quit(&self) -> fdo::Result<()> {
        Ok(())
    }
    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn set_fullscreen(&self, _: bool) -> zbus::Result<()> {
        Ok(())
    }
    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn identity(&self) -> fdo::Result<String> {
        Ok(self.identity.clone())
    }
    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok(self.desktop_entry.clone().unwrap_or_default())
    }
    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }
}

impl PlayerInterface for Imp {
    async fn next(&self) -> fdo::Result<()> {
        (self.handler)(MediaControlEvent::Next);
        Ok(())
    }
    async fn previous(&self) -> fdo::Result<()> {
        (self.handler)(MediaControlEvent::Previous);
        Ok(())
    }
    async fn pause(&self) -> fdo::Result<()> {
        (self.handler)(MediaControlEvent::Pause);
        Ok(())
    }
    async fn play_pause(&self) -> fdo::Result<()> {
        (self.handler)(MediaControlEvent::Toggle);
        Ok(())
    }
    async fn stop(&self) -> fdo::Result<()> {
        (self.handler)(MediaControlEvent::Stop);
        Ok(())
    }
    async fn play(&self) -> fdo::Result<()> {
        (self.handler)(MediaControlEvent::Play);
        Ok(())
    }
    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        (self.handler)(MediaControlEvent::SeekBy(offset.as_micros() as f64 / 1e6));
        Ok(())
    }
    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        // Per spec, stale requests for another track are ignored.
        if self.data().track_id.as_ref() == Some(&track_id) && !position.is_negative() {
            let micros = position.as_micros() as u64;
            (self.handler)(MediaControlEvent::SeekTo(Duration::from_micros(micros)));
        }
        Ok(())
    }
    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Ok(())
    }
    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(self.data().status())
    }
    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(LoopStatus::None)
    }
    async fn set_loop_status(&self, _: LoopStatus) -> zbus::Result<()> {
        Ok(())
    }
    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn set_rate(&self, _: PlaybackRate) -> zbus::Result<()> {
        Ok(())
    }
    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn set_shuffle(&self, _: bool) -> zbus::Result<()> {
        Ok(())
    }
    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(self.data().metadata())
    }
    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(1.0)
    }
    async fn set_volume(&self, volume: Volume) -> zbus::Result<()> {
        (self.handler)(MediaControlEvent::SetVolume(volume.clamp(0.0, 1.0)));
        Ok(())
    }
    async fn position(&self) -> fdo::Result<Time> {
        Ok(time(self.data().position()))
    }
    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(true)
    }
    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(true)
    }
    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.data().meta.is_some())
    }
    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.data().can_control())
    }
    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.data().can_control())
    }
    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}

pub(super) struct MprisControls {
    server: Server<Imp>,
    data: Arc<Mutex<Data>>,
    track_counter: u64,
    art_counter: u64,
}

impl MprisControls {
    pub(super) fn new(
        config: &MediaControlsConfig,
        handler: Handler,
    ) -> Result<Self, MediaControlsError> {
        let data = Arc::new(Mutex::new(Data::default()));
        let imp = Imp {
            handler,
            identity: config.display_name.clone(),
            desktop_entry: config.desktop_entry.clone(),
            data: data.clone(),
        };
        let server = zbus::block_on(Server::new(&config.dbus_name, imp)).map_err(err)?;
        Ok(Self {
            server,
            data,
            track_counter: 0,
            art_counter: 0,
        })
    }

    fn data(&self) -> std::sync::MutexGuard<'_, Data> {
        self.data.lock().unwrap()
    }

    fn emit(&self, properties: impl IntoIterator<Item = Property>) -> BackendResult {
        zbus::block_on(self.server.properties_changed(properties)).map_err(err)
    }

    fn remove_art_file(&self) {
        if let Some(path) = self.data().art_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Backend for MprisControls {
    fn set_metadata(&mut self, meta: Option<&MediaMetadata>) -> BackendResult {
        let same_track = {
            let data = self.data();
            data.meta.as_ref().map(|m| &m.title) == meta.map(|m| &m.title)
        };
        if !same_track {
            self.remove_art_file();
            self.track_counter += 1;
        }
        let path = format!("/org/musiccore/track/{}", self.track_counter);
        let (metadata, status, can) = {
            let mut data = self.data();
            data.meta = meta.cloned();
            data.track_id = meta.and_then(|_| TrackId::try_from(path).ok());
            if !same_track {
                data.art_url = None;
            }
            (data.metadata(), data.status(), data.can_control())
        };
        self.emit([
            Property::Metadata(metadata),
            Property::PlaybackStatus(status),
            Property::CanPlay(meta.is_some()),
            Property::CanPause(can),
            Property::CanSeek(can),
        ])
    }

    fn set_artwork(&mut self, art: Artwork<'_>) -> BackendResult {
        let url = match art {
            Artwork::Url(url) => url.to_owned(),
            Artwork::Bytes(bytes) => {
                self.art_counter += 1;
                let path = std::env::temp_dir().join(format!(
                    "music-core-mpris-art-{}-{}",
                    std::process::id(),
                    self.art_counter
                ));
                std::fs::write(&path, bytes).map_err(|e| MediaControlsError(e.to_string()))?;
                // Clients read the old file lazily; only remove it once the new one exists.
                self.remove_art_file();
                let url = format!("file://{}", path.display()).replace(' ', "%20");
                self.data().art_file = Some(path);
                url
            }
        };
        let metadata = {
            let mut data = self.data();
            data.art_url = Some(url);
            data.metadata()
        };
        self.emit([Property::Metadata(metadata)])
    }

    fn set_playback(&mut self, playback: MediaPlayback, position: Duration) -> BackendResult {
        let (changed, jumped, can) = {
            let mut data = self.data();
            let expected = data.position();
            let changed = data.playback != Some(playback);
            data.playback = Some(playback);
            data.position = (position, Some(Instant::now()));
            (
                changed,
                position.abs_diff(expected) > Duration::from_secs(1),
                data.can_control(),
            )
        };
        if changed {
            self.emit([
                Property::PlaybackStatus(self.data().status()),
                Property::CanPause(can),
                Property::CanSeek(can),
            ])?;
        }
        if jumped {
            zbus::block_on(self.server.emit(Signal::Seeked {
                position: time(position),
            }))
            .map_err(err)?;
        }
        Ok(())
    }

    fn position_hint(&mut self, position: Duration) {
        self.data().position = (position, Some(Instant::now()));
    }
}

impl Drop for MprisControls {
    fn drop(&mut self) {
        self.remove_art_file();
    }
}
