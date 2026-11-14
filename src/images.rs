//! Artwork cache.
//!
//! Covers are fetched and decoded off the UI thread, downscaled to the size
//! they are drawn at, and kept in a byte-budgeted LRU. Evicted images also
//! release their GPU atlas texture, so memory stays flat no matter how long
//! the app runs or how many pages are visited.
//!
//! The same cover is drawn at different sizes (a card, then Now Playing), so
//! an image decoded small is decoded again, larger, when a larger copy is
//! asked for. The downloaded files are kept on disk (bounded), which makes
//! that, and the next start, cheap.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::*;
use image::{Frame, RgbaImage, imageops::FilterType};
use smallvec::SmallVec;
use tokio::sync::Semaphore;

use crate::runtime;
use crate::spotify::http;

/// Decoded pixels we are willing to keep around.
const BUDGET_BYTES: usize = 48 * 1024 * 1024;
/// Largest edge kept after decoding. Nothing in the UI draws bigger than this.
const MAX_EDGE: u32 = 1280;
const MAX_PARALLEL_FETCHES: usize = 6;
/// Downloaded artwork kept on disk. Artwork URLs never change content, so
/// a large budget means covers seldom download twice.
const DISK_BUDGET_BYTES: u64 = 300 * 1024 * 1024;
/// Edge of the blurred backdrop bitmap; drawn stretched, it stays smooth.
const BACKDROP_EDGE: u32 = 48;

/// What to make of a downloaded image.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Variant {
    /// Downscaled to (at least) this edge in device pixels.
    Sized(u32),
    /// Tiny and blurred, for backgrounds.
    Backdrop,
}

enum Entry {
    Loading,
    Ready {
        image: Arc<RenderImage>,
        bytes: usize,
        last_used: u64,
        /// The edge it was decoded at, and whether the source was larger.
        edge: u32,
        downscaled: bool,
        /// A larger copy is being made.
        upgrading: bool,
        /// Average color, for tinting around it, and how light the image
        /// is (0 black to 1 white).
        color: Hsla,
        luma: f32,
    },
    Failed,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    url: SharedString,
    backdrop: bool,
}

pub struct ImageCache {
    entries: HashMap<Key, Entry>,
    used_bytes: usize,
    clock: u64,
    permits: Arc<Semaphore>,
}

impl Global for ImageCache {}

struct Decoded {
    image: Arc<RenderImage>,
    bytes: usize,
    edge: u32,
    downscaled: bool,
    color: Hsla,
    luma: f32,
}

/// A blurred backdrop: the bitmap, a glow color drawn from it, and its
/// lightness (0 black to 1 white), to pick how much to darken it by.
pub struct Backdrop {
    pub image: Arc<RenderImage>,
    pub glow: Hsla,
    pub luma: f32,
}

impl ImageCache {
    pub fn init(cx: &mut App) {
        cx.set_global(ImageCache {
            entries: HashMap::new(),
            used_bytes: 0,
            clock: 0,
            permits: Arc::new(Semaphore::new(MAX_PARALLEL_FETCHES)),
        });
        // Keep the disk cache inside its budget, away from the UI thread.
        runtime::spawn(async { tokio::task::spawn_blocking(trim_disk).await.ok() });
    }

    /// Decoded image for `url`, kicking off a fetch on first use.
    ///
    /// `edge` is the size in device pixels the image will be drawn at; the
    /// decoded bitmap is never kept much larger than that.
    pub fn get(url: &SharedString, edge: u32, cx: &mut App) -> Option<Arc<RenderImage>> {
        let edge = edge.clamp(32, MAX_EDGE);
        Self::lookup(url, Variant::Sized(edge), cx).map(|(image, _, _)| image)
    }

    /// A small blurred copy of `url` and its average color, for a backdrop
    /// that follows the artwork.
    pub fn backdrop(url: &SharedString, cx: &mut App) -> Option<Backdrop> {
        Self::lookup(url, Variant::Backdrop, cx).map(|(image, glow, luma)| Backdrop {
            image,
            glow,
            luma,
        })
    }

    fn lookup(
        url: &SharedString,
        variant: Variant,
        cx: &mut App,
    ) -> Option<(Arc<RenderImage>, Hsla, f32)> {
        let key = Key {
            url: url.clone(),
            backdrop: variant == Variant::Backdrop,
        };
        let cache = cx.global_mut::<ImageCache>();
        cache.clock += 1;
        let clock = cache.clock;
        let found = match cache.entries.get_mut(&key) {
            Some(Entry::Ready {
                image,
                last_used,
                edge,
                downscaled,
                upgrading,
                color,
                luma,
                ..
            }) => {
                *last_used = clock;
                let wanted = match variant {
                    Variant::Sized(wanted) => wanted,
                    Variant::Backdrop => 0,
                };
                // Decoded smaller than it is drawn now: make a larger copy,
                // showing this one meanwhile.
                let upgrade = wanted > *edge && *downscaled && !*upgrading;
                if upgrade {
                    *upgrading = true;
                }
                let found = Some((image.clone(), *color, *luma));
                if !upgrade {
                    return found;
                }
                found
            }
            Some(Entry::Loading | Entry::Failed) => return None,
            None => {
                cache.entries.insert(key.clone(), Entry::Loading);
                None
            }
        };
        Self::fetch(key, variant, cx);
        found
    }

    fn fetch(key: Key, variant: Variant, cx: &mut App) {
        let permits = cx.global::<ImageCache>().permits.clone();
        cx.spawn(async move |cx| {
            let url = key.url.clone();
            let result = runtime::run(async move {
                let _permit = permits.acquire_owned().await?;
                let bytes = download(&url).await?;
                tokio::task::spawn_blocking(move || decode(&bytes, variant)).await?
            })
            .await;

            cx.update(|cx| {
                let cache = cx.global_mut::<ImageCache>();
                let old = cache.entries.remove(&key);
                let mut dropped = None;
                let entry = match (result, old) {
                    (Ok(decoded), old) => {
                        if let Some(Entry::Ready { image, bytes, .. }) = old {
                            cache.used_bytes -= bytes;
                            dropped = Some(image);
                        }
                        cache.used_bytes += decoded.bytes;
                        cache.clock += 1;
                        Entry::Ready {
                            image: decoded.image,
                            bytes: decoded.bytes,
                            last_used: cache.clock,
                            edge: decoded.edge,
                            downscaled: decoded.downscaled,
                            upgrading: false,
                            color: decoded.color,
                            luma: decoded.luma,
                        }
                    }
                    // A failed upgrade keeps the copy it had.
                    (
                        Err(_),
                        Some(Entry::Ready {
                            image,
                            bytes,
                            last_used,
                            edge,
                            color,
                            luma,
                            ..
                        }),
                    ) => Entry::Ready {
                        image,
                        bytes,
                        last_used,
                        edge,
                        downscaled: false,
                        upgrading: false,
                        color,
                        luma,
                    },
                    (Err(_), _) => Entry::Failed,
                };
                cache.entries.insert(key, entry);
                if let Some(image) = dropped {
                    cx.drop_image(image, None);
                }
                Self::evict(cx);
                cx.refresh_windows();
            });
        })
        .detach();
    }

    /// Drop least-recently-used images until we are back under budget.
    fn evict(cx: &mut App) {
        let cache = cx.global_mut::<ImageCache>();
        if cache.used_bytes <= BUDGET_BYTES {
            return;
        }
        let mut ready: Vec<(u64, Key)> = cache
            .entries
            .iter()
            .filter_map(|(key, entry)| match entry {
                Entry::Ready { last_used, .. } => Some((*last_used, key.clone())),
                _ => None,
            })
            .collect();
        ready.sort_unstable_by_key(|(used, _)| *used);

        let mut dropped = Vec::new();
        for (_, key) in ready {
            if cache.used_bytes <= BUDGET_BYTES * 3 / 4 {
                break;
            }
            if let Some(Entry::Ready { image, bytes, .. }) = cache.entries.remove(&key) {
                cache.used_bytes -= bytes;
                dropped.push(image);
            }
        }
        for image in dropped {
            cx.drop_image(image, None);
        }
    }

    pub fn used_bytes(cx: &App) -> usize {
        cx.global::<ImageCache>().used_bytes
    }

    /// Forget everything, e.g. on logout.
    pub fn clear(cx: &mut App) {
        let cache = cx.global_mut::<ImageCache>();
        cache.used_bytes = 0;
        let images: Vec<_> = cache
            .entries
            .drain()
            .filter_map(|(_, entry)| match entry {
                Entry::Ready { image, .. } => Some(image),
                _ => None,
            })
            .collect();
        for image in images {
            cx.drop_image(image, None);
        }
        let _ = std::fs::remove_dir_all(disk_dir());
    }
}

fn disk_dir() -> PathBuf {
    crate::paths::cache_dir().join("images")
}

/// The file `url` is kept in: Spotify's image ids (40 hex digits at the
/// end of i.scdn.co URLs) are unique and name it as they are; anything
/// else (generated playlist covers share their last segment) is named by a
/// hash of the whole URL.
fn disk_path(url: &str) -> PathBuf {
    let tail = url.rsplit('/').next().unwrap_or_default();
    let name = if tail.len() == 40 && tail.bytes().all(|b| b.is_ascii_hexdigit()) {
        tail.to_string()
    } else {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut hasher);
        format!("h{:016x}", hasher.finish())
    };
    disk_dir().join(name)
}

/// The image's bytes, from disk when it was fetched before.
async fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let path = disk_path(url);
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }
    let bytes = http()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec();
    // Best effort; a write that fails only costs a download next time.
    if tokio::fs::create_dir_all(disk_dir()).await.is_ok() {
        let partial = path.with_extension("part");
        if tokio::fs::write(&partial, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&partial, &path).await;
        }
    }
    Ok(bytes)
}

/// Remove the oldest files until the disk cache fits its budget.
fn trim_disk() {
    let Ok(dir) = std::fs::read_dir(disk_dir()) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = dir
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let used = meta.accessed().or_else(|_| meta.modified()).ok()?;
            Some((used, meta.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= DISK_BUDGET_BYTES {
        return;
    }
    files.sort_unstable_by_key(|(used, _, _)| *used);
    for (_, len, path) in files {
        if total <= DISK_BUDGET_BYTES * 3 / 4 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// Decode, downscale and convert to the BGRA layout the renderer wants.
fn decode(bytes: &[u8], variant: Variant) -> anyhow::Result<Decoded> {
    let decoded = image::load_from_memory(bytes)?;
    let source_edge = decoded.width().max(decoded.height());
    let (decoded, edge) = match variant {
        Variant::Sized(edge) if source_edge > edge => {
            (decoded.resize(edge, edge, FilterType::CatmullRom), edge)
        }
        Variant::Sized(_) => (decoded, source_edge),
        Variant::Backdrop => {
            let small = decoded.resize_exact(BACKDROP_EDGE, BACKDROP_EDGE, FilterType::Triangle);
            (small.blur(3.5), BACKDROP_EDGE)
        }
    };
    let mut rgba: RgbaImage = decoded.into_rgba8();
    let (color, luma) = average(&rgba);
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let bytes = rgba.as_raw().len();
    let frames: SmallVec<[Frame; 1]> = SmallVec::from_elem(Frame::new(rgba), 1);
    Ok(Decoded {
        image: Arc::new(RenderImage::new(frames)),
        bytes,
        edge,
        downscaled: source_edge > edge,
        color,
        luma,
    })
}

/// The image's average color, made vivid enough to glow with, and its
/// average lightness.
fn average(rgba: &RgbaImage) -> (Hsla, f32) {
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    // Every few pixels is plenty for an average.
    for pixel in rgba.pixels().step_by(7) {
        r += u64::from(pixel[0]);
        g += u64::from(pixel[1]);
        b += u64::from(pixel[2]);
        n += 1;
    }
    let n = n.max(1) as f32 * 255.0;
    let luma = 0.2126 * (r as f32 / n) + 0.7152 * (g as f32 / n) + 0.0722 * (b as f32 / n);
    let color: Hsla = Rgba {
        r: r as f32 / n,
        g: g as f32 / n,
        b: b as f32 / n,
        a: 1.0,
    }
    .into();
    let glow = Hsla {
        s: (color.s * 1.4).clamp(0.35, 0.9),
        l: color.l.clamp(0.45, 0.7),
        ..color
    };
    (glow, luma)
}

#[cfg(test)]
mod tests {
    // Named imports: a glob would pull in GPUI's `test` attribute and shadow the built-in one.
    use super::{Variant, decode};
    use image::RgbaImage;

    #[test]
    fn disk_names_do_not_collide() {
        let a =
            super::disk_path("https://pickasso.spotifycdn.com/image/x/dt/v1/img/thisisv3/AAA/en");
        let b =
            super::disk_path("https://pickasso.spotifycdn.com/image/x/dt/v1/img/thisisv3/BBB/en");
        assert_ne!(a, b);
        let id = "ab67616d0000b2732794743c6eb13addc2b01420";
        assert!(super::disk_path(&format!("https://i.scdn.co/image/{id}")).ends_with(id));
    }

    #[test]
    fn decode_downscales_and_swaps_channels() {
        let mut source = RgbaImage::new(128, 64);
        for pixel in source.pixels_mut() {
            *pixel = image::Rgba([255, 0, 0, 255]);
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let decoded = decode(&png, Variant::Sized(32)).unwrap();
        let (image, bytes) = (decoded.image, decoded.bytes);
        assert!(decoded.downscaled);
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (32, 16));
        assert_eq!(bytes, 32 * 16 * 4);
        // Red in RGBA becomes the third byte in BGRA.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
    }
}
