use gtk::prelude::*;
use gtk::{
    gdk::prelude::GdkPixbufExt,
    gdk_pixbuf::{Colorspace, InterpType, Pixbuf},
    gio,
};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Current(i32),
    Queue,
}

const HERO_PIXELS: i32 = 752;

type Key = (PathBuf, Kind);
type CacheEntry = (Key, Option<Rc<Artwork>>);
use crate::UiUpdates;

#[derive(Default)]
struct Cache {
    current: VecDeque<CacheEntry>,
    queue: VecDeque<CacheEntry>,
    pending: HashMap<Key, Vec<Weak<UiUpdates>>>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

pub(crate) struct Artwork {
    pub hero: Option<HeroImage>,
    pub mini: Option<Pixbuf>,
    pub thumb: Pixbuf,
    pub backdrop: Option<Pixbuf>,
    pub hue: Option<f64>,
}

#[derive(Clone)]
pub(crate) struct HeroImage {
    pub surface: gtk::cairo::Surface,
    pub edge: f64,
}

struct Decoded {
    hero: Option<Pixels>,
    mini: Option<Pixels>,
    thumb: Pixels,
    backdrop: Option<Pixels>,
    hue: Option<f64>,
}

struct Pixels {
    bytes: glib::Bytes,
    size: i32,
    stride: i32,
}

impl Pixels {
    fn pixbuf(self) -> Pixbuf {
        Pixbuf::from_bytes(
            &self.bytes,
            Colorspace::Rgb,
            true,
            8,
            self.size,
            self.size,
            self.stride,
        )
    }
}

impl Cache {
    fn entries(&mut self, kind: Kind) -> &mut VecDeque<CacheEntry> {
        match kind {
            Kind::Current(_) => &mut self.current,
            Kind::Queue => &mut self.queue,
        }
    }
}

// Outer None means pending. Inner None means the path is absent or failed to decode.
pub(crate) fn get(
    path: Option<&Path>,
    current: bool,
    updates: &Rc<UiUpdates>,
) -> Option<Option<Rc<Artwork>>> {
    let Some(path) = path else { return Some(None) };
    let pixels = hero_pixels();
    let kind = if current {
        Kind::Current(pixels)
    } else {
        Kind::Queue
    };
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache
            .current
            .iter()
            .position(|(key, _)| key.0 == path && (!current || key.1 == kind))
        {
            let entry = cache.current.remove(index).unwrap();
            let result = entry.1.clone();
            cache.current.push_front(entry);
            return Some(result);
        }
        let entries = cache.entries(kind);
        if let Some(index) = entries
            .iter()
            .position(|(key, _)| key.0 == path && key.1 == kind)
        {
            let entry = entries.remove(index).unwrap();
            let result = entry.1.clone();
            entries.push_front(entry);
            return Some(result);
        }
        // Prefer a full-size decode already in flight over a second thumbnail decode.
        let key = (path.to_owned(), kind);
        let preferred = (key.0.clone(), Kind::Current(pixels));
        let pending_key = if cache.pending.contains_key(&preferred) {
            &preferred
        } else {
            &key
        };
        let waiter = Rc::downgrade(updates);
        if let Some(waiters) = cache.pending.get_mut(pending_key) {
            if !waiters.iter().any(|previous| previous.ptr_eq(&waiter)) {
                waiters.push(waiter);
            }
            return None;
        }
        // Reserve capacity for the selected track when the queue is scrolling.
        if cache.pending.len() >= if current { 6 } else { 4 } {
            // One completion is enough to try admitting the work again.
            if let Some(waiters) = cache.pending.values_mut().next() {
                if !waiters.iter().any(|previous| previous.ptr_eq(&waiter)) {
                    waiters.push(waiter);
                }
            }
            return None;
        }
        cache.pending.insert(key.clone(), vec![waiter]);
        let source = key.0.clone();
        let task = gio::spawn_blocking(move || decode(&source, kind));
        glib::MainContext::default().spawn_local(async move {
            let result = task.await.ok().flatten().map(|image| {
                Rc::new(Artwork {
                    hero: image.hero.and_then(|pixels| {
                        let edge = f64::from(pixels.size);
                        let surface = pixels
                            .pixbuf()
                            .create_surface(1, None::<&gtk::gdk::Window>)?;
                        Some(HeroImage { surface, edge })
                    }),
                    mini: image.mini.map(Pixels::pixbuf),
                    thumb: image.thumb.pixbuf(),
                    backdrop: image.backdrop.map(Pixels::pixbuf),
                    hue: image.hue,
                })
            });
            CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if let Some(waiters) = cache.pending.remove(&key) {
                    for updates in waiters.into_iter().filter_map(|waiter| waiter.upgrade()) {
                        updates.request();
                    }
                }
                let entries = cache.entries(kind);
                entries.push_front((key, result));
                entries.truncate(if matches!(kind, Kind::Current(_)) {
                    4
                } else {
                    128
                });
            });
        });
        None
    })
}

fn hero_pixels_for_scale(scale: i32) -> i32 {
    (384 * scale.clamp(1, 2)).min(HERO_PIXELS)
}

// Ask GTK on its main thread, then pass a plain pixel limit to decoding. Keep
// both densities in the bounded cache and re-decode after moving to HiDPI.
pub(crate) fn hero_pixels() -> i32 {
    let scale = gtk::Window::list_toplevels()
        .iter()
        .filter(|window| window.is_mapped())
        .map(|window| window.scale_factor())
        .max()
        .unwrap_or(1);
    hero_pixels_for_scale(scale)
}

pub(crate) const QUEUE_THUMB_PIXELS: i32 = 40;

fn decode(path: &Path, kind: Kind) -> Option<Decoded> {
    let limit = match kind {
        Kind::Current(pixels) => pixels,
        Kind::Queue => QUEUE_THUMB_PIXELS,
    };
    let (width, height) = Pixbuf::file_info(path).map(|(_, width, height)| (width, height))?;
    let shortest = width.min(height);
    if shortest <= 0 {
        return None;
    }
    let longest = width.max(height);
    // Keep wide or tall artwork bounded even when its short edge is already small.
    let ratio = (f64::from(limit) / f64::from(shortest))
        .min(f64::from(limit * 4) / f64::from(longest))
        .min(1.0);
    let pixbuf = Pixbuf::from_file_at_scale(
        path,
        (f64::from(width) * ratio).ceil() as i32,
        (f64::from(height) * ratio).ceil() as i32,
        true,
    )
    .ok()?;
    let current = matches!(kind, Kind::Current(_));
    let hue = current
        .then(|| {
            let ratio = 48.0 / f64::from(pixbuf.width().max(pixbuf.height()));
            let small = pixbuf.scale_simple(
                (f64::from(pixbuf.width()) * ratio).round().max(1.0) as i32,
                (f64::from(pixbuf.height()) * ratio).round().max(1.0) as i32,
                InterpType::Bilinear,
            )?;
            super::theme::cover_hue(&small)
        })
        .flatten();
    Some(Decoded {
        hero: current
            .then(|| rounded(&pixbuf, pixbuf.width().min(pixbuf.height()), 0.0))
            .flatten(),
        mini: current.then(|| rounded(&pixbuf, 48, 8.0)).flatten(),
        thumb: rounded(&pixbuf, QUEUE_THUMB_PIXELS, 8.0)?,
        backdrop: current.then(|| rounded(&pixbuf, 32, 0.0)).flatten(),
        hue,
    })
}

fn rounded(pixbuf: &Pixbuf, size: i32, radius: f64) -> Option<Pixels> {
    let square = pixbuf.width().min(pixbuf.height());
    if square <= 0 {
        return None;
    }
    let x = (pixbuf.width() - square) / 2;
    let y = (pixbuf.height() - square) / 2;
    let cropped = pixbuf.new_subpixbuf(x, y, square, square);
    // The common square cover is already decoded at the requested size.
    // Re-scaling it would allocate another full-size RGB/RGBA buffer.
    // Keep the copy for subrectangles so their larger parent stride is not retained.
    let scaled = if pixbuf.width() == size && pixbuf.height() == size {
        pixbuf.clone()
    } else {
        cropped.scale_simple(size, size, InterpType::Bilinear)?
    };
    let scaled = if scaled.has_alpha() {
        scaled
    } else {
        scaled.add_alpha(false, 0, 0, 0).ok()?
    };
    let stride = scaled.rowstride() as usize;
    let bytes = scaled.read_pixel_bytes();
    if radius == 0.0 {
        return Some(Pixels {
            bytes,
            size,
            stride: stride as i32,
        });
    }
    let mut data = bytes.as_ref().to_vec();
    let edge = size as f64;
    let radius = radius.min(edge / 2.0);
    let corner = radius.ceil() as i32;
    for y in 0..corner {
        for x in 0..corner {
            let dx = (radius - f64::from(x) - 0.5).max(0.0);
            let dy = (radius - f64::from(y) - 0.5).max(0.0);
            let alpha = (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
            if alpha < 1.0 {
                for py in [y, size - 1 - y] {
                    for px in [x, size - 1 - x] {
                        let offset = py as usize * stride + px as usize * 4 + 3;
                        data[offset] = (f64::from(data[offset]) * alpha) as u8;
                    }
                }
            }
        }
    }
    Some(Pixels {
        bytes: glib::Bytes::from_owned(data),
        size,
        stride: stride as i32,
    })
}
