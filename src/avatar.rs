//! Gravatar avatar fetching with on-disk + in-memory caching.
//!
//! `AvatarCache` is the per-app coordinator: it tracks which emails
//! we've requested, spawns a worker thread per request to download +
//! decode + cache, and surfaces completed RGBA pixel buffers as
//! [`aetna_core::Image`] handles for direct use with the `image()`
//! widget.
//!
//! Disk cache lives at `$XDG_CACHE_HOME/whisper-git/avatars/{hash}.png`
//! so a once-fetched avatar persists across app restarts. Failed
//! requests are remembered in-memory only — we don't poison the disk
//! cache when Gravatar 404s — and don't retry within the same session.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use aetna_core::image::Image;
use winit::event_loop::EventLoopProxy;

/// Side length of the avatar image we request from Gravatar and store
/// in cache. Sized roughly 2× the on-screen render so simple
/// downscaling keeps it crisp at the typical row density.
pub const AVATAR_PIXELS: u32 = 64;

enum AvatarState {
    InFlight,
    Loaded(Image),
    Failed,
}

struct DownloadResult {
    email_key: String,
    pixels: Option<Vec<u8>>,
}

/// Manages avatar downloads and results.
///
/// `request_avatar` is fire-and-forget — it spawns a worker thread
/// when called for an unseen email, and is a no-op otherwise. Call
/// `drain_completions` once per UI tick to fold any finished workers
/// into the in-memory cache; `get(email)` reads back the loaded
/// `Image` (or `None` when still in-flight, failed, or unrequested).
pub struct AvatarCache {
    states: HashMap<String, AvatarState>,
    sender: Sender<DownloadResult>,
    receiver: Receiver<DownloadResult>,
    /// `Some` for the live app — used to wake the event loop after a
    /// worker thread finishes so the UI rebuilds with the new avatar.
    /// `None` for the screenshot pipeline, which drives prefetches
    /// synchronously and never needs to wake.
    proxy: Option<EventLoopProxy<()>>,
}

impl AvatarCache {
    pub fn new(proxy: EventLoopProxy<()>) -> Self {
        Self::with_optional_proxy(Some(proxy))
    }

    /// Construct without a proxy — only useful when the cache will
    /// only be driven through `prefetch_sync` (no async completions
    /// to deliver). The screenshot pipeline uses this to backfill
    /// avatars before its single render pass.
    pub fn new_sync_only() -> Self {
        Self::with_optional_proxy(None)
    }

    fn with_optional_proxy(proxy: Option<EventLoopProxy<()>>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            states: HashMap::new(),
            sender,
            receiver,
            proxy,
        }
    }

    /// Spawn a download for this email if we haven't already. The
    /// email is normalized (trimmed + lower-cased) per the Gravatar
    /// hashing contract, and used as the cache key. Without a proxy
    /// the request still spawns; the worker just won't wake the
    /// event loop on completion (caller is expected to drain
    /// manually or use `prefetch_sync` instead).
    pub fn request(&mut self, email: &str) {
        let key = email.trim().to_lowercase();
        if key.is_empty() || self.states.contains_key(&key) {
            return;
        }
        self.states.insert(key.clone(), AvatarState::InFlight);
        let sender = self.sender.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let pixels = download_avatar(&key);
            let _ = sender.send(DownloadResult {
                email_key: key,
                pixels,
            });
            if let Some(proxy) = proxy {
                let _ = proxy.send_event(());
            }
        });
    }

    /// Synchronous variant — block on the download, decode, and stash
    /// the result. Used by the screenshot pipeline (no polling loop
    /// to drain async completions). Idempotent: skips emails already
    /// loaded or known-failed in this cache.
    pub fn prefetch_sync(&mut self, email: &str) {
        let key = email.trim().to_lowercase();
        if key.is_empty() {
            return;
        }
        if matches!(
            self.states.get(&key),
            Some(AvatarState::Loaded(_)) | Some(AvatarState::Failed)
        ) {
            return;
        }
        let state = match download_avatar(&key) {
            Some(p) => match Image::try_from_rgba8(AVATAR_PIXELS, AVATAR_PIXELS, p) {
                Some(img) => AvatarState::Loaded(img),
                None => AvatarState::Failed,
            },
            None => AvatarState::Failed,
        };
        self.states.insert(key, state);
    }

    /// Drain every completion ready this tick. Returns true if any
    /// new avatars landed (caller can request a redraw — the
    /// downstream `image()` widget will pick them up automatically
    /// once `get()` starts returning `Some`).
    pub fn drain_completions(&mut self) -> bool {
        let mut any = false;
        while let Ok(result) = self.receiver.try_recv() {
            let state = match result.pixels {
                Some(p) => match Image::try_from_rgba8(AVATAR_PIXELS, AVATAR_PIXELS, p) {
                    Some(img) => AvatarState::Loaded(img),
                    None => AvatarState::Failed,
                },
                None => AvatarState::Failed,
            };
            self.states.insert(result.email_key, state);
            any = true;
        }
        any
    }

    /// Loaded `Image` for this email, if present. Cheap clone (Arc
    /// internally) so callers can stash it in per-frame snapshots.
    pub fn get(&self, email: &str) -> Option<Image> {
        let key = email.trim().to_lowercase();
        match self.states.get(&key) {
            Some(AvatarState::Loaded(img)) => Some(img.clone()),
            _ => None,
        }
    }
}

/// Trait bridge — `Image::from_rgba8` panics on a length mismatch,
/// but the disk cache or HTTP body could legitimately disagree with
/// our expected dimensions. Wrap the assertion in a check + return
/// `None` so we can demote to `Failed` instead of crashing.
trait ImageTryFrom {
    fn try_from_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Option<Image>;
}

impl ImageTryFrom for Image {
    fn try_from_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Option<Image> {
        let expected = (width as usize) * (height as usize) * 4;
        if pixels.len() != expected {
            return None;
        }
        Some(Image::from_rgba8(width, height, pixels))
    }
}

/// Fetch + decode Gravatar for one email. Returns RGBA pixels at
/// `AVATAR_PIXELS × AVATAR_PIXELS`, or `None` on any failure
/// (network, 404, decode). Honors a disk cache to avoid repeat
/// fetches across runs.
fn download_avatar(email_key: &str) -> Option<Vec<u8>> {
    let hash = format!("{:x}", md5::compute(email_key.as_bytes()));
    let cache_dir = avatar_cache_dir();
    let cache_file = cache_dir.join(format!("{hash}.png"));

    if cache_file.exists()
        && let Ok(img) = image::open(&cache_file)
    {
        let rgba = img
            .resize_exact(
                AVATAR_PIXELS,
                AVATAR_PIXELS,
                image::imageops::FilterType::Lanczos3,
            )
            .to_rgba8();
        return Some(rgba.into_raw());
    }

    // `d=404` makes Gravatar 404 when the email has no avatar — we
    // fall back to our local identicon in that case rather than
    // accepting Gravatar's stock identicon.
    let url = format!("https://www.gravatar.com/avatar/{hash}?s={AVATAR_PIXELS}&d=404");
    let mut response = ureq::get(&url).call().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img
        .resize_exact(
            AVATAR_PIXELS,
            AVATAR_PIXELS,
            image::imageops::FilterType::Lanczos3,
        )
        .to_rgba8();

    // Persist for next session — best-effort; a write failure (e.g.
    // read-only XDG_CACHE_HOME) shouldn't drop the avatar we just
    // decoded for the live cache.
    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = rgba.save(&cache_file);

    Some(rgba.into_raw())
}

fn avatar_cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".cache")
        });
    base.join("whisper-git").join("avatars")
}
