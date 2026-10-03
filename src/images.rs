//! Artwork cache.
//!
//! Covers are fetched and decoded off the UI thread, downscaled to the size
//! they are drawn at, and kept in a byte-budgeted LRU. Evicted images also
//! release their GPU atlas texture, so memory stays flat no matter how long
//! the app runs or how many pages are visited.

use std::collections::HashMap;
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
const MAX_EDGE: u32 = 640;
const MAX_PARALLEL_FETCHES: usize = 6;

enum Entry {
    Loading,
    Ready {
        image: Arc<RenderImage>,
        bytes: usize,
        last_used: u64,
    },
    Failed,
}

pub struct ImageCache {
    entries: HashMap<SharedString, Entry>,
    used_bytes: usize,
    clock: u64,
    permits: Arc<Semaphore>,
}

impl Global for ImageCache {}

impl ImageCache {
    pub fn init(cx: &mut App) {
        cx.set_global(ImageCache {
            entries: HashMap::new(),
            used_bytes: 0,
            clock: 0,
            permits: Arc::new(Semaphore::new(MAX_PARALLEL_FETCHES)),
        });
    }

    /// Decoded image for `url`, kicking off a fetch on first use.
    ///
    /// `edge` is the size in device pixels the image will be drawn at; the
    /// decoded bitmap is never kept larger than that.
    pub fn get(url: &SharedString, edge: u32, cx: &mut App) -> Option<Arc<RenderImage>> {
        let cache = cx.global_mut::<ImageCache>();
        cache.clock += 1;
        let clock = cache.clock;
        match cache.entries.get_mut(url) {
            Some(Entry::Ready {
                image, last_used, ..
            }) => {
                *last_used = clock;
                return Some(image.clone());
            }
            Some(Entry::Loading | Entry::Failed) => return None,
            None => {}
        }

        cache.entries.insert(url.clone(), Entry::Loading);
        let permits = cache.permits.clone();
        let url = url.clone();
        let edge = edge.clamp(32, MAX_EDGE);

        cx.spawn(async move |cx| {
            let fetch_url = url.clone();
            let result = runtime::run(async move {
                let _permit = permits.acquire_owned().await?;
                let bytes = http()
                    .get(fetch_url.as_ref())
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await?;
                tokio::task::spawn_blocking(move || decode(&bytes, edge)).await?
            })
            .await;

            cx.update(|cx| {
                let entry = match result {
                    Ok((image, bytes)) => {
                        let cache = cx.global_mut::<ImageCache>();
                        cache.used_bytes += bytes;
                        cache.clock += 1;
                        Entry::Ready {
                            image,
                            bytes,
                            last_used: cache.clock,
                        }
                    }
                    Err(_) => Entry::Failed,
                };
                cx.global_mut::<ImageCache>().entries.insert(url, entry);
                Self::evict(cx);
                cx.refresh_windows();
            });
        })
        .detach();

        None
    }

    /// Drop least-recently-used images until we are back under budget.
    fn evict(cx: &mut App) {
        let cache = cx.global_mut::<ImageCache>();
        if cache.used_bytes <= BUDGET_BYTES {
            return;
        }
        let mut ready: Vec<(u64, SharedString)> = cache
            .entries
            .iter()
            .filter_map(|(url, entry)| match entry {
                Entry::Ready { last_used, .. } => Some((*last_used, url.clone())),
                _ => None,
            })
            .collect();
        ready.sort_unstable_by_key(|(used, _)| *used);

        let mut dropped = Vec::new();
        for (_, url) in ready {
            if cache.used_bytes <= BUDGET_BYTES * 3 / 4 {
                break;
            }
            if let Some(Entry::Ready { image, bytes, .. }) = cache.entries.remove(&url) {
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
    }
}

/// Decode, downscale and convert to the BGRA layout the renderer wants.
fn decode(bytes: &[u8], edge: u32) -> anyhow::Result<(Arc<RenderImage>, usize)> {
    let decoded = image::load_from_memory(bytes)?;
    let decoded = if decoded.width().max(decoded.height()) > edge {
        decoded.resize(edge, edge, FilterType::Triangle)
    } else {
        decoded
    };
    let mut rgba: RgbaImage = decoded.into_rgba8();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let bytes = rgba.as_raw().len();
    let frames: SmallVec<[Frame; 1]> = SmallVec::from_elem(Frame::new(rgba), 1);
    Ok((Arc::new(RenderImage::new(frames)), bytes))
}

#[cfg(test)]
mod tests {
    // Named imports: a glob would pull in GPUI's `test` attribute and shadow the built-in one.
    use super::decode;
    use image::RgbaImage;

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

        let (image, bytes) = decode(&png, 32).unwrap();
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (32, 16));
        assert_eq!(bytes, 32 * 16 * 4);
        // Red in RGBA becomes the third byte in BGRA.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
    }
}
