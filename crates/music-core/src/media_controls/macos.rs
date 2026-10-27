//! macOS: `MPNowPlayingInfoCenter` (what Control Center shows) and
//! `MPRemoteCommandCenter` (media keys, Control Center buttons, AirPods).
//!
//! Ported from Hummingbird `src/services/controllers/macos.rs` (upstream commit
//! e15e8ba0ef6ff5445294030d373e3c8ccc7d780c). Copyright Hummingbird contributors,
//! Apache-2.0, modified: event callback instead of the playback-thread bridge,
//! playback rate published so the OS extrapolates the position, artwork from
//! URL (fetched on a thread) or bytes without leaking the `NSImage`, stop
//! command, handlers removed and now-playing info cleared on drop.

use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::NSImage;
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSData, NSMutableDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumTitle,
    MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
    MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyElapsedPlaybackTime,
    MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState, MPRemoteCommand,
    MPRemoteCommandCenter, MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
};

use super::{
    Artwork, Backend, BackendResult, Handler, MediaControlEvent, MediaControlsError, MediaMetadata,
    MediaPlayback,
};

type Info = NSMutableDictionary<NSString, AnyObject>;

pub(super) struct MacControls {
    /// Registered command targets, removed again on drop.
    targets: Vec<(Retained<MPRemoteCommand>, Retained<AnyObject>)>,
    /// Bumped per track so a slow artwork download can't land on the next track.
    art_generation: Arc<AtomicU64>,
}

impl MacControls {
    pub(super) fn new(handler: Handler) -> Result<Self, MediaControlsError> {
        let mut targets = Vec::new();
        // SAFETY: plain MediaPlayer calls; the handler blocks only capture
        // `Send + Sync` Rust data and return a valid status.
        unsafe {
            let center = MPRemoteCommandCenter::sharedCommandCenter();
            let simple = [
                (center.playCommand(), MediaControlEvent::Play),
                (center.pauseCommand(), MediaControlEvent::Pause),
                (center.togglePlayPauseCommand(), MediaControlEvent::Toggle),
                (center.stopCommand(), MediaControlEvent::Stop),
                (center.nextTrackCommand(), MediaControlEvent::Next),
                (center.previousTrackCommand(), MediaControlEvent::Previous),
            ];
            for (command, event) in simple {
                let handler = handler.clone();
                let block = RcBlock::new(move |_: NonNull<MPRemoteCommandEvent>| {
                    handler(event.clone());
                    MPRemoteCommandHandlerStatus::Success
                });
                command.setEnabled(true);
                let target = command.addTargetWithHandler(&block);
                targets.push((command, target));
            }

            let seek = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
                // changePlaybackPositionCommand always sends this subclass.
                let event = event.cast::<MPChangePlaybackPositionCommandEvent>();
                let secs = event.as_ref().positionTime();
                handler(MediaControlEvent::SeekTo(Duration::from_secs_f64(
                    secs.max(0.0),
                )));
                MPRemoteCommandHandlerStatus::Success
            });
            let command = center.changePlaybackPositionCommand();
            command.setEnabled(true);
            let target = command.addTargetWithHandler(&seek);
            targets.push((Retained::into_super(command), target));
        }
        Ok(Self {
            targets,
            art_generation: Arc::new(AtomicU64::new(0)),
        })
    }
}

/// Copies the current now-playing info (or starts empty), lets `edit` change it, publishes it.
fn edit_info(edit: impl FnOnce(&Info)) {
    // SAFETY: MPNowPlayingInfoCenter is documented as usable from any thread;
    // keys are MediaPlayer's own constants with matching value types.
    unsafe {
        let center = MPNowPlayingInfoCenter::defaultCenter();
        let info: Retained<Info> = NSMutableDictionary::new();
        if let Some(previous) = center.nowPlayingInfo() {
            info.addEntriesFromDictionary(&previous);
        }
        edit(&info);
        center.setNowPlayingInfo(Some(&info));
    }
}

fn set(info: &Info, key: &NSString, value: &AnyObject) {
    // SAFETY: `NSMutableDictionary` copies the key and retains the value.
    unsafe { info.setObject_forKey(value, ProtocolObject::from_ref(key)) };
}

fn set_artwork_bytes(bytes: &[u8]) -> BackendResult {
    let data = NSData::with_bytes(bytes);
    let image = NSImage::initWithData(NSImage::alloc(), &data)
        .ok_or_else(|| MediaControlsError("artwork is not a readable image".into()))?;
    // SAFETY: `size` has no preconditions; the request handler returns a
    // pointer to the image it owns, which lives as long as the artwork object.
    unsafe {
        let size = image.size();
        let handler = RcBlock::new(move |_: CGSize| NonNull::from(&*image));
        let artwork = MPMediaItemArtwork::initWithBoundsSize_requestHandler(
            MPMediaItemArtwork::alloc(),
            size,
            &handler,
        );
        edit_info(|info| set(info, MPMediaItemPropertyArtwork, &artwork));
    }
    Ok(())
}

impl Backend for MacControls {
    fn set_metadata(&mut self, meta: Option<&MediaMetadata>) -> BackendResult {
        self.art_generation.fetch_add(1, Ordering::SeqCst);
        // SAFETY: see `edit_info`.
        unsafe {
            let center = MPNowPlayingInfoCenter::defaultCenter();
            let Some(meta) = meta else {
                center.setNowPlayingInfo(None);
                return Ok(());
            };
            let info: Retained<Info> = NSMutableDictionary::new();
            set(
                &info,
                MPMediaItemPropertyTitle,
                &NSString::from_str(&meta.title),
            );
            set(
                &info,
                MPMediaItemPropertyArtist,
                &NSString::from_str(&meta.artists),
            );
            if let Some(album) = &meta.album {
                set(
                    &info,
                    MPMediaItemPropertyAlbumTitle,
                    &NSString::from_str(album),
                );
            }
            if let Some(duration) = meta.duration {
                let secs = NSNumber::new_f64(duration.as_secs_f64());
                set(&info, MPMediaItemPropertyPlaybackDuration, &secs);
            }
            // Keep the artwork when only the details of the same track changed.
            if let Some(previous) = center.nowPlayingInfo()
                && let Some(art) = previous.objectForKey(MPMediaItemPropertyArtwork)
                && previous
                    .objectForKey(MPMediaItemPropertyTitle)
                    .is_some_and(|t| {
                        t.downcast_ref::<NSString>()
                            .is_some_and(|t| t.to_string() == meta.title)
                    })
            {
                set(&info, MPMediaItemPropertyArtwork, &art);
            }
            center.setNowPlayingInfo(Some(&info));
        }
        Ok(())
    }

    fn set_artwork(&mut self, art: Artwork<'_>) -> BackendResult {
        match art {
            Artwork::Bytes(bytes) => set_artwork_bytes(bytes),
            Artwork::Url(url) => {
                // MPMediaItemArtwork needs an image, so fetch it off the UI thread.
                let generation = self.art_generation.clone();
                let expected = generation.load(Ordering::SeqCst);
                let url = url.to_owned();
                std::thread::Builder::new()
                    .name("music-core-artwork".into())
                    .spawn(move || match crate::fetch_bytes(&url) {
                        Ok(bytes) if generation.load(Ordering::SeqCst) == expected => {
                            if let Err(e) = set_artwork_bytes(&bytes) {
                                log::warn!("{e}");
                            }
                        }
                        Ok(_) => {}
                        Err(e) => log::warn!("artwork download failed: {e}"),
                    })
                    .map(drop)
                    .map_err(|e| MediaControlsError(e.to_string()))
            }
        }
    }

    fn set_playback(&mut self, playback: MediaPlayback, position: Duration) -> BackendResult {
        let rate = if playback == MediaPlayback::Playing {
            1.0
        } else {
            0.0
        };
        edit_info(|info| {
            let elapsed = NSNumber::new_f64(position.as_secs_f64());
            set(
                info,
                unsafe { MPNowPlayingInfoPropertyElapsedPlaybackTime },
                &elapsed,
            );
            set(
                info,
                unsafe { MPNowPlayingInfoPropertyPlaybackRate },
                &NSNumber::new_f64(rate),
            );
        });
        // SAFETY: see `edit_info`.
        unsafe {
            MPNowPlayingInfoCenter::defaultCenter().setPlaybackState(match playback {
                MediaPlayback::Stopped => MPNowPlayingPlaybackState::Stopped,
                MediaPlayback::Paused => MPNowPlayingPlaybackState::Paused,
                MediaPlayback::Playing => MPNowPlayingPlaybackState::Playing,
            });
        }
        Ok(())
    }
}

impl Drop for MacControls {
    fn drop(&mut self) {
        self.art_generation.fetch_add(1, Ordering::SeqCst);
        // SAFETY: each target came from `addTargetWithHandler` on that command.
        unsafe {
            for (command, target) in &self.targets {
                command.removeTarget(Some(target));
                command.setEnabled(false);
            }
            let center = MPNowPlayingInfoCenter::defaultCenter();
            center.setNowPlayingInfo(None);
            center.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2 RGB PNG.
    const PNG: [u8; 74] = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 2,
        0, 0, 0, 253, 212, 154, 115, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 192,
        240, 31, 140, 128, 24, 0, 29, 240, 3, 253, 211, 208, 125, 38, 0, 0, 0, 0, 73, 69, 78, 68,
        174, 66, 96, 130,
    ];

    fn info_string(key: &NSString) -> Option<String> {
        unsafe {
            let info = MPNowPlayingInfoCenter::defaultCenter().nowPlayingInfo()?;
            let value = info.objectForKey(key)?;
            Some(value.downcast_ref::<NSString>()?.to_string())
        }
    }

    fn info_f64(key: &NSString) -> Option<f64> {
        unsafe {
            let info = MPNowPlayingInfoCenter::defaultCenter().nowPlayingInfo()?;
            let value = info.objectForKey(key)?;
            Some(value.downcast_ref::<NSNumber>()?.as_f64())
        }
    }

    /// Writes into the process's now-playing dictionary and reads it back
    /// (no UI involved, so this says nothing about what Control Center shows).
    #[test]
    fn publishes_now_playing_info() {
        let mut controls = MacControls::new(Arc::new(|_| {})).unwrap();
        let meta = MediaMetadata {
            title: "Around the World".into(),
            artists: "Daft Punk".into(),
            album: Some("Homework".into()),
            duration: Some(Duration::from_secs(429)),
        };
        controls.set_metadata(Some(&meta)).unwrap();
        controls
            .set_playback(MediaPlayback::Playing, Duration::from_secs(30))
            .unwrap();
        controls.set_artwork(Artwork::Bytes(&PNG)).unwrap();
        unsafe {
            assert_eq!(
                info_string(MPMediaItemPropertyTitle).as_deref(),
                Some("Around the World")
            );
            assert_eq!(
                info_string(MPMediaItemPropertyArtist).as_deref(),
                Some("Daft Punk")
            );
            assert_eq!(
                info_string(MPMediaItemPropertyAlbumTitle).as_deref(),
                Some("Homework")
            );
            assert_eq!(info_f64(MPMediaItemPropertyPlaybackDuration), Some(429.0));
            assert_eq!(
                info_f64(MPNowPlayingInfoPropertyElapsedPlaybackTime),
                Some(30.0)
            );
            assert_eq!(info_f64(MPNowPlayingInfoPropertyPlaybackRate), Some(1.0));
            let info = MPNowPlayingInfoCenter::defaultCenter()
                .nowPlayingInfo()
                .unwrap();
            assert!(info.objectForKey(MPMediaItemPropertyArtwork).is_some());
        }
        assert!(
            controls
                .set_artwork(Artwork::Bytes(b"not an image"))
                .is_err()
        );

        drop(controls);
        unsafe {
            assert!(
                MPNowPlayingInfoCenter::defaultCenter()
                    .nowPlayingInfo()
                    .is_none()
            );
        }
    }
}
