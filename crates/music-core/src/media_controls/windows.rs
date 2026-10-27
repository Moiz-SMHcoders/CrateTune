//! Windows: System Media Transport Controls (media keys, taskbar/lock-screen flyout).
//!
//! Ported from Hummingbird `src/services/controllers/windows.rs` (upstream commit
//! e15e8ba0ef6ff5445294030d373e3c8ccc7d780c). Copyright Hummingbird contributors,
//! Apache-2.0, modified: event callback instead of the playback-thread bridge,
//! blocking `join()` instead of `.await`, artwork from a URL, no shuffle/repeat,
//! returns an error instead of panicking without an `HWND`.

use std::ffi::c_void;
use std::time::Duration;

use windows::Foundation::{TypedEventHandler, Uri};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs, SystemMediaTransportControlsDisplayUpdater,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::Streams::{
    DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::core::HSTRING;

use super::{
    Artwork, Backend, BackendResult, Handler, MediaControlEvent, MediaControlsConfig,
    MediaControlsError, MediaMetadata, MediaPlayback,
};

impl From<windows::core::Error> for MediaControlsError {
    fn from(e: windows::core::Error) -> Self {
        MediaControlsError(e.to_string())
    }
}

pub(super) struct WindowsControls {
    controls: SystemMediaTransportControls,
    display: SystemMediaTransportControlsDisplayUpdater,
    timeline: SystemMediaTransportControlsTimelineProperties,
    tokens: [i64; 2],
}

impl WindowsControls {
    pub(super) fn new(
        config: &MediaControlsConfig,
        handler: Handler,
    ) -> Result<Self, MediaControlsError> {
        let hwnd = config
            .hwnd
            .ok_or_else(|| MediaControlsError("Windows needs MediaControlsConfig::hwnd".into()))?;
        let interop: ISystemMediaTransportControlsInterop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        // SAFETY: the caller passes the HWND of a live top-level window.
        let controls: SystemMediaTransportControls =
            unsafe { interop.GetForWindow(HWND(hwnd.get() as *mut c_void))? };
        let display = controls.DisplayUpdater()?;
        let timeline = SystemMediaTransportControlsTimelineProperties::new()?;

        controls.SetIsEnabled(true)?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsStopEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;

        let on_button = handler.clone();
        let buttons = controls.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_, args| {
            let Some(args) = args.as_ref() else {
                return Ok(());
            };
            let event = match args.Button()? {
                SystemMediaTransportControlsButton::Play => MediaControlEvent::Play,
                SystemMediaTransportControlsButton::Pause => MediaControlEvent::Pause,
                SystemMediaTransportControlsButton::Stop => MediaControlEvent::Stop,
                SystemMediaTransportControlsButton::Next => MediaControlEvent::Next,
                SystemMediaTransportControlsButton::Previous => MediaControlEvent::Previous,
                _ => return Ok(()),
            };
            on_button(event);
            Ok(())
        }))?;

        let seek = controls.PlaybackPositionChangeRequested(&TypedEventHandler::<
            SystemMediaTransportControls,
            PlaybackPositionChangeRequestedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                // TimeSpan counts 100 ns ticks.
                let ticks = args.RequestedPlaybackPosition()?.Duration.max(0) as u64;
                handler(MediaControlEvent::SeekTo(Duration::from_nanos(ticks * 100)));
            }
            Ok(())
        }))?;

        Ok(Self {
            controls,
            display,
            timeline,
            tokens: [buttons, seek],
        })
    }
}

impl Backend for WindowsControls {
    fn set_metadata(&mut self, meta: Option<&MediaMetadata>) -> BackendResult {
        self.display.ClearAll()?;
        let Some(meta) = meta else {
            self.display.Update()?;
            return Ok(());
        };
        self.display.SetType(MediaPlaybackType::Music)?;
        let music = self.display.MusicProperties()?;
        music.SetTitle(&HSTRING::from(meta.title.as_str()))?;
        music.SetArtist(&HSTRING::from(meta.artists.as_str()))?;
        if let Some(album) = &meta.album {
            music.SetAlbumTitle(&HSTRING::from(album.as_str()))?;
        }
        self.display.Update()?;

        let end = meta.duration.unwrap_or_default();
        self.timeline.SetStartTime(Duration::ZERO.into())?;
        self.timeline.SetMinSeekTime(Duration::ZERO.into())?;
        self.timeline.SetEndTime(end.into())?;
        self.timeline.SetMaxSeekTime(end.into())?;
        self.controls.UpdateTimelineProperties(&self.timeline)?;
        Ok(())
    }

    fn set_artwork(&mut self, art: Artwork<'_>) -> BackendResult {
        let reference = match art {
            Artwork::Url(url) => {
                RandomAccessStreamReference::CreateFromUri(&Uri::CreateUri(&HSTRING::from(url))?)?
            }
            Artwork::Bytes(bytes) => {
                let stream = InMemoryRandomAccessStream::new()?;
                let writer = DataWriter::CreateDataWriter(&stream)?;
                writer.WriteBytes(bytes)?;
                writer.StoreAsync()?.join()?;
                writer.DetachStream()?;
                stream.Seek(0)?;
                RandomAccessStreamReference::CreateFromStream(&stream)?
            }
        };
        self.display.SetThumbnail(&reference)?;
        self.display.Update()?;
        Ok(())
    }

    fn set_playback(&mut self, playback: MediaPlayback, position: Duration) -> BackendResult {
        self.controls.SetPlaybackStatus(match playback {
            MediaPlayback::Stopped => MediaPlaybackStatus::Stopped,
            MediaPlayback::Paused => MediaPlaybackStatus::Paused,
            MediaPlayback::Playing => MediaPlaybackStatus::Playing,
        })?;
        self.timeline.SetPosition(position.into())?;
        self.controls.UpdateTimelineProperties(&self.timeline)?;
        Ok(())
    }
}

impl Drop for WindowsControls {
    fn drop(&mut self) {
        let _ = self.controls.RemoveButtonPressed(self.tokens[0]);
        let _ = self
            .controls
            .RemovePlaybackPositionChangeRequested(self.tokens[1]);
        let _ = self.display.ClearAll();
        let _ = self.display.Update();
        let _ = self.controls.SetIsEnabled(false);
    }
}
