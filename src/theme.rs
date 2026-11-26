use gtk::prelude::*;

const LIGHT: &str = "
@define-color md_primary #70549f;
@define-color md_on_primary #ffffff;
@define-color md_primary_container #ebddff;
@define-color md_on_primary_container #271243;
@define-color md_secondary_container #e8def5;
@define-color md_on_secondary_container #261c35;
@define-color md_tertiary_container #ffd8ea;
@define-color md_on_tertiary_container #361226;
@define-color md_surface #fcf8ff;
@define-color md_surface_low #f4eff8;
@define-color md_stage #f4eff8;
@define-color md_glow transparent;
@define-color md_surface_container #eee8f3;
@define-color md_surface_high #e7e1ed;
@define-color md_on_surface #201b27;
@define-color md_on_surface_variant #494252;
@define-color md_outline #7c7387;
@define-color md_outline_variant #cec4d9;
@define-color md_inverse_surface #332d3a;
@define-color md_inverse_on_surface #f4eff8;
@define-color md_error #ba1a1a;
";

const DARK: &str = "
@define-color md_primary #c8b5f8;
@define-color md_on_primary #342259;
@define-color md_primary_container #4b3970;
@define-color md_on_primary_container #ebddff;
@define-color md_secondary_container #494158;
@define-color md_on_secondary_container #e8def5;
@define-color md_tertiary_container #603b4f;
@define-color md_on_tertiary_container #ffd8ea;
@define-color md_surface #17151c;
@define-color md_surface_low #1d1a23;
@define-color md_stage #1d1a23;
@define-color md_glow transparent;
@define-color md_surface_container #24202b;
@define-color md_surface_high #2e2937;
@define-color md_on_surface #e9e2ef;
@define-color md_on_surface_variant #c5bcd3;
@define-color md_outline #a59ab2;
@define-color md_outline_variant #494252;
@define-color md_inverse_surface #e9e2ef;
@define-color md_inverse_on_surface #332d3a;
@define-color md_error #ffb4ab;
";

thread_local! {
    static THEME: std::cell::OnceCell<Theme> = const { std::cell::OnceCell::new() };
}

struct Theme {
    provider: gtk::CssProvider,
    settings: gtk::Settings,
    tint: gtk::CssProvider,
    hue: std::cell::Cell<Option<f64>>,
    dynamic: std::cell::Cell<bool>,
    motion: std::cell::Cell<bool>,
}

pub(crate) fn install(screen: &gtk::gdk::Screen) {
    THEME.with(|theme| {
        theme.get_or_init(|| {
            let provider = gtk::CssProvider::new();
            let settings = gtk::Settings::for_screen(screen).expect("GTK screen settings");
            gtk::StyleContext::add_provider_for_screen(
                screen,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            settings.connect_gtk_theme_name_notify(|_| refresh());
            settings.connect_notify_local(Some("gtk-enable-animations"), |_, _| refresh());
            settings.connect_gtk_application_prefer_dark_theme_notify(|_| refresh());
            let tint = gtk::CssProvider::new();
            gtk::StyleContext::add_provider_for_screen(
                screen,
                &tint,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
            Theme {
                provider,
                tint,
                settings,
                hue: std::cell::Cell::new(None),
                dynamic: std::cell::Cell::new(true),
                motion: std::cell::Cell::new(true),
            }
        });
    });
    refresh();
}

pub(crate) fn set_hue(hue: Option<f64>) {
    THEME.with(|theme| {
        if let Some(theme) = theme.get() {
            if theme.hue.replace(hue) != hue && theme.dynamic.get() {
                refresh_tint(theme);
            }
        }
    });
}

pub(crate) fn set_dynamic(enabled: bool) {
    THEME.with(|theme| {
        if let Some(theme) = theme.get() {
            if theme.dynamic.replace(enabled) != enabled {
                refresh_tint(theme);
            }
        }
    });
}

pub(crate) fn set_motion(enabled: bool) {
    let changed = THEME.with(|theme| {
        theme
            .get()
            .is_some_and(|theme| theme.motion.replace(enabled) != enabled)
    });
    if changed {
        refresh();
    }
}

#[cfg(test)]
pub(crate) fn set_cover(path: Option<&std::path::Path>) {
    set_hue(
        path.and_then(|path| gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, 48, 48, true).ok())
            .as_ref()
            .and_then(cover_hue),
    );
}

fn dark_scheme(settings: &gtk::Settings) -> bool {
    match std::env::var("GTK_THEME")
        .unwrap_or_default()
        .rsplit_once(':')
    {
        Some((_, "dark")) => true,
        Some((_, "light")) => false,
        _ => {
            settings.is_gtk_application_prefer_dark_theme()
                || settings
                    .gtk_theme_name()
                    .is_some_and(|name| name.to_ascii_lowercase().contains("dark"))
        }
    }
}

fn refresh() {
    THEME.with(|theme| {
        let Some(theme) = theme.get() else { return };
        let baseline = if dark_scheme(&theme.settings) {
            DARK
        } else {
            LIGHT
        };
        let motion =
            if theme.motion.get() && theme.settings.property::<bool>("gtk-enable-animations") {
                ""
            } else {
                "* { transition: none; }"
            };
        let css = format!("{baseline}{}{motion}", include_str!("style.css"));
        if let Err(error) = theme.provider.load_from_data(css.as_bytes()) {
            eprintln!("Не удалось загрузить стиль: {error}");
        }
        refresh_tint(theme);
    });
}

fn refresh_tint(theme: &Theme) {
    let css = if let Some(hue) = theme.hue.get().filter(|_| theme.dynamic.get()) {
        let tone = if dark_scheme(&theme.settings) {
            28.0
        } else {
            82.0
        };
        let [r, g, b] = tonal_rgb(tone, 24.0, hue);
        let [gr, gg, gb] = tonal_rgb(
            if dark_scheme(&theme.settings) {
                66.0
            } else {
                58.0
            },
            34.0,
            hue,
        );
        let opacity = if dark_scheme(&theme.settings) {
            0.16
        } else {
            0.10
        };
        format!("@define-color md_stage mix(@md_surface_low, #{r:02x}{g:02x}{b:02x}, 0.26); @define-color md_glow alpha(#{gr:02x}{gg:02x}{gb:02x}, {opacity});")
    } else {
        String::new()
    };
    if let Err(error) = theme.tint.load_from_data(css.as_bytes()) {
        eprintln!("Не удалось загрузить цвет обложки: {error}");
    }
}

pub(crate) fn cover_hue(cover: &gtk::gdk_pixbuf::Pixbuf) -> Option<f64> {
    let bytes = cover.read_pixel_bytes();
    let channels = cover.n_channels() as usize;
    let mut bins = [(0.0, 0.0, 0.0); 24];
    for row in bytes.as_ref().chunks(cover.rowstride() as usize) {
        for pixel in row[..cover.width() as usize * channels].chunks_exact(channels) {
            if channels == 4 && pixel[3] < 128 {
                continue;
            }
            let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(|v| {
                let v = f64::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            });
            let x = ((0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047).cbrt();
            let y = (0.2126729 * r + 0.7151522 * g + 0.0721750 * b).cbrt();
            let z = ((0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883).cbrt();
            let a = 500.0 * (x - y);
            let b = 200.0 * (y - z);
            let chroma = a.hypot(b);
            if chroma < 8.0 || y < 0.24 {
                continue;
            }
            let hue = a.atan2(b).rem_euclid(std::f64::consts::TAU);
            let index = (hue * 24.0 / std::f64::consts::TAU).round() as usize % 24;
            let bin = &mut bins[index];
            let weight = chroma.min(50.0).sqrt();
            bin.0 += weight;
            bin.1 += hue.sin() * weight;
            bin.2 += hue.cos() * weight;
        }
    }
    let (weight, sin, cos) = bins.into_iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
    (weight > 0.0).then(|| sin.atan2(cos))
}

fn tonal_rgb(tone: f64, chroma: f64, hue: f64) -> [u8; 3] {
    let y = (tone + 16.0) / 116.0;
    let linear = |v: f64| {
        if v > 6.0 / 29.0 {
            v.powi(3)
        } else {
            (116.0 * v - 16.0) / 903.3
        }
    };
    let mut chroma = chroma;
    loop {
        let x = 0.95047 * linear(y + chroma * hue.sin() / 500.0);
        let z = 1.08883 * linear(y - chroma * hue.cos() / 200.0);
        let y = linear(y);
        let rgb = [
            3.2404542 * x - 1.5371385 * y - 0.4985314 * z,
            -0.9692660 * x + 1.8760108 * y + 0.0415560 * z,
            0.0556434 * x - 0.2040259 * y + 1.0572252 * z,
        ];
        if chroma < 0.5 || rgb.iter().all(|v| (0.0..=1.0).contains(v)) {
            return rgb.map(|v| {
                let v = v.clamp(0.0, 1.0);
                let srgb = if v <= 0.0031308 {
                    12.92 * v
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                };
                (srgb * 255.0).round() as u8
            });
        }
        chroma *= 0.8;
    }
}

/// Cache a tiny diffuse texture when its color changes. Do not retain a
/// window-sized CSS gradient surface or run a separate animation source.
pub(crate) fn attach_cover_glow(widget: &impl IsA<gtk::Widget>) {
    use std::{cell::RefCell, rc::Rc};
    let texture = Rc::new(RefCell::new(None::<gtk::cairo::ImageSurface>));
    let color = Rc::new(RefCell::new(None::<gtk::gdk::RGBA>));
    let refresh = {
        let texture = texture.clone();
        let color = color.clone();
        move |widget: &gtk::Widget| {
            let next = widget.style_context().lookup_color("md_glow");
            if *color.borrow() == next {
                return;
            }
            *color.borrow_mut() = next;
            let surface = next.filter(|c| c.alpha() > 0.0).and_then(|color| {
                let surface =
                    gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 64, 64).ok()?;
                let cr = gtk::cairo::Context::new(&surface).ok()?;
                let glow = gtk::cairo::RadialGradient::new(32.0, 24.0, 0.0, 32.0, 24.0, 40.0);
                for (offset, alpha) in [(0.0, 1.0), (0.5, 0.45), (1.0, 0.0)] {
                    glow.add_color_stop_rgba(
                        offset,
                        color.red(),
                        color.green(),
                        color.blue(),
                        color.alpha() * alpha,
                    );
                }
                cr.set_source(&glow).ok()?;
                cr.paint().ok()?;
                Some(surface)
            });
            texture.replace(surface);
            widget.queue_draw();
        }
    };
    let widget = widget.upcast_ref::<gtk::Widget>();
    refresh(widget);
    widget.connect_style_updated(move |widget| refresh(widget));
    widget.connect_draw(move |widget, cr| {
        if let Some(surface) = texture.borrow().as_ref() {
            let _ = cr.save();
            cr.scale(
                f64::from(widget.allocated_width()) / 64.0,
                f64::from(widget.allocated_height()) / 64.0,
            );
            if cr.set_source_surface(surface, 0.0, 0.0).is_ok() {
                cr.source().set_filter(gtk::cairo::Filter::Bilinear);
                let _ = cr.paint();
            }
            let _ = cr.restore();
        }
        glib::Propagation::Proceed
    });
}
