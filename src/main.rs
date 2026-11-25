#[cfg(test)]
use gtk::gdk_pixbuf::Colorspace;
use gtk::gdk_pixbuf::Pixbuf;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

mod artwork;
mod cover_art;
mod library;
mod lyrics;
mod motion;
mod mpv;
mod session;
mod shuffle;
mod soundcloud;
mod theme;
mod yandex_export;
use library::{collect_audio, extract_cover, preview_track, read_track};
use lyrics::online_lyrics;
use mpv::Mpv;
use session::Session;
use shuffle::ShuffleOrder;

const PAGE_NAMES: [&str; 3] = ["Плеер", "Очередь", "Настройки"];

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Track {
    path: PathBuf,
    title: String,
    artist: String,
    album: String,
    duration: f64,
    cover_path: Option<PathBuf>,
    lyrics: Vec<Lyric>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Lyric {
    time: Option<f64>,
    text: String,
}

struct Model {
    tracks: Vec<Track>,
    current: Option<usize>,
    library_revision: u64,
    cover_pending: std::collections::HashSet<PathBuf>,
    cover_attempted: std::collections::HashSet<PathBuf>,
    metadata_ready: std::collections::HashSet<PathBuf>,
    metadata_pending: std::collections::HashSet<PathBuf>,
    lyrics_pending: std::collections::HashSet<PathBuf>,
    lyrics_results: std::collections::HashMap<PathBuf, String>,
    shuffle: bool,
    shuffle_order: ShuffleOrder,
    play_next: Option<PathBuf>,
    repeat: usize,
    online_covers: bool,
    volume: f64,
    speed: f64,
    soundcloud_dir: PathBuf,
    status: Option<String>,
    imports_pending: usize,
    failed_tracks: std::collections::HashSet<usize>,
    mpv: Option<Mpv>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("ym-export") {
        if let Err(error) = yandex_export::run(&args[1..]) {
            eprintln!("Не удалось экспортировать плейлист: {error}");
            std::process::exit(1);
        }
        return;
    }
    let app = gtk::Application::new(Some("local.ownplayer.desktop"), Default::default());
    app.connect_startup(|_| {
        if let Some(screen) = gtk::gdk::Screen::default() {
            theme::install(&screen);
        }
    });
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &gtk::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let model = Rc::new(RefCell::new(Model {
        tracks: Vec::new(),
        current: None,
        library_revision: 0,
        cover_pending: std::collections::HashSet::new(),
        cover_attempted: Default::default(),
        metadata_ready: Default::default(),
        metadata_pending: Default::default(),
        lyrics_pending: Default::default(),
        lyrics_results: Default::default(),
        shuffle: false,
        shuffle_order: ShuffleOrder::default(),
        play_next: None,
        repeat: 0,
        online_covers: true,
        volume: 75.0,
        speed: 1.0,
        soundcloud_dir: soundcloud::default_download_dir(),
        status: None,
        imports_pending: 0,
        failed_tracks: Default::default(),
        mpv: None,
    }));
    restore_session(&model);
    let window = gtk::ApplicationWindow::new(app);
    window.set_title("Veyra");
    window.set_default_size(1160, 780);
    window.set_size_request(760, 480);
    window.set_position(gtk::WindowPosition::Center);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.set_border_width(16);

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.set_size_request(72, -1);
    sidebar.style_context().add_class("sidebar");

    let brand = gtk::Box::new(gtk::Orientation::Vertical, 0);
    brand.set_halign(gtk::Align::Center);
    brand.set_size_request(40, 40);
    brand.set_margin_bottom(16);
    brand.style_context().add_class("brand-mark");
    let brand_icon = symbolize("audio-x-generic-symbolic", 20);
    brand_icon.style_context().add_class("brand-icon");
    brand.set_center_widget(Some(&brand_icon));
    sidebar.pack_start(&brand, false, false, 0);

    let pages = gtk::Stack::new();
    pages.set_transition_type(gtk::StackTransitionType::Crossfade);
    pages.set_transition_duration(180);
    let page_buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));
    for (index, name) in PAGE_NAMES[..2].iter().enumerate() {
        let name = name.to_string();
        let icon = if index == 0 {
            "audio-x-generic-symbolic"
        } else {
            "view-list-symbolic"
        };
        let button = nav_button(icon, &name);
        let pages_clone = pages.clone();
        let all_buttons = Rc::downgrade(&page_buttons);
        button.connect_clicked(move |_| {
            if let Some(all_buttons) = all_buttons.upgrade() {
                set_page(&pages_clone, &all_buttons, index);
            }
        });
        sidebar.pack_start(&button, false, false, 0);
        page_buttons.borrow_mut().push(button);
    }
    if let Some(first) = page_buttons.borrow().first() {
        first.style_context().add_class("selected");
    }
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.pack_start(&spacer, true, true, 0);
    let add_files = action_button("document-open-symbolic", "Добавить файлы");
    let add_folder = action_button("folder-open-symbolic", "Добавить папку");
    let download = action_button("document-save-symbolic", "Скачать с SoundCloud");
    let settings = action_button("preferences-system-symbolic", "Настройки");
    sidebar.pack_start(&add_files, false, false, 0);
    sidebar.pack_start(&add_folder, false, false, 0);
    sidebar.pack_start(&download, false, false, 0);
    let sidebar_divider = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sidebar_divider.set_size_request(-1, 1);
    sidebar_divider.set_margin_top(8);
    sidebar_divider.set_margin_bottom(8);
    sidebar_divider.style_context().add_class("divider");
    sidebar.pack_start(&sidebar_divider, false, false, 0);
    sidebar.pack_start(&settings, false, false, 0);
    page_buttons.borrow_mut().push(settings.clone());

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = gtk::Box::new(gtk::Orientation::Vertical, 3);
    header.set_margin_start(8);
    let subheading = gtk::Label::new(None);
    subheading.set_xalign(0.0);
    subheading.set_line_wrap(true);
    subheading.set_no_show_all(true);
    subheading.style_context().add_class("secondary");
    header.pack_start(&subheading, false, false, 0);
    subheading.set_margin_bottom(10);
    content.pack_start(&header, false, false, 0);

    let (player_page, _) = build_player_page(&model);
    let queue_page = build_queue_page(&model);
    pages.add_named(&player_page, "Плеер");
    pages.add_named(&queue_page, "Очередь");
    pages.add_named(&build_settings_page(&model), "Настройки");
    content.pack_start(&pages, true, true, 0);
    let controls = build_controls(&model);

    let main_column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    main_column.pack_start(&content, true, true, 0);
    main_column.pack_start(&controls, false, false, 0);
    root.pack_start(&sidebar, false, false, 0);
    root.pack_start(&main_column, true, true, 0);
    window.add(&root);

    let model_files = model.clone();
    add_files.connect_clicked(move |_| {
        if let Some(files) = choose_files(false) {
            append_paths(&model_files, files);
        }
    });
    let model_folder = model.clone();
    add_folder.connect_clicked(move |_| {
        if let Some(files) = choose_files(true) {
            append_paths(&model_folder, files);
        }
    });
    let settings_pages = pages.downgrade();
    let settings_buttons = Rc::downgrade(&page_buttons);
    let previous_page = Rc::new(Cell::new(0));
    let previous = previous_page.clone();
    settings.connect_clicked(move |_| {
        if let (Some(pages), Some(buttons)) = (settings_pages.upgrade(), settings_buttons.upgrade())
        {
            let current = pages.visible_child_name();
            if current.as_deref() == Some(PAGE_NAMES[2]) {
                set_page(&pages, &buttons, previous.get());
            } else {
                previous.set(usize::from(current.as_deref() == Some(PAGE_NAMES[1])));
                set_page(&pages, &buttons, 2);
            }
        }
    });
    let settings_pages = pages.downgrade();
    let settings_buttons = Rc::downgrade(&page_buttons);
    window.connect_key_press_event(move |_, event| {
        if event.keyval() == gtk::gdk::keys::constants::Escape {
            if let (Some(pages), Some(buttons)) =
                (settings_pages.upgrade(), settings_buttons.upgrade())
            {
                if pages.visible_child_name().as_deref() == Some(PAGE_NAMES[2]) {
                    set_page(&pages, &buttons, previous_page.get());
                    return glib::Propagation::Stop;
                }
            }
        }
        glib::Propagation::Proceed
    });
    let model_download = model.clone();
    download.connect_clicked(move |_| show_soundcloud_dialog(&model_download));
    let model_close = model.clone();
    window.connect_delete_event(move |_, _| {
        save_session(&model_close);
        glib::Propagation::Proceed
    });

    let buttons_lifetime = page_buttons.clone();
    window.connect_destroy(move |_| buttons_lifetime.borrow_mut().clear());
    let model_count = model.clone();
    let subheading_update = subheading.clone();
    let alive = widget_lifetime(&window);
    let header_alive = alive.clone();
    glib::timeout_add_local(Duration::from_millis(400), move || {
        if !header_alive.get() {
            return glib::ControlFlow::Break;
        }
        let state = model_count.borrow();
        let text = if let Some(message) = &state.status {
            message.as_str()
        } else if state.imports_pending > 0 {
            "Добавляем музыку…"
        } else {
            ""
        };
        if subheading_update.text() != text {
            subheading_update.set_text(text);
            subheading_update.set_visible(!text.is_empty());
        }
        glib::ControlFlow::Continue
    });

    start_mpv(&model);
    let model_tick = model.clone();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        tick_mpv(&model_tick);
        start_lyrics_job(&model_tick, false);
        glib::ControlFlow::Continue
    });
    motion::animate_buttons(window.upcast_ref());
    window.show_all();
}

fn symbolize(name: &str, size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(Some(name), gtk::IconSize::Button);
    image.set_pixel_size(size);
    image
}

fn icon_button(icon: &str, tooltip: &str, class: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_image(Some(&symbolize(icon, 18)));
    describe_control(&button, tooltip);
    button.style_context().add_class(class);
    button
}

fn describe_control(widget: &impl IsA<gtk::Widget>, label: &str) {
    widget.set_tooltip_text(Some(label));
    if let Some(accessible) = widget.accessible() {
        accessible.set_name(label);
    }
}

fn nav_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.style_context().add_class("nav-item");
    button.set_focus_on_click(false);
    describe_control(&button, label);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.set_halign(gtk::Align::Center);
    let indicator = rail_indicator(icon);
    let text = gtk::Label::new(Some(label));
    column.pack_start(&indicator, false, false, 0);
    column.pack_start(&text, false, false, 0);
    button.add(&column);
    button
}

fn action_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.style_context().add_class("sidebar-action");
    button.set_focus_on_click(false);
    describe_control(&button, label);
    button.add(&rail_indicator(icon));
    button
}

fn rail_indicator(icon: &str) -> gtk::Box {
    let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    indicator.style_context().add_class("rail-indicator");
    indicator.set_size_request(56, 32);
    indicator.set_halign(gtk::Align::Center);
    indicator.set_valign(gtk::Align::Center);
    indicator.set_center_widget(Some(&symbolize(icon, 24)));
    indicator
}

fn cover_placeholder(size: i32, glyph: i32) -> gtk::Box {
    let area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    area.set_size_request(size, size);
    area.style_context().add_class("cover-placeholder");
    let icon = symbolize("audio-x-generic-symbolic", glyph);
    icon.style_context().add_class("cover-glyph");
    area.set_center_widget(Some(&icon));
    area
}

fn tracks_phrase(count: usize) -> String {
    let word = match count % 10 {
        1 if count % 100 != 11 => "трек",
        2..=4 if !(12..=14).contains(&(count % 100)) => "трека",
        _ => "треков",
    };
    format!("{count} {word}")
}

fn set_page(pages: &gtk::Stack, buttons: &Rc<RefCell<Vec<gtk::Button>>>, index: usize) {
    pages.set_visible_child_name(PAGE_NAMES[index]);
    for (position, button) in buttons.borrow().iter().enumerate() {
        if position == index {
            button.style_context().add_class("selected");
        } else {
            button.style_context().remove_class("selected");
        }
    }
}

fn build_player_page(model: &Rc<RefCell<Model>>) -> (gtk::Widget, gtk::Stack) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.style_context().add_class("player-stage");

    let views = gtk::Stack::new();
    views.set_transition_type(gtk::StackTransitionType::Crossfade);
    views.set_transition_duration(180);
    page.pack_start(&views, true, true, 0);

    const ARTWORK_SIZE: i32 = 288;
    const HERO_SPACING: i32 = 32;
    const HERO_INSET: i32 = 24;
    const DETAILS_MIN: i32 = 180;
    let artwork = gtk::Box::new(gtk::Orientation::Vertical, 0);
    artwork.set_size_request(
        ARTWORK_SIZE + HERO_SPACING + 2 * HERO_INSET + DETAILS_MIN,
        ARTWORK_SIZE + 32,
    );
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, HERO_SPACING);
    hero.set_valign(gtk::Align::Center);
    hero.set_margin_top(16);
    hero.set_margin_bottom(16);
    hero.set_margin_start(HERO_INSET);
    hero.set_margin_end(HERO_INSET);

    let cover_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    cover_frame.style_context().add_class("cover-frame");
    cover_frame.set_size_request(ARTWORK_SIZE, ARTWORK_SIZE);
    cover_frame.set_valign(gtk::Align::Center);
    let cover_stack = gtk::Stack::new();
    cover_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    cover_stack.set_transition_duration(180);
    cover_stack.add_named(&cover_placeholder(ARTWORK_SIZE, 84), "placeholder");
    let cover = gtk::Image::new();
    cover.set_size_request(ARTWORK_SIZE, ARTWORK_SIZE);
    cover_stack.add_named(&cover, "image");
    cover_stack.set_visible_child_name("placeholder");
    let cover_alt = gtk::Image::new();
    cover_alt.set_size_request(ARTWORK_SIZE, ARTWORK_SIZE);
    cover_stack.add_named(&cover_alt, "image-alt");
    cover_frame.pack_start(&cover_stack, true, true, 0);
    hero.pack_start(&cover_frame, false, false, 0);

    let details = gtk::Box::new(gtk::Orientation::Vertical, 8);
    details.set_valign(gtk::Align::Center);
    details.set_size_request(DETAILS_MIN, -1);
    details.style_context().add_class("track-details");
    let title = gtk::Label::new(Some("Добавь музыку"));
    title.set_xalign(0.0);
    title.set_line_wrap(true);
    title.set_lines(3);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(1);
    title.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
    title.style_context().add_class("track-title");
    let artist = gtk::Label::new(Some("Выбери файлы или добавь папку с музыкой"));
    artist.set_xalign(0.0);
    artist.set_line_wrap(true);
    artist.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
    artist.set_lines(2);
    artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    artist.set_max_width_chars(1);
    artist.style_context().add_class("track-artist");
    let album = gtk::Label::new(None);
    album.set_xalign(0.0);
    album.set_ellipsize(gtk::pango::EllipsizeMode::End);
    album.set_max_width_chars(1);
    album.set_no_show_all(true);
    album.style_context().add_class("track-album");
    let show_lyrics = gtk::Button::new();
    show_lyrics.style_context().add_class("lyrics-action");
    show_lyrics.set_focus_on_click(false);
    let lyrics_row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    lyrics_row.pack_start(&symbolize("format-text-rich-symbolic", 16), false, false, 0);
    lyrics_row.pack_start(&gtk::Label::new(Some("Показать текст")), false, false, 0);
    show_lyrics.add(&lyrics_row);
    show_lyrics.set_halign(gtk::Align::Start);
    show_lyrics.set_margin_top(16);
    let views_lyrics = views.downgrade();
    show_lyrics.connect_clicked(move |_| {
        if let Some(views) = views_lyrics.upgrade() {
            views.set_visible_child_name("lyrics");
        }
    });
    details.pack_start(&title, false, false, 0);
    details.pack_start(&artist, false, false, 0);
    details.pack_start(&album, false, false, 0);
    details.pack_start(&show_lyrics, false, false, 0);
    let start = gtk::Button::with_label("Добавить музыку");
    start.style_context().add_class("tonal-button");
    start.set_halign(gtk::Align::Start);
    start.set_margin_top(22);
    let model_start = model.clone();
    start.connect_clicked(move |_| {
        if let Some(paths) = choose_files(false) {
            append_paths(&model_start, paths);
        }
    });
    details.pack_start(&start, false, false, 0);
    start.show_all();
    show_lyrics.show_all();
    start.set_no_show_all(true);
    show_lyrics.set_no_show_all(true);
    start.set_visible(model.borrow().current.is_none());
    show_lyrics.set_visible(model.borrow().current.is_some());
    // The details column uses the space left after laying out the cover.
    hero.pack_start(&details, true, true, 0);
    artwork.pack_start(&hero, true, true, 0);
    views.add_named(&artwork, "artwork");
    views.add_named(&build_lyrics_view(model, &views), "lyrics");
    views.set_visible_child_name("artwork");
    let mapped = Rc::new(Cell::new(false));
    views.connect_map(move |stack| {
        if !mapped.replace(true) {
            stack.set_visible_child_name("artwork");
        }
    });

    let model_update = model.clone();
    let views_update = views.downgrade();
    let alive = widget_lifetime(&page);
    let mut rendered: Option<(PathBuf, String, String, String)> = None;
    let mut rendered_cover = None;
    glib::timeout_add_local(Duration::from_millis(120), move || {
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        let state = model_update.borrow();
        if let Some(index) = state.current {
            if let Some(track) = state.tracks.get(index) {
                if rendered
                    .as_ref()
                    .map(|(path, title, artist, album)| (path, title, artist, album))
                    != Some((&track.path, &track.title, &track.artist, &track.album))
                {
                    rendered = Some((
                        track.path.clone(),
                        track.title.clone(),
                        track.artist.clone(),
                        track.album.clone(),
                    ));
                    start.hide();
                    show_lyrics.show();
                    title.set_text(&track.title);
                    title.set_tooltip_text(Some(&track.title));
                    artist.set_text(&track.artist);
                    artist.set_tooltip_text(Some(&track.artist));
                    album.set_text(&track.album);
                    album.set_tooltip_text(Some(&track.album));
                    album.set_visible(!track.album.is_empty());
                }
                if let Some(pixbuf) =
                    current_artwork(track, &mut rendered_cover, &state.cover_pending)
                {
                    crossfade_cover(
                        &cover_stack,
                        &cover,
                        &cover_alt,
                        pixbuf.as_ref().and_then(|art| art.hero.as_ref()),
                    );
                }
            }
        }
        if state.current.is_none() && rendered.take().is_some() {
            title.set_text("Добавь музыку");
            title.set_tooltip_text(None);
            artist.set_text("Выбери файлы или добавь папку с музыкой");
            artist.set_tooltip_text(None);
            album.hide();
            show_lyrics.hide();
            start.show();
            rendered_cover = None;
            cover_stack.set_visible_child_name("placeholder");
            if let Some(views) = views_update.upgrade() {
                views.set_visible_child_name("artwork");
            }
        }
        glib::ControlFlow::Continue
    });
    (page.upcast(), views)
}

// Keep the current cover visible until lookup and decoding finish.
fn current_artwork(
    track: &Track,
    rendered: &mut Option<(Option<PathBuf>, Option<PathBuf>)>,
    pending: &std::collections::HashSet<PathBuf>,
) -> Option<Option<Rc<artwork::Artwork>>> {
    let missing_cover_track = track.cover_path.is_none().then_some(&track.path);
    if rendered
        .as_ref()
        .map(|(cover, track)| (cover.as_ref(), track.as_ref()))
        == Some((track.cover_path.as_ref(), missing_cover_track))
        || (missing_cover_track.is_some() && pending.contains(&track.path))
    {
        return None;
    }
    let image = artwork::get(track.cover_path.as_deref(), true)?;
    *rendered = Some((track.cover_path.clone(), missing_cover_track.cloned()));
    Some(image)
}

fn crossfade_cover(
    stack: &gtk::Stack,
    image: &gtk::Image,
    alternate: &gtk::Image,
    pixbuf: Option<&Pixbuf>,
) {
    let Some(pixbuf) = pixbuf else {
        stack.set_visible_child_name("placeholder");
        return;
    };
    let (image, name) = if stack.visible_child_name().as_deref() == Some("image") {
        (alternate, "image-alt")
    } else {
        (image, "image")
    };
    image.set_from_pixbuf(Some(pixbuf));
    stack.set_visible_child_name(name);
}

fn build_lyrics_view(model: &Rc<RefCell<Model>>, views: &gtk::Stack) -> gtk::Widget {
    let overlay = gtk::Overlay::new();
    overlay.style_context().add_class("lyrics-overlay");

    let backdrop = gtk::DrawingArea::new();
    backdrop.style_context().add_class("lyrics-backdrop");
    backdrop.set_hexpand(true);
    backdrop.set_vexpand(true);
    let backdrop_source: Rc<RefCell<Option<Pixbuf>>> = Rc::new(RefCell::new(None));
    let backdrop_previous: Rc<RefCell<Option<Pixbuf>>> = Rc::new(RefCell::new(None));
    let backdrop_mix = motion::Tween::new(1.0);
    {
        let source = backdrop_source.clone();
        let previous = backdrop_previous.clone();
        let mix = backdrop_mix.clone();
        backdrop.connect_draw(move |area, cr| {
            let allocation = area.allocation();
            let width = allocation.width() as f64;
            let height = allocation.height() as f64;
            let radius = 28.0;
            let half_pi = std::f64::consts::FRAC_PI_2;
            cr.new_sub_path();
            cr.arc(width - radius, radius, radius, -half_pi, 0.0);
            cr.arc(width - radius, height - radius, radius, 0.0, half_pi);
            cr.arc(radius, height - radius, radius, half_pi, 2.0 * half_pi);
            cr.arc(radius, radius, radius, 2.0 * half_pi, 3.0 * half_pi);
            cr.close_path();
            cr.clip();
            gtk::render_background(&area.style_context(), cr, 0.0, 0.0, width, height);
            for (image, opacity) in [
                (previous.borrow(), 1.0 - mix.value()),
                (source.borrow(), mix.value()),
            ] {
                let Some(pixbuf) = image.as_ref().filter(|_| opacity > 0.0) else {
                    continue;
                };
                let source_width = f64::from(pixbuf.width());
                let source_height = f64::from(pixbuf.height());
                let scale = (width / source_width).max(height / source_height);
                let _ = cr.save();
                cr.translate(
                    (width - source_width * scale) / 2.0,
                    (height - source_height * scale) / 2.0,
                );
                cr.scale(scale, scale);
                cr.set_source_pixbuf(pixbuf, 0.0, 0.0);
                let _ = cr.paint_with_alpha(opacity);
                let _ = cr.restore();
            }
            glib::Propagation::Proceed
        });
    }
    overlay.add(&backdrop);

    let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
    scrim.style_context().add_class("lyrics-scrim");
    overlay.add_overlay(&scrim);
    overlay.set_overlay_pass_through(&scrim, true);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(28);
    page.set_margin_end(28);
    overlay.add_overlay(&page);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let back = icon_button("go-previous-symbolic", "К плееру", "icon-button");
    back.style_context().add_class("lyrics-back");
    back.set_valign(gtk::Align::Center);
    back.set_focus_on_click(false);
    header.pack_start(&back, false, false, 0);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_valign(gtk::Align::Center);
    let lyrics_title = gtk::Label::new(Some("Текст песни"));
    lyrics_title.set_xalign(0.0);
    lyrics_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    lyrics_title.set_max_width_chars(32);
    lyrics_title.style_context().add_class("lyrics-title");
    let lyrics_artist = gtk::Label::new(Some("Выбери трек"));
    lyrics_artist.set_xalign(0.0);
    lyrics_artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    lyrics_artist.set_max_width_chars(32);
    lyrics_artist.style_context().add_class("lyrics-artist");
    titles.pack_start(&lyrics_title, false, false, 0);
    titles.pack_start(&lyrics_artist, false, false, 0);
    header.pack_start(&titles, true, true, 0);
    let find = gtk::Button::new();
    find.style_context().add_class("tonal-button");
    let find_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    find_row.pack_start(&symbolize("system-search-symbolic", 16), false, false, 0);
    let find_label = gtk::Label::new(Some("Найти текст"));
    find_row.pack_start(&find_label, false, false, 0);
    find.add(&find_row);
    find.show_all();
    find.set_no_show_all(true);
    header.pack_start(&find, false, false, 0);
    page.pack_start(&header, false, false, 0);

    let body = gtk::Stack::new();
    body.set_transition_type(gtk::StackTransitionType::Crossfade);
    body.set_transition_duration(200);
    let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    empty_box.set_halign(gtk::Align::Center);
    empty_box.set_valign(gtk::Align::Center);
    let empty_icon = symbolize("format-text-rich-symbolic", 40);
    empty_icon.style_context().add_class("empty-glyph");
    empty_box.pack_start(&empty_icon, false, false, 0);
    let empty = gtk::Label::new(Some("Выбери трек, чтобы увидеть текст"));
    empty.set_line_wrap(true);
    empty.set_justify(gtk::Justification::Center);
    empty.set_max_width_chars(46);
    empty.style_context().add_class("empty-state");
    empty_box.pack_start(&empty, false, false, 0);
    body.add_named(&empty_box, "empty");

    let lyrics_list = gtk::ListBox::new();
    lyrics_list.set_selection_mode(gtk::SelectionMode::Single);
    lyrics_list.set_activate_on_single_click(false);
    lyrics_list.style_context().add_class("lyrics-list");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let spacer_top = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer_top.set_size_request(-1, 0);
    let spacer_bottom = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer_bottom.set_size_request(-1, 0);
    content.pack_start(&spacer_top, false, false, 0);
    content.pack_start(&lyrics_list, false, false, 0);
    content.pack_start(&spacer_bottom, false, false, 0);
    let lyrics_scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    lyrics_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    lyrics_scroll.set_overlay_scrolling(false);
    lyrics_scroll.set_shadow_type(gtk::ShadowType::None);
    lyrics_scroll.style_context().add_class("lyrics-scroll");
    lyrics_scroll.add(&content);

    body.add_named(&lyrics_scroll, "list");
    body.set_visible_child_name("empty");
    page.pack_start(&body, true, true, 0);

    let hint = gtk::Label::new(Some("Текст песни"));
    hint.set_xalign(0.5);
    hint.set_line_wrap(true);
    hint.set_justify(gtk::Justification::Center);
    hint.style_context().add_class("lyrics-hint");
    page.pack_start(&hint, false, false, 0);

    let views_back = views.downgrade();
    back.connect_clicked(move |_| {
        if let Some(views) = views_back.upgrade() {
            views.set_visible_child_name("artwork");
        }
    });

    let model_find = model.clone();
    find.connect_clicked(move |_| start_lyrics_job(&model_find, true));
    let browsing_until = Rc::new(Cell::new(Instant::now()));
    let seek_browsing = browsing_until.clone();
    let explicit_seek = Rc::new(Cell::new(false));
    let row_seek = explicit_seek.clone();
    let model_play = model.clone();
    lyrics_list.connect_row_activated(move |list, row| {
        let line_index = row.index() as usize;
        let mut state = model_play.borrow_mut();
        let time = state
            .current
            .and_then(|i| state.tracks.get(i))
            .and_then(|t| t.lyrics.get(line_index))
            .and_then(|line| line.time);
        if let (Some(time), Some(player)) =
            (time, state.mpv.as_mut().filter(|player| player.loaded))
        {
            if player
                .command(json!(["seek", time, "absolute+exact"]))
                .is_ok()
            {
                if player.paused {
                    // mpv can report the preceding decoded position while paused.
                    player.paused_lyric_position = Some(time);
                }
                seek_browsing.set(Instant::now());
                row_seek.set(true);
            }
        }
        list.select_row(None::<&gtk::ListBoxRow>);
    });
    let resume = gtk::Button::with_label("К текущей строке");
    resume.style_context().add_class("lyrics-resume");
    resume.set_halign(gtk::Align::Center);
    resume.show_all();
    resume.set_no_show_all(true);
    resume.hide();
    resume.set_valign(gtk::Align::End);
    resume.set_margin_bottom(48);
    overlay.add_overlay(&resume);
    let follow = motion::Tween::new(0.0);
    let adjustment = lyrics_scroll.vadjustment();
    let unmap_follow = follow.clone();
    lyrics_scroll.connect_unmap(move |scroll| unmap_follow.snap(scroll.vadjustment().value()));
    let scrollbar_drag = Rc::new(Cell::new(false));
    let browse = {
        let browsing = browsing_until.clone();
        let motion = follow.clone();
        let adjustment = adjustment.clone();
        move || {
            browsing.set(Instant::now() + Duration::from_secs(5));
            motion.snap(adjustment.value());
        }
    };
    let wheel_browse = browse.clone();
    lyrics_scroll.connect_scroll_event(move |_, _| {
        wheel_browse();
        glib::Propagation::Proceed
    });
    if let Some(scrollbar) = lyrics_scroll.vscrollbar() {
        let press_browse = browse.clone();
        let dragging = scrollbar_drag.clone();
        scrollbar.connect_button_press_event(move |_, _| {
            dragging.set(true);
            press_browse();
            glib::Propagation::Proceed
        });
        let dragging = scrollbar_drag.clone();
        scrollbar.connect_button_release_event(move |_, _| {
            dragging.set(false);
            browse();
            glib::Propagation::Proceed
        });
    }
    let browsing = browsing_until.clone();
    resume.connect_clicked(move |_| browsing.set(Instant::now()));

    let model_update = model.clone();
    let list_update = lyrics_list.clone();
    let lyrics_adjustment = lyrics_scroll.vadjustment();
    let label_update = empty.clone();
    let body_update = body.clone();
    let hint_update = hint.clone();
    let title_update = lyrics_title.clone();
    let artist_update = lyrics_artist.clone();
    let backdrop_update = backdrop.clone();
    let backdrop_source_update = backdrop_source.clone();

    let alive = widget_lifetime(&overlay);
    let lyrics_view = overlay.downgrade();
    let mut rendered_track: Option<PathBuf> = None;
    let mut rendered_cover = None;
    let mut rendered_lines: Vec<Lyric> = Vec::new();
    let mut rendered_labels = None;
    let mut focus_style: Option<(Option<usize>, bool)> = None;
    let mut synchronized = false;
    let mut active = None;
    let mut lyric_times = Vec::new();
    let mut last_position: Option<f64> = None;
    let mut snap_follow = false;
    let mut rendered_status: Option<(bool, bool, bool, Option<String>)> = None;

    glib::timeout_add_local(Duration::from_millis(120), move || {
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        if !lyrics_view.upgrade().is_some_and(|view| view.is_mapped()) {
            return glib::ControlFlow::Continue;
        }
        let state = model_update.borrow();
        if let Some(index) = state.current {
            if let Some(track) = state.tracks.get(index) {
                let pending = state.lyrics_pending.contains(&track.path);
                let ready = state.metadata_ready.contains(&track.path);
                let status = (
                    track.lyrics.is_empty(),
                    pending,
                    ready,
                    state.lyrics_results.get(&track.path).map(String::as_str),
                );
                if rendered_status
                    .as_ref()
                    .map(|(empty, pending, ready, result)| {
                        (*empty, *pending, *ready, result.as_deref())
                    })
                    != Some(status)
                {
                    rendered_status = Some((
                        status.0,
                        status.1,
                        status.2,
                        status.3.map(ToOwned::to_owned),
                    ));
                    find.set_visible(status.0);
                    find.set_sensitive(ready && !pending);
                    find_label.set_text(if pending {
                        "Ищем текст…"
                    } else if status.3.is_some() {
                        "Искать снова"
                    } else {
                        "Найти текст"
                    });
                    if status.0 {
                        label_update.set_text(if pending {
                            "Ищем текст песни"
                        } else if !ready {
                            "Читаем данные трека…"
                        } else {
                            status.3.unwrap_or("Подготовка к поиску текста…")
                        });
                        hint_update.set_text("Плеер сам ищет текст в LRCLIB. Можно также положить .lrc рядом с аудиофайлом.");
                    }
                    body_update.set_visible_child_name(if status.0 { "empty" } else { "list" });
                }
                if let Some(image) =
                    current_artwork(track, &mut rendered_cover, &state.cover_pending)
                {
                    let pixbuf = image.and_then(|image| image.backdrop.clone());
                    let previous = backdrop_source_update.replace(pixbuf);
                    backdrop_previous.replace(previous);
                    backdrop_mix.snap(0.0);
                    let previous = backdrop_previous.clone();
                    backdrop_mix.animate(&backdrop_update, 1.0, 180, move |area, value| {
                        if value == 1.0 {
                            previous.replace(None);
                        }
                        area.queue_draw();
                    });
                    backdrop_update.queue_draw();
                }
                let labels = (&track.title, &track.artist);
                if rendered_labels
                    .as_ref()
                    .map(|(title, artist)| (title, artist))
                    != Some(labels)
                {
                    title_update.set_text(labels.0);
                    artist_update.set_text(labels.1);
                    rendered_labels = Some((track.title.clone(), track.artist.clone()));
                }
                if rendered_track.as_ref() != Some(&track.path) {
                    rendered_track = Some(track.path.clone());
                    browsing_until.set(Instant::now());
                    scrollbar_drag.set(false);
                    last_position = None;
                }
                if rendered_lines != track.lyrics {
                    rendered_lines.clone_from(&track.lyrics);
                    last_position = None;
                    focus_style = None;

                    lyric_times.clear();
                    lyric_times.extend(
                        rendered_lines
                            .iter()
                            .enumerate()
                            .filter_map(|(index, line)| line.time.map(|time| (time, index))),
                    );
                    lyric_times.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                    synchronized = !lyric_times.is_empty();
                    hint_update.set_text(if synchronized {
                        "Двойной щелчок по строке — перейти к этому моменту"
                    } else if track.lyrics.is_empty() {
                        "Плеер сам ищет текст в LRCLIB. Можно также положить .lrc рядом с аудиофайлом."
                    } else {
                        "В тексте нет временных меток. Листай его вручную."
                    });
                    for child in list_update.children() {
                        list_update.remove(&child);
                    }
                    for line in &track.lyrics {
                        let row = gtk::ListBoxRow::new();
                        row.style_context().add_class("lyric-row");
                        let text = gtk::Label::new(Some(&line.text));
                        text.set_xalign(0.0);
                        text.set_line_wrap(true);
                        text.set_justify(gtk::Justification::Left);
                        text.set_margin_top(12);
                        text.set_margin_bottom(12);
                        text.set_margin_start(24);
                        text.set_margin_end(24);
                        text.style_context().add_class("lyric-text");
                        if !synchronized {
                            text.style_context().add_class("lyric-plain");
                        }
                        row.add(&text);
                        list_update.add(&row);
                    }
                    list_update.show_all();
                    follow.snap(0.0);
                    lyrics_adjustment.set_value(0.0);
                }
                if synchronized && !track.lyrics.is_empty() && lyrics_scroll.is_mapped() {
                    let position = state.mpv.as_ref().map_or(0.0, |player| {
                        player.paused_lyric_position.unwrap_or(player.pos)
                    });
                    snap_follow |= explicit_seek.replace(false)
                        || last_position.is_some_and(|last| (last - position).abs() > 0.75);
                    if last_position != Some(position) {
                        last_position = Some(position);
                        let end =
                            lyric_times.partition_point(|(time, _)| *time <= position + 0.020);
                        active = end.checked_sub(1).map(|i| lyric_times[i].1);
                    }
                    let browsing = Instant::now() < browsing_until.get()
                        || scrollbar_drag.get()
                        || lyrics_scroll.has_grab();
                    resume.set_visible(browsing);
                    if focus_style != Some((active, browsing)) {
                        update_lyric_focus(
                            &list_update,
                            focus_style,
                            (active, browsing),
                            rendered_lines.len(),
                        );
                        focus_style = Some((active, browsing));
                    }
                    let height = lyrics_adjustment.page_size();
                    let reading_line = height * 0.32;
                    let row_height = |index| {
                        list_update
                            .row_at_index(index)
                            .map_or(0.0, |row| f64::from(row.allocated_height()))
                    };
                    let mut relayout = false;
                    for (spacer, padding) in [
                        (&spacer_top, reading_line - row_height(0) / 2.0),
                        (
                            &spacer_bottom,
                            height
                                - reading_line
                                - row_height(rendered_lines.len() as i32 - 1) / 2.0,
                        ),
                    ] {
                        let padding = padding.ceil().max(0.0) as i32;
                        if spacer.height_request() != padding {
                            spacer.set_size_request(-1, padding);
                            relayout = true;
                        }
                    }
                    if browsing {
                        follow.snap(lyrics_adjustment.value());
                    } else if !relayout {
                        let goal = active
                            .and_then(|i| list_update.row_at_index(i as i32))
                            .and_then(|row| {
                                let (_, y) = row.translate_coordinates(&content, 0, 0)?;
                                Some(
                                    f64::from(y) + f64::from(row.allocated_height()) / 2.0
                                        - reading_line,
                                )
                            })
                            .unwrap_or(0.0)
                            .clamp(0.0, (lyrics_adjustment.upper() - height).max(0.0));
                        if std::mem::take(&mut snap_follow) {
                            follow.snap(goal);
                            lyrics_adjustment.set_value(goal);
                        } else if (goal - lyrics_adjustment.value()).abs() > 0.5 {
                            follow.animate(&lyrics_scroll, goal, 200, |scroll, value| {
                                scroll.vadjustment().set_value(value);
                            });
                        }
                    }
                } else {
                    resume.hide();
                    for spacer in [&spacer_top, &spacer_bottom] {
                        if spacer.height_request() != 0 {
                            spacer.set_size_request(-1, 0);
                        }
                    }
                }
            }
        } else {
            if rendered_track.take().is_some() {
                rendered_status = None;
                resume.hide();
                find.hide();
                title_update.set_text("Текст песни");
                artist_update.set_text("Выбери трек");
                rendered_labels = None;
                label_update.set_text("Выбери трек");
                hint_update.set_text("");
                body_update.set_visible_child_name("empty");
            }
            if rendered_cover.take().is_some() {
                backdrop_source_update.replace(None);
                backdrop_previous.replace(None);
                backdrop_mix.snap(1.0);
                backdrop_update.queue_draw();
            }
        }
        glib::ControlFlow::Continue
    });
    overlay.upcast()
}

fn update_lyric_focus(
    list: &gtk::ListBox,
    previous: Option<(Option<usize>, bool)>,
    (active, browsing): (Option<usize>, bool),
    count: usize,
) {
    let (old_active, old_browsing) = previous.unwrap_or((None, false));
    let past_end = |index: Option<usize>, browsing| if browsing { 0 } else { index.unwrap_or(0) };
    let old_end = past_end(old_active, old_browsing);
    let new_end = past_end(active, browsing);
    for index in old_end.min(new_end)..old_end.max(new_end).min(count) {
        if let Some(row) = list.row_at_index(index as i32) {
            set_style_class(&row, "past", index < new_end);
        }
    }
    for index in [old_active, active].into_iter().flatten() {
        if let Some(row) = list.row_at_index(index as i32) {
            set_style_class(&row, "active", active == Some(index));
        }
    }
}

fn widget_lifetime(widget: &impl IsA<gtk::Widget>) -> Rc<Cell<bool>> {
    let alive = Rc::new(Cell::new(true));
    let destroyed = alive.clone();
    widget.connect_destroy(move |_| destroyed.set(false));
    alive
}

fn animations_enabled() -> bool {
    gtk::Settings::default()
        .map(|settings| settings.property::<bool>("gtk-enable-animations"))
        .unwrap_or(true)
}

#[derive(Clone)]
struct QueueRow {
    row: gtk::ListBoxRow,
    path: Rc<RefCell<PathBuf>>,
    number: gtk::Label,
    cover: gtk::Image,
    cover_stack: gtk::Stack,
    rendered_cover: Rc<RefCell<Option<PathBuf>>>,
    title: gtk::Label,
    artist: gtk::Label,
    duration: gtk::Label,
}

fn make_queue_row(position: usize, model: &Rc<RefCell<Model>>) -> QueueRow {
    let row = gtk::ListBoxRow::new();
    row.style_context().add_class("queue-row");
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    line.set_margin_top(6);
    line.set_margin_bottom(6);
    line.set_margin_start(12);
    line.set_margin_end(12);
    let number = gtk::Label::new(Some(&(position + 1).to_string()));
    number.style_context().add_class("queue-index");
    line.pack_start(&number, false, false, 0);
    let cover_stack = gtk::Stack::new();
    cover_stack.style_context().add_class("queue-cover");
    cover_stack.add_named(&cover_placeholder(44, 20), "placeholder");
    let cover = gtk::Image::new();
    cover.set_size_request(44, 44);
    cover_stack.add_named(&cover, "image");
    cover_stack.set_visible_child_name("placeholder");
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(None);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(48);
    title.style_context().add_class("queue-title");
    let artist = gtk::Label::new(None);
    artist.set_xalign(0.0);
    artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    artist.set_max_width_chars(48);
    artist.style_context().add_class("queue-artist");
    titles.pack_start(&title, false, false, 0);
    titles.pack_start(&artist, false, false, 0);
    let duration = gtk::Label::new(None);
    duration.style_context().add_class("queue-duration");
    line.pack_start(&cover_stack, false, false, 0);
    line.pack_start(&titles, true, true, 0);
    line.pack_start(&duration, false, false, 0);
    let path = Rc::new(RefCell::new(PathBuf::new()));
    let menu = gtk::Menu::new();
    for (label, action) in [
        ("Воспроизвести", QueueEdit::Play),
        ("Поставить следующим", QueueEdit::PlayNext),
        ("Переместить выше", QueueEdit::Up),
        ("Переместить ниже", QueueEdit::Down),
        ("Убрать из очереди", QueueEdit::Remove),
    ] {
        let item = gtk::MenuItem::with_label(label);
        let model = model.clone();
        let path = path.clone();
        item.connect_activate(move |_| {
            edit_queue(&model, &path.borrow(), action);
        });
        menu.append(&item);
    }
    line.pack_start(&menu_button(&menu, "Действия с треком"), false, false, 0);
    row.add(&line);

    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let targets = [gtk::TargetEntry::new(
        "application/x-own-player-track",
        gtk::TargetFlags::SAME_APP,
        0,
    )];
    let handle = gtk::EventBox::new();
    handle.set_tooltip_text(Some("Перетащи трек, чтобы изменить порядок"));
    handle.add(&symbolize("list-drag-handle-symbolic", 16));
    handle.style_context().add_class("queue-handle");
    line.pack_start(&handle, false, false, 0);
    handle.drag_source_set(
        gtk::gdk::ModifierType::BUTTON1_MASK,
        &targets,
        gtk::gdk::DragAction::MOVE,
    );
    let drag_snapshot = Rc::new(RefCell::new(PathBuf::new()));
    let drag_start = drag_snapshot.clone();
    let source = path.clone();
    handle.connect_drag_begin(move |_, _| {
        drag_start.replace(source.borrow().clone());
    });
    handle.connect_drag_data_get(move |_, _, data, _, _| {
        data.set(
            &data.target(),
            8,
            drag_snapshot.borrow().as_os_str().as_bytes(),
        );
    });
    row.drag_dest_set(gtk::DestDefaults::ALL, &targets, gtk::gdk::DragAction::MOVE);
    let target = path.clone();
    let model = model.clone();
    row.connect_drag_data_received(move |row, context, _, y, data, _, time| {
        let source = PathBuf::from(std::ffi::OsString::from_vec(data.data()));
        let target = target.borrow().clone();
        let after = y >= row.allocated_height() / 2;
        let moved = edit_queue(&model, &source, QueueEdit::Drop(&target, after));
        context.drag_finish(moved, false, time);
    });

    QueueRow {
        row,
        path,
        number,
        cover,
        cover_stack,
        rendered_cover: Rc::new(RefCell::new(None)),
        title,
        artist,
        duration,
    }
}

fn queue_filter(rows: &Rc<RefCell<Vec<QueueRow>>>, model: &Rc<RefCell<Model>>, filter: &str) {
    let state = model.borrow();
    for (index, item) in rows.borrow().iter().enumerate() {
        let visible = state.tracks.get(index).is_some_and(|track| {
            filter.is_empty()
                || format!("{} {} {}", track.title, track.artist, track.album)
                    .to_lowercase()
                    .contains(filter)
        });
        item.row.set_visible(visible);
    }
}

fn update_queue_state(
    body: &gtk::Stack,
    rows: &Rc<RefCell<Vec<QueueRow>>>,
    model: &Rc<RefCell<Model>>,
) {
    body.set_visible_child_name(if model.borrow().tracks.is_empty() {
        "empty"
    } else if rows.borrow().iter().any(|item| item.row.is_visible()) {
        "list"
    } else {
        "no-results"
    });
}

fn set_style_class(widget: &impl IsA<gtk::Widget>, class: &str, enabled: bool) {
    if enabled {
        widget.style_context().add_class(class);
    } else {
        widget.style_context().remove_class(class);
    }
}

fn queue_highlight(rows: &Rc<RefCell<Vec<QueueRow>>>, current: Option<usize>) {
    for (index, item) in rows.borrow().iter().enumerate() {
        if Some(index) == current {
            item.number.set_text("♪");
            item.number.set_tooltip_text(Some("Текущий трек"));
        } else {
            item.number.set_text(&(index + 1).to_string());
            item.number.set_tooltip_text(
                item.row
                    .style_context()
                    .has_class("queued-next")
                    .then_some("Следующий трек"),
            );
        }
        set_style_class(&item.row, "playing", Some(index) == current);
    }
}

#[derive(Clone, Copy)]
enum QueueEdit<'a> {
    Play,
    PlayNext,
    Up,
    Down,
    Remove,
    Drop(&'a Path, bool),
}

fn menu_button(menu: &gtk::Menu, tooltip: &str) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.set_image(Some(&symbolize("view-more-symbolic", 18)));
    button.set_tooltip_text(Some(tooltip));
    button.style_context().add_class("queue-menu");
    if let Some(accessible) = button.accessible() {
        accessible.set_name(tooltip);
    }
    menu.show_all();
    button.set_popup(Some(menu));
    motion::animate_button(&button);
    button
}

fn edit_queue(model: &Rc<RefCell<Model>>, path: &Path, action: QueueEdit<'_>) -> bool {
    let mut state = model.borrow_mut();
    let Some(index) = state.tracks.iter().position(|track| track.path == path) else {
        return false;
    };
    let current = state.current.map(|index| state.tracks[index].path.clone());
    let destination = match action {
        QueueEdit::Play => {
            drop(state);
            play_index(model, index);
            return true;
        }

        QueueEdit::PlayNext => {
            if current.as_deref() == Some(path) {
                return false;
            }
            state.play_next = Some(path.to_path_buf());
            state.library_revision += 1;
            drop(state);
            save_session(model);
            return true;
        }
        QueueEdit::Up => index.saturating_sub(1),
        QueueEdit::Down => (index + 1).min(state.tracks.len() - 1),
        QueueEdit::Drop(target, after) => {
            let Some(target) = state.tracks.iter().position(|track| track.path == target) else {
                return false;
            };
            if target == index {
                return false;
            }
            let boundary = target + usize::from(after);
            boundary - usize::from(index < boundary)
        }
        QueueEdit::Remove => index,
    };
    if matches!(action, QueueEdit::Remove) {
        state.tracks.remove(index);
        state.shuffle_order.remove(path);
        if state.play_next.as_deref() == Some(path) {
            state.play_next = None;
        }
    } else if destination != index {
        let track = state.tracks.remove(index);
        state.tracks.insert(destination, track);
    } else {
        return false;
    }
    state.current =
        current.and_then(|path| state.tracks.iter().position(|track| track.path == path));
    let removed_current = state.current.is_none() && matches!(action, QueueEdit::Remove);
    let continue_at = (removed_current
        && state.mpv.as_ref().is_some_and(|p| p.loaded && !p.paused))
    .then(|| index.min(state.tracks.len().saturating_sub(1)))
    .filter(|_| !state.tracks.is_empty());
    state.library_revision += 1;
    state.failed_tracks.clear();
    drop(state);
    if let Some(index) = continue_at {
        play_index(model, index);
    } else if removed_current {
        stop_playback(model);
    }
    save_session(model);
    true
}

fn clear_queue(model: &Rc<RefCell<Model>>) {
    stop_playback(model);
    let mut state = model.borrow_mut();
    state.tracks.clear();
    state.current = None;
    state.play_next = None;
    state.shuffle_order = ShuffleOrder::default();
    state.failed_tracks.clear();
    state.library_revision += 1;
    drop(state);
    save_session(model);
}

fn build_queue_page(model: &Rc<RefCell<Model>>) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let summary = gtk::Label::new(None);
    summary.set_xalign(0.0);
    summary.style_context().add_class("queue-summary");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let heading = gtk::Label::new(Some("Очередь"));
    heading.style_context().add_class("heading");
    header.pack_start(&heading, false, false, 0);
    header.pack_end(&summary, false, false, 0);
    page.pack_start(&header, false, false, 0);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.style_context().add_class("queue-toolbar");
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Найти в очереди"));
    search.style_context().add_class("search");
    toolbar.pack_start(&search, true, true, 0);
    let menu = gtk::Menu::new();
    let start = gtk::MenuItem::with_label("Воспроизвести с начала");
    let model_start = model.clone();
    start.connect_activate(move |_| play_index(&model_start, 0));
    menu.append(&start);
    let clear_all = gtk::MenuItem::with_label("Очистить очередь…");
    let model_clear = model.clone();
    clear_all.connect_activate(move |_| {
        let parent = active_window();
        let dialog = gtk::MessageDialog::new(
            parent.as_ref(),
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            gtk::MessageType::Question,
            gtk::ButtonsType::None,
            "Очистить очередь?",
        );
        dialog.set_secondary_text(Some("Аудиофайлы останутся на диске."));
        dialog.add_button("Отмена", gtk::ResponseType::Cancel);
        dialog.add_button("Очистить", gtk::ResponseType::Accept);
        if dialog.run() == gtk::ResponseType::Accept {
            clear_queue(&model_clear);
        }
        dialog.close();
    });
    menu.append(&clear_all);
    let queue_actions = menu_button(&menu, "Действия с очередью");
    queue_actions.style_context().add_class("queue-add");
    let add = icon_button(
        "list-add-symbolic",
        "Добавить музыку в очередь",
        "queue-add",
    );
    let model_add = model.clone();
    add.connect_clicked(move |_| {
        if let Some(paths) = choose_files(false) {
            append_paths(&model_add, paths);
        }
    });
    toolbar.pack_start(&add, false, false, 0);
    toolbar.pack_start(&queue_actions, false, false, 0);
    page.pack_start(&toolbar, false, false, 0);
    let body = gtk::Stack::new();
    body.set_transition_type(gtk::StackTransitionType::Crossfade);
    body.set_transition_duration(200);
    let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    empty_box.set_halign(gtk::Align::Center);
    empty_box.set_valign(gtk::Align::Center);
    let empty_icon = symbolize("view-list-symbolic", 40);
    empty_icon.style_context().add_class("empty-glyph");
    empty_box.pack_start(&empty_icon, false, false, 0);
    let empty_title = gtk::Label::new(Some("Очередь пуста"));
    empty_title.style_context().add_class("empty-title");
    empty_box.pack_start(&empty_title, false, false, 0);
    let empty_note = gtk::Label::new(Some("Добавь файлы или папку с музыкой"));
    empty_note.style_context().add_class("secondary");
    empty_box.pack_start(&empty_note, false, false, 0);
    body.add_named(&empty_box, "empty");
    let no_results = gtk::Box::new(gtk::Orientation::Vertical, 12);
    no_results.set_halign(gtk::Align::Center);
    no_results.set_valign(gtk::Align::Center);
    let no_title = gtk::Label::new(Some("Ничего не нашлось"));
    no_title.style_context().add_class("empty-title");
    let no_note = gtk::Label::new(Some("Попробуй другое название или имя исполнителя"));
    no_note.style_context().add_class("secondary");
    let clear = gtk::Button::with_label("Сбросить поиск");
    clear.set_halign(gtk::Align::Center);
    let search_clear = search.clone();
    clear.connect_clicked(move |_| search_clear.set_text(""));
    no_results.pack_start(&no_title, false, false, 0);
    no_results.pack_start(&no_note, false, false, 0);
    no_results.pack_start(&clear, false, false, 8);
    body.add_named(&no_results, "no-results");

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.style_context().add_class("queue-card");
    scroll.set_overlay_scrolling(true);
    scroll.set_shadow_type(gtk::ShadowType::None);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    list.style_context().add_class("queue-list");
    scroll.add(&list);
    let table = gtk::Box::new(gtk::Orientation::Vertical, 0);
    table.style_context().add_class("queue-table");
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    columns.style_context().add_class("queue-columns");
    let number = gtk::Label::new(Some("#"));
    number.style_context().add_class("queue-index");
    let name = gtk::Label::new(Some("Трек"));
    name.set_xalign(0.0);
    name.set_margin_start(56);
    let duration = gtk::Label::new(Some("Время"));
    duration.style_context().add_class("queue-duration");
    duration.set_margin_end(96);
    columns.pack_start(&number, false, false, 0);
    columns.pack_start(&name, true, true, 0);
    columns.pack_start(&duration, false, false, 0);
    table.pack_start(&columns, false, false, 0);
    table.pack_start(&scroll, true, true, 0);
    body.add_named(&table, "list");
    body.set_visible_child_name("empty");
    page.pack_start(&body, true, true, 0);

    let rows: Rc<RefCell<Vec<QueueRow>>> = Rc::new(RefCell::new(Vec::new()));

    let model_play = model.clone();
    let rows_play = rows.clone();
    list.connect_row_activated(move |_, row| {
        let index = rows_play
            .borrow()
            .get(row.index() as usize)
            .and_then(|item| {
                let path = item.path.borrow();
                model_play
                    .borrow()
                    .tracks
                    .iter()
                    .position(|track| track.path == *path)
            });
        if let Some(index) = index {
            play_index(&model_play, index);
        }
    });

    let model_filter = model.clone();
    let rows_filter = rows.clone();
    let body_filter = body.clone();
    search.connect_search_changed(move |entry| {
        let filter = entry.text().trim().to_lowercase();
        queue_filter(&rows_filter, &model_filter, &filter);
        update_queue_state(&body_filter, &rows_filter, &model_filter);
    });

    let model_update = model.clone();
    let rows_update = rows.clone();
    let list_update = list.clone();
    let body_update = body.clone();
    let search_update = search.clone();
    let alive = widget_lifetime(&page);
    let mut rendered_revision = u64::MAX;
    let mut rendered_current = None;
    let mut visible_covers = Vec::new();
    glib::timeout_add_local(Duration::from_millis(80), move || {
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        let revision = {
            let state = model_update.borrow();
            start.set_sensitive(!state.tracks.is_empty() && state.mpv.is_some());
            clear_all.set_sensitive(!state.tracks.is_empty());
            state.library_revision
        };
        if rendered_revision != revision {
            rendered_revision = revision;
            let state = model_update.borrow();
            let total: f64 = state.tracks.iter().map(|track| track.duration).sum();
            let hours = (total / 3600.0) as u64;
            let minutes = (total / 60.0) as u64 % 60;
            let duration = if hours > 0 {
                format!(" · {hours} ч {minutes} мин")
            } else if minutes > 0 {
                format!(" · {minutes} мин")
            } else {
                String::new()
            };
            summary.set_text(&format!(
                "{}{}",
                tracks_phrase(state.tracks.len()),
                duration
            ));

            {
                let mut rows = rows_update.borrow_mut();
                while rows.len() < state.tracks.len() {
                    let item = make_queue_row(rows.len(), &model_update);
                    list_update.add(&item.row);
                    rows.push(item);
                }
                while rows.len() > state.tracks.len() {
                    if let Some(item) = rows.pop() {
                        list_update.remove(&item.row);
                    }
                }
                for (index, track) in state.tracks.iter().enumerate() {
                    let item = &rows[index];
                    if *item.path.borrow() != track.path {
                        item.path.replace(track.path.clone());
                        if item.rendered_cover.borrow().as_ref() != track.cover_path.as_ref() {
                            item.rendered_cover.replace(None);
                            item.cover_stack.set_visible_child_name("placeholder");
                            item.cover.clear();
                        }
                    }
                    let is_next = state.play_next.as_ref() == Some(&track.path);
                    set_style_class(&item.row, "queued-next", is_next);
                    item.number.set_text(&(index + 1).to_string());
                    item.number
                        .set_tooltip_text(is_next.then_some("Следующий трек"));

                    item.title.set_text(&track.title);
                    item.title.set_tooltip_text(Some(&track.title));
                    let mut subtitle = if track.album.is_empty() {
                        track.artist.clone()
                    } else {
                        format!("{}  ·  {}", track.artist, track.album)
                    };
                    if is_next {
                        subtitle.push_str("  ·  Следующий");
                    }
                    item.artist.set_text(&subtitle);
                    item.artist.set_tooltip_text(Some(&subtitle));
                    item.duration.set_text(&if track.duration > 0.0 {
                        format_time(track.duration)
                    } else {
                        String::new()
                    });
                }
            }
            list_update.show_all();
            let filter = search_update.text().trim().to_lowercase();
            queue_filter(&rows_update, &model_update, &filter);
            update_queue_state(&body_update, &rows_update, &model_update);
            queue_highlight(&rows_update, state.current);
        }
        let current = model_update.borrow().current;
        if current != rendered_current {
            rendered_current = current;
            queue_highlight(&rows_update, current);
        }
        // Load visible rows and a small buffer, limiting background work.
        if scroll.is_mapped() {
            let state = model_update.borrow();
            let adjustment = scroll.vadjustment();
            let top = adjustment.value();
            let bottom = top + adjustment.page_size();
            let available = 4usize.saturating_sub(state.cover_pending.len());
            let mut jobs = Vec::new();
            let first = list_update
                .row_at_y((top - 160.0).max(4.0) as i32)
                .map_or(0, |row| row.index() as usize);
            let rows = rows_update.borrow();
            let mut visible = Vec::new();
            for (index, (item, track)) in rows.iter().zip(&state.tracks).enumerate().skip(first) {
                if !item.row.is_visible() {
                    continue;
                }
                let bounds = item.row.allocation();
                let y = f64::from(bounds.y());
                let end = y + f64::from(bounds.height());
                if y > bottom + 160.0 {
                    break;
                }
                if end < top - 160.0 {
                    continue;
                }
                visible.push(index);
                if *item.rendered_cover.borrow() != track.cover_path {
                    if let Some(image) = artwork::get(track.cover_path.as_deref(), false) {
                        if let Some(image) = image {
                            item.cover.set_from_pixbuf(Some(&image.thumb));
                            item.cover_stack.set_visible_child_name("image");
                        } else {
                            item.cover.clear();
                            item.cover_stack.set_visible_child_name("placeholder");
                        }
                        *item.rendered_cover.borrow_mut() = track.cover_path.clone();
                    }
                }
                if available > 0
                    && track.cover_path.is_none()
                    && state.metadata_ready.contains(&track.path)
                    && !state.cover_pending.contains(&track.path)
                    && !state.cover_attempted.contains(&track.path)
                {
                    jobs.push(((top - end).max(y - bottom).max(0.0), &track.path));
                }
            }
            if visible != visible_covers {
                for index in visible_covers.drain(..) {
                    if let Some(item) = rows
                        .get(index)
                        .filter(|_| visible.binary_search(&index).is_err())
                    {
                        item.rendered_cover.replace(None);
                        item.cover_stack.set_visible_child_name("placeholder");
                        item.cover.clear();
                    }
                }
                visible_covers = visible;
            }
            drop(rows);
            jobs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let paths: Vec<_> = jobs
                .into_iter()
                .take(available)
                .map(|(_, path)| path.clone())
                .collect();
            drop(state);
            for path in paths {
                start_cover_job(&model_update, path);
            }
        }
        glib::ControlFlow::Continue
    });
    page.upcast()
}

fn build_controls(model: &Rc<RefCell<Model>>) -> gtk::Widget {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 4);
    panel.style_context().add_class("player-bar");

    let seek_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let time = gtk::Label::new(Some("0:00"));
    time.style_context().add_class("timecode");
    let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1000.0, 1.0);
    seek.set_draw_value(false);
    describe_control(&seek, "Позиция воспроизведения");
    seek.style_context().add_class("seek");
    let duration = gtk::Label::new(Some("0:00"));
    duration.style_context().add_class("timecode");
    seek_row.pack_start(&time, false, false, 0);
    seek_row.pack_start(&seek, true, true, 0);
    seek_row.pack_start(&duration, false, false, 0);
    seek_row.style_context().add_class("timeline");

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    let now_box = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    now_box.set_valign(gtk::Align::Center);
    now_box.style_context().add_class("now-playing");
    let small_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    small_frame.style_context().add_class("mini-cover-frame");
    small_frame.set_size_request(48, 48);
    let small_stack = gtk::Stack::new();
    small_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    small_stack.set_transition_duration(180);
    small_stack.add_named(&cover_placeholder(48, 20), "placeholder");
    let small_image = gtk::Image::new();
    small_image.set_size_request(48, 48);
    small_stack.add_named(&small_image, "image");
    let small_alt = gtk::Image::new();
    small_alt.set_size_request(48, 48);
    small_stack.add_named(&small_alt, "image-alt");
    small_stack.set_visible_child_name("placeholder");
    small_frame.pack_start(&small_stack, true, true, 0);
    now_box.pack_start(&small_frame, false, false, 0);
    let now_text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    now_text.set_valign(gtk::Align::Center);
    let now = gtk::Label::new(Some("Трек не выбран"));
    now.set_xalign(0.0);
    now.set_ellipsize(gtk::pango::EllipsizeMode::End);
    now.set_max_width_chars(26);
    now.style_context().add_class("now-title");
    let now_artist = gtk::Label::new(Some(""));
    now_artist.set_xalign(0.0);
    now_artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    now_artist.set_max_width_chars(26);
    now_artist.style_context().add_class("now-artist");
    now_text.pack_start(&now, false, false, 0);
    now_text.pack_start(&now_artist, false, false, 0);
    now_box.pack_start(&now_text, true, true, 0);
    row.pack_start(&now_box, true, true, 0);

    let shuffle = gtk::ToggleButton::new();
    shuffle.set_focus_on_click(false);
    shuffle.add(&toggle_content(&symbolize(
        "media-playlist-shuffle-symbolic",
        18,
    )));
    describe_control(&shuffle, "Перемешать");
    shuffle.style_context().add_class("transport-button");
    shuffle.set_active(model.borrow().shuffle);
    let prev = gtk::Button::new();
    prev.set_focus_on_click(false);
    prev.set_image(Some(&symbolize("media-skip-backward-symbolic", 18)));
    describe_control(&prev, "Предыдущий трек");
    prev.style_context().add_class("transport-button");
    let play = gtk::Button::new();
    play.set_focus_on_click(false);
    let play_icons = gtk::Stack::new();
    play_icons.set_transition_type(gtk::StackTransitionType::Crossfade);
    play_icons.set_transition_duration(140);
    play_icons.add_named(&symbolize("media-playback-start-symbolic", 28), "play");
    play_icons.add_named(&symbolize("media-playback-pause-symbolic", 28), "pause");
    play_icons.set_visible_child_name("play");
    play.add(&play_icons);
    describe_control(&play, "Воспроизвести");
    play.style_context().add_class("play-fab");
    let next = gtk::Button::new();
    next.set_focus_on_click(false);
    next.set_image(Some(&symbolize("media-skip-forward-symbolic", 18)));
    describe_control(&next, "Следующий трек");
    next.style_context().add_class("transport-button");
    let repeat = gtk::Button::new();
    repeat.set_focus_on_click(false);
    let repeat_icons = gtk::Stack::new();
    repeat_icons.set_transition_type(gtk::StackTransitionType::Crossfade);
    repeat_icons.set_transition_duration(150);
    repeat_icons.add_named(&symbolize("media-playlist-repeat-symbolic", 18), "all");
    let repeat_one = if gtk::IconTheme::default()
        .is_some_and(|theme| theme.has_icon("media-playlist-repeat-song-symbolic"))
    {
        symbolize("media-playlist-repeat-song-symbolic", 18).upcast::<gtk::Widget>()
    } else {
        let icon = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        icon.add(&symbolize("media-playlist-repeat-symbolic", 18));
        icon.add(&gtk::Label::new(Some("1")));
        icon.upcast()
    };
    repeat_icons.add_named(&repeat_one, "one");
    repeat.add(&toggle_content(&repeat_icons));
    repeat.style_context().add_class("transport-button");
    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    transport.style_context().add_class("transport");
    transport.set_halign(gtk::Align::Center);
    transport.set_valign(gtk::Align::Center);
    transport.pack_start(&shuffle, false, false, 0);
    transport.pack_start(&prev, false, false, 0);
    transport.pack_start(&play, false, false, 0);
    transport.pack_start(&next, false, false, 0);
    transport.pack_start(&repeat, false, false, 0);
    row.set_center_widget(Some(&transport));

    let volume_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    volume_box.style_context().add_class("volume-group");
    volume_box.set_halign(gtk::Align::End);
    volume_box.set_valign(gtk::Align::Center);
    let volume_icon = symbolize("audio-volume-high-symbolic", 18);
    volume_icon.style_context().add_class("muted-icon");
    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume.set_value(model.borrow().volume);
    volume.set_draw_value(false);
    describe_control(&volume, "Громкость");
    volume.set_size_request(104, -1);
    volume.style_context().add_class("volume");
    volume_box.pack_start(&volume_icon, false, false, 0);
    volume_box.pack_start(&volume, false, false, 0);
    row.pack_end(&volume_box, false, false, 0);
    panel.pack_start(&row, false, false, 0);
    panel.pack_start(&seek_row, false, false, 0);

    apply_repeat_label(&repeat, model.borrow().repeat);

    let m = model.clone();
    play.connect_clicked(move |button| {
        let selected = {
            let state = m.borrow();
            state
                .current
                .or_else(|| (!state.tracks.is_empty()).then_some(0))
        };
        if let Some(index) = selected.filter(|_| {
            m.borrow()
                .mpv
                .as_ref()
                .map(|player| !player.loaded)
                .unwrap_or(true)
        }) {
            play_index(&m, index);
            return;
        }
        let mut state = m.borrow_mut();
        if let Some(player) = state.mpv.as_mut() {
            let _ = player.command(json!(["cycle", "pause"]));
            player.paused = !player.paused;
            set_play_icon(button, player.loaded && !player.paused);
        }
    });
    let m = model.clone();
    next.connect_clicked(move |_| next_track(&m, 1, false));
    let m = model.clone();
    prev.connect_clicked(move |_| next_track(&m, -1, false));
    let m = model.clone();
    shuffle.connect_toggled(move |button| {
        let mut state = m.borrow_mut();
        state.shuffle = button.is_active();
        state.shuffle_order = ShuffleOrder::default();
        drop(state);
        save_session(&m);
    });
    let m = model.clone();
    repeat.connect_clicked(move |button| {
        let mut state = m.borrow_mut();
        state.repeat = (state.repeat + 1) % 3;
        let mode = state.repeat;
        drop(state);
        apply_repeat_label(button, mode);
        let mut state = m.borrow_mut();
        let loop_one = state.repeat == 2;
        if let Some(player) = state.mpv.as_mut() {
            let _ = player.command(json!([
                "set_property",
                "loop-file",
                if loop_one { "inf" } else { "no" }
            ]));
        }
        drop(state);
        save_session(&m);
    });
    let m = model.clone();
    let volume_save = Rc::new(RefCell::new(None));
    volume.connect_value_changed(move |scale| {
        let value = scale.value();
        let mut state = m.borrow_mut();
        state.volume = value;
        if let Some(player) = state.mpv.as_mut() {
            let _ = player.command(json!(["set_property", "volume", value]));
        }
        drop(state);
        schedule_session_save(&m, &volume_save);
    });
    let m = model.clone();
    let time_clone = time.clone();
    seek.connect_change_value(move |_, _, value| {
        let mut state = m.borrow_mut();
        if let Some(player) = state.mpv.as_mut() {
            let at = player.duration * value / 1000.0;
            if player.loaded
                && player
                    .command(json!(["seek", at, "absolute+exact"]))
                    .is_ok()
            {
                if player.paused {
                    player.paused_lyric_position = Some(at);
                }
                time_clone.set_text(&format_time(at));
            }
        }
        glib::Propagation::Proceed
    });
    let m = model.clone();
    let now_clone = now.clone();
    let now_artist_clone = now_artist.clone();
    let seek_tick = seek.clone();
    let time_tick = time.clone();
    let duration_tick = duration.clone();
    let play_tick = play.clone();
    let small_image_tick = small_image.clone();
    let small_stack_tick = small_stack.clone();
    let alive = widget_lifetime(&panel);
    let mut rendered_cover = None;
    let mut rendered_track: Option<(PathBuf, String, String)> = None;
    let mut rendered_time = None;
    let mut rendered_duration = None;
    let mut rendered_playing: Option<bool> = None;
    glib::timeout_add_local(Duration::from_millis(120), move || {
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        let state = m.borrow();
        if let Some(index) = state.current {
            if let Some(track) = state.tracks.get(index) {
                if rendered_track
                    .as_ref()
                    .map(|(path, title, artist)| (path, title, artist))
                    != Some((&track.path, &track.title, &track.artist))
                {
                    rendered_track = Some((
                        track.path.clone(),
                        track.title.clone(),
                        track.artist.clone(),
                    ));
                    now_clone.set_text(&track.title);
                    now_clone.set_tooltip_text(Some(&track.title));
                    now_artist_clone.set_text(&track.artist);
                    now_artist_clone.set_tooltip_text(Some(&track.artist));
                }
                if let Some(image) =
                    current_artwork(track, &mut rendered_cover, &state.cover_pending)
                {
                    crossfade_cover(
                        &small_stack_tick,
                        &small_image_tick,
                        &small_alt,
                        image.as_ref().and_then(|art| art.mini.as_ref()),
                    );
                    theme::set_hue(image.and_then(|art| art.hue));
                }
            }
        }
        if state.current.is_none() {
            if rendered_track.take().is_some() {
                now_clone.set_text("Трек не выбран");
                now_artist_clone.set_text("");
                now_clone.set_tooltip_text(None);
                now_artist_clone.set_tooltip_text(None);
            }
            if rendered_cover.take().is_some() {
                small_stack_tick.set_visible_child_name("placeholder");
                theme::set_hue(None);
            }
        }
        let available = !state.tracks.is_empty() && state.mpv.is_some();
        for (widget, sensitive) in [
            (play_tick.upcast_ref::<gtk::Widget>(), available),
            (
                prev.upcast_ref(),
                available && (!state.shuffle || state.shuffle_order.has_previous()),
            ),
            (next.upcast_ref(), available && can_advance(&state)),
            (
                seek_tick.upcast_ref(),
                state
                    .mpv
                    .as_ref()
                    .is_some_and(|p| p.loaded && p.duration > 0.0),
            ),
        ] {
            if widget.is_sensitive() != sensitive {
                widget.set_sensitive(sensitive);
            }
        }
        if let Some(player) = state.mpv.as_ref() {
            let position = player.paused_lyric_position.unwrap_or(player.pos);
            let second = position as u64;
            if rendered_time != Some(second) {
                rendered_time = Some(second);
                time_tick.set_text(&format_time(position));
            }
            let duration = player.duration as u64;
            if rendered_duration != Some(duration) {
                rendered_duration = Some(duration);
                duration_tick.set_text(&format_time(player.duration));
            }
            if !seek_tick.has_grab() {
                let fraction = if player.duration > 0.0 {
                    position / player.duration
                } else {
                    0.0
                };
                let value = (fraction * 1000.0).clamp(0.0, 1000.0);
                if seek_tick.value() != value {
                    seek_tick.set_value(value);
                }
            }
            let playing = player.loaded && !player.paused;
            if rendered_playing != Some(playing) {
                rendered_playing = Some(playing);
                set_play_icon(&play_tick, playing);
            }
        }
        glib::ControlFlow::Continue
    });
    panel.upcast()
}

fn set_play_icon(button: &gtk::Button, playing: bool) {
    describe_control(
        button,
        if playing {
            "Пауза"
        } else {
            "Воспроизвести"
        },
    );
    set_style_class(button, "playing", playing);
    if let Some(icons) = button
        .child()
        .and_then(|child| child.downcast::<gtk::Stack>().ok())
    {
        icons.set_visible_child_name(if playing { "pause" } else { "play" });
    }
}

fn toggle_content(image: &impl IsA<gtk::Widget>) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.set_halign(gtk::Align::Fill);
    overlay.set_valign(gtk::Align::Fill);
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    overlay.add(image);
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.style_context().add_class("transport-indicator");
    dot.set_size_request(4, 4);
    dot.set_halign(gtk::Align::Center);
    dot.set_valign(gtk::Align::End);
    dot.set_margin_bottom(5);
    overlay.add_overlay(&dot);
    overlay.set_overlay_pass_through(&dot, true);
    overlay
}

fn apply_repeat_label(button: &gtk::Button, mode: usize) {
    let (tooltip, active) = match mode {
        1 => ("Повторять очередь", true),
        2 => ("Повторять трек", true),
        _ => ("Без повтора", false),
    };
    describe_control(button, tooltip);
    if let Some(icons) = button
        .child()
        .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        .and_then(|overlay| overlay.child())
        .and_then(|child| child.downcast::<gtk::Stack>().ok())
    {
        icons.set_visible_child_name(if mode == 2 { "one" } else { "all" });
    }
    set_style_class(button, "active", active);
}

fn start_mpv(model: &Rc<RefCell<Model>>) {
    let Some((child, path)) = Mpv::spawn() else {
        model.borrow_mut().status =
            Some("Не удалось запустить mpv. Проверь, что он установлен.".into());
        return;
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(Mpv::connect(child, path));
    });
    let model = Rc::downgrade(model);
    glib::timeout_add_local(Duration::from_millis(30), move || {
        let Some(model) = model.upgrade() else {
            return glib::ControlFlow::Break;
        };
        match receiver.try_recv() {
            Ok(Some(mut player)) => {
                let mut state = model.borrow_mut();
                let _ = player.command(json!(["set_property", "volume", state.volume]));
                let _ = player.command(json!(["set_property", "speed", state.speed]));
                let _ = player.command(json!([
                    "set_property",
                    "loop-file",
                    if state.repeat == 2 { "inf" } else { "no" }
                ]));
                state.mpv = Some(player);
            }
            Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                model.borrow_mut().status =
                    Some("Не удалось запустить mpv. Проверь, что он установлен.".into());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
        }
        glib::ControlFlow::Break
    });
}

fn tick_mpv(model: &Rc<RefCell<Model>>) {
    let (ended, failed) = {
        let mut state = model.borrow_mut();
        let Some(player) = state.mpv.as_mut() else {
            return;
        };
        player.read_events();
        let ended = std::mem::take(&mut player.ended);
        let failed = std::mem::take(&mut player.failed);
        if let Some(error) = player.error.take() {
            state.status = Some(error);
        }
        (ended, failed)
    };
    if failed {
        let mut state = model.borrow_mut();
        if let Some(current) = state.current {
            state.failed_tracks.insert(current);
            let next = advance_index(&mut state, 1, true);
            drop(state);
            if let Some(next) = next {
                load_index(model, next);
                let mut state = model.borrow_mut();
                if state.status.is_none() {
                    state.status = Some(
                        "Не удалось воспроизвести предыдущий файл. Перешли к следующему треку."
                            .into(),
                    );
                }
            }
        }
    } else if ended {
        model.borrow_mut().failed_tracks.clear();
        next_track(model, 1, true);
    }
}

fn play_index(model: &Rc<RefCell<Model>>, index: usize) {
    model.borrow_mut().failed_tracks.clear();
    load_index(model, index);
}

fn load_index(model: &Rc<RefCell<Model>>, index: usize) {
    let mut state = model.borrow_mut();
    if index >= state.tracks.len() {
        return;
    }
    if state.mpv.is_none() {
        state.status = Some(
            "Не удалось запустить mpv. Проверь, что он установлен, и перезапусти плеер.".into(),
        );
        return;
    }
    let path = state.tracks[index].path.clone();
    state.status = None;
    if let Some(player) = state.mpv.as_mut() {
        if player.command(json!(["loadfile", path, "replace"])).is_ok() {
            player.pos = 0.0;
            player.duration = 0.0;
            player.ended = false;
            player.paused = false;
            player.loaded = true;
        } else {
            state.status = Some("Не удалось передать трек в mpv. Перезапусти плеер.".into());
            return;
        }
        let _ = player.command(json!(["set_property", "pause", false]));
    }
    state.current = Some(index);
    if state.shuffle {
        state.shuffle_order.record(&path);
    }
    if state.play_next.as_ref() == Some(&path) {
        state.play_next = None;
        state.library_revision += 1;
    }
    drop(state);
    save_session(model);
    if !model.borrow().metadata_ready.contains(&path) && path.is_file() {
        start_metadata_job(model, vec![path.clone()]);
    }
    start_cover_job(model, path);
}

fn next_track(model: &Rc<RefCell<Model>>, direction: isize, automatic: bool) {
    let mut state = model.borrow_mut();
    if state.tracks.is_empty() {
        return;
    }
    let index = advance_index(&mut state, direction, false);
    drop(state);
    if let Some(index) = index {
        play_index(model, index);
    } else if automatic {
        stop_playback(model);
    }
}

fn can_advance(state: &Model) -> bool {
    let Some(current) = state.current else {
        return true;
    };
    state.play_next.is_some()
        || state.repeat == 1
        || if state.shuffle {
            state
                .shuffle_order
                .can_advance(state.tracks.iter().map(|track| track.path.as_path()))
        } else {
            current + 1 < state.tracks.len()
        }
}

fn advance_index(state: &mut Model, direction: isize, skip_failed: bool) -> Option<usize> {
    if direction > 0 {
        if let Some(path) = &state.play_next {
            if let Some(index) = state.tracks.iter().position(|track| &track.path == path) {
                return Some(index);
            }
        }
    }
    let Some(current) = state.current else {
        return (!state.tracks.is_empty()).then_some(0);
    };
    if state.shuffle {
        let tracks = state
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| {
                (!skip_failed || !state.failed_tracks.contains(&index))
                    .then_some(track.path.as_path())
            });
        let current_path = &state.tracks[current].path;
        state.shuffle_order.record(current_path);
        let path = if direction < 0 {
            state.shuffle_order.previous()?.to_path_buf()
        } else {
            let mut path =
                state
                    .shuffle_order
                    .next(tracks.clone(), current_path, state.repeat == 1);
            while skip_failed
                && path
                    .as_ref()
                    .is_some_and(|path| !tracks.clone().any(|item| item == path))
            {
                path = state
                    .shuffle_order
                    .next(tracks.clone(), current_path, state.repeat == 1);
            }
            path?
        };
        return state.tracks.iter().position(|track| track.path == path);
    }
    if skip_failed {
        return (current + 1..state.tracks.len())
            .chain((0..current).filter(|_| state.repeat == 1))
            .find(|index| !state.failed_tracks.contains(index));
    }
    next_index(current, state.tracks.len(), direction, state.repeat)
}

fn next_index(current: usize, count: usize, direction: isize, repeat: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    if direction < 0 {
        return Some(if current == 0 && repeat == 1 {
            count - 1
        } else {
            current.saturating_sub(1)
        });
    }
    if current + 1 < count {
        Some(current + 1)
    } else if repeat == 1 {
        Some(0)
    } else {
        None
    }
}

fn stop_playback(model: &Rc<RefCell<Model>>) {
    let mut state = model.borrow_mut();
    if let Some(player) = state.mpv.as_mut() {
        let _ = player.command(json!(["stop"]));
        player.paused = true;
        player.loaded = false;
        player.pos = 0.0;
        player.duration = 0.0;
    }
}

fn append_paths(model: &Rc<RefCell<Model>>, paths: Vec<PathBuf>) {
    {
        let mut state = model.borrow_mut();
        state.imports_pending += 1;
        state.status = None;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut found = Vec::new();
        for path in paths {
            collect_audio(&path, &mut found);
        }
        found.sort();
        found.dedup();
        let _ = sender.send(found);
    });
    let model_result = model.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        match receiver.try_recv() {
            Ok(paths) => {
                let mut state = model_result.borrow_mut();
                state.imports_pending = state.imports_pending.saturating_sub(1);
                let found = !paths.is_empty();
                let mut fresh = Vec::new();
                let mut known: std::collections::HashSet<PathBuf> = state
                    .tracks
                    .iter()
                    .map(|track| track.path.clone())
                    .collect();
                for path in paths {
                    if known.insert(path.clone()) {
                        state.tracks.push(preview_track(path.clone()));
                        fresh.push(path);
                    }
                }
                if fresh.is_empty() {
                    state.status = Some(if found {
                        "Эти треки уже есть в очереди".into()
                    } else {
                        "Не нашлось аудиофайлов поддерживаемых форматов. Проверь папку.".into()
                    });
                    return glib::ControlFlow::Break;
                }
                state.status = None;
                state.library_revision += 1;
                let start = state.current.is_none();
                drop(state);
                save_session(&model_result);
                if start {
                    play_index(&model_result, 0);
                }
                start_metadata_job(&model_result, fresh);
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let mut state = model_result.borrow_mut();
                state.imports_pending = state.imports_pending.saturating_sub(1);
                state.status = Some("Импорт прервался. Попробуй добавить файлы ещё раз.".into());
                glib::ControlFlow::Break
            }
        }
    });
}

fn restore_session(model: &Rc<RefCell<Model>>) {
    let saved = match session::load() {
        Ok(saved) => saved,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!("Не удалось восстановить очередь: {error}");
            return;
        }
    };
    let mut state = model.borrow_mut();
    let mut seen = std::collections::HashSet::new();
    let mut paths = Vec::new();
    for path in saved.paths {
        if seen.insert(path.clone()) {
            state.tracks.push(preview_track(path.clone()));
            if path.is_file() {
                paths.push(path);
            }
        }
    }
    state.current = saved
        .current_path
        .and_then(|path| state.tracks.iter().position(|track| track.path == path));
    state.play_next = saved.play_next.filter(|path| {
        state.tracks.iter().any(|track| &track.path == path)
            && state
                .current
                .is_none_or(|index| &state.tracks[index].path != path)
    });
    state.shuffle = saved.shuffle;
    state.repeat = saved.repeat.min(2);
    state.online_covers = saved.online_covers;
    state.volume = saved.volume.clamp(0.0, 100.0);
    state.speed = saved.speed.clamp(0.5, 2.0);
    if let Some(dir) = saved.soundcloud_dir {
        state.soundcloud_dir = dir;
    }
    state.library_revision += 1;
    let current_path = state
        .current
        .and_then(|index| state.tracks.get(index))
        .map(|track| track.path.clone());
    drop(state);
    if let Some(path) = current_path.filter(|path| path.is_file()) {
        if let Some(position) = paths.iter().position(|item| item == &path) {
            paths.swap(0, position);
        }
        start_cover_job(model, path);
    }
    if !paths.is_empty() {
        start_metadata_job(model, paths);
    }
}

fn start_metadata_job(model: &Rc<RefCell<Model>>, mut paths: Vec<PathBuf>) {
    {
        let mut state = model.borrow_mut();
        paths.retain(|path| state.metadata_pending.insert(path.clone()));
    }
    if paths.is_empty() {
        return;
    }
    let indices: std::collections::HashMap<PathBuf, usize> = model
        .borrow()
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.path.clone(), index))
        .collect();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for path in paths {
            if sender.send(read_track(path)).is_err() {
                break;
            }
        }
    });
    let model_result = model.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let mut received = 0;
        while received < 32 {
            match receiver.try_recv() {
                Ok(mut track) => {
                    let ready_path = track.path.clone();
                    let mut state = model_result.borrow_mut();
                    if let Some(existing) = indices
                        .get(&track.path)
                        .and_then(|index| state.tracks.get_mut(*index))
                    {
                        track.cover_path = existing.cover_path.clone();
                        if track.lyrics.is_empty() {
                            track.lyrics = existing.lyrics.clone();
                        }
                        *existing = track;
                        state.metadata_pending.remove(&ready_path);
                        state.metadata_ready.insert(ready_path);
                        state.library_revision += 1;
                    }
                    received += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

/// At most two requests at once; results belong to the file, never to the open view.
fn start_lyrics_job(model: &Rc<RefCell<Model>>, retry: bool) {
    let track = {
        let mut state = model.borrow_mut();
        let Some(path) = state
            .current
            .and_then(|i| state.tracks.get(i))
            .map(|track| track.path.clone())
        else {
            return;
        };
        if retry {
            state.lyrics_results.remove(&path);
        }
        let track = &state.tracks[state.current.unwrap()];
        if !state.metadata_ready.contains(&path)
            || !track.lyrics.is_empty()
            || state.lyrics_pending.contains(&path)
            || state.lyrics_results.contains_key(&path)
            || state.lyrics_pending.len() >= 2
        {
            return;
        }
        let track = track.clone();
        state.lyrics_pending.insert(path);
        track
    };
    let path = track.path.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(online_lyrics(&track));
    });
    let model = model.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("Поиск прервался. Нажми «Искать снова».".into())
            }
        };
        let mut state = model.borrow_mut();
        state.lyrics_pending.remove(&path);
        let message = match result {
            Ok(lines) if !lines.is_empty() => {
                if let Some(track) = state.tracks.iter_mut().find(|track| track.path == path) {
                    if track.lyrics.is_empty() {
                        track.lyrics = lines;
                    }
                }
                "Текст найден".into()
            }
            Ok(_) => "Текст пока не найден. Можно попробовать ещё раз.".into(),
            Err(error) => error,
        };
        state.lyrics_results.insert(path.clone(), message);
        glib::ControlFlow::Break
    });
}

fn start_cover_job(model: &Rc<RefCell<Model>>, path: PathBuf) {
    let (selected, online) = {
        let mut state = model.borrow_mut();
        let Some(selected) = state
            .tracks
            .iter()
            .find(|track| track.path == path)
            .cloned()
        else {
            return;
        };
        if selected.cover_path.is_some() || !state.cover_pending.insert(path.clone()) {
            return;
        }
        state.cover_attempted.insert(path.clone());
        (selected, state.online_covers)
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let lookup_identity = (selected.title.clone(), selected.artist.clone());
    std::thread::spawn(move || {
        let local = extract_cover(&selected.path);
        let cover = if local.is_some() || !online {
            local
        } else {
            cover_art::online_cover(&selected)
        };
        let _ = sender.send(cover);
    });
    let model_result = model.clone();
    glib::timeout_add_local(Duration::from_millis(35), move || {
        match receiver.try_recv() {
            Ok(cover) => {
                let mut state = model_result.borrow_mut();
                state.cover_pending.remove(&path);
                let mut retry = false;
                if let Some(track) = state.tracks.iter_mut().find(|track| track.path == path) {
                    retry = cover.is_none()
                        && online
                        && (track.title.as_str(), track.artist.as_str())
                            != (lookup_identity.0.as_str(), lookup_identity.1.as_str());
                    track.cover_path = cover;
                }
                drop(state);
                if retry {
                    start_cover_job(&model_result, path.clone());
                }
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                model_result.borrow_mut().cover_pending.remove(&path);
                glib::ControlFlow::Break
            }
        }
    });
}

fn schedule_session_save(
    model: &Rc<RefCell<Model>>,
    pending: &Rc<RefCell<Option<glib::SourceId>>>,
) {
    if let Some(source) = pending.borrow_mut().take() {
        source.remove();
    }
    let model = model.clone();
    let pending_done = pending.clone();
    *pending.borrow_mut() = Some(glib::timeout_add_local_once(
        Duration::from_millis(300),
        move || {
            pending_done.borrow_mut().take();
            save_session(&model);
        },
    ));
}

fn save_session(model: &Rc<RefCell<Model>>) {
    let state = model.borrow();
    let saved = Session {
        paths: state
            .tracks
            .iter()
            .map(|track| track.path.clone())
            .collect(),
        current_path: state
            .current
            .and_then(|index| state.tracks.get(index))
            .map(|track| track.path.clone()),
        play_next: state.play_next.clone(),
        shuffle: state.shuffle,
        repeat: state.repeat,
        online_covers: state.online_covers,
        volume: state.volume,
        speed: state.speed,
        soundcloud_dir: Some(state.soundcloud_dir.clone()),
    };
    if let Err(error) = session::save(&saved) {
        eprintln!("Не удалось сохранить очередь: {error}");
    }
}

fn build_settings_page(model: &Rc<RefCell<Model>>) -> gtk::ScrolledWindow {
    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_overlay_scrolling(true);
    scroll.set_shadow_type(gtk::ShadowType::None);
    scroll.style_context().add_class("settings-page");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    scroll.add(&content);
    content.set_margin_top(20);
    content.set_margin_bottom(16);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.pack_start(&settings_text("Настройки", "heading"), false, false, 0);

    let playback = settings_section(&content, "Воспроизведение");
    let speed_block = gtk::Box::new(gtk::Orientation::Vertical, 4);
    speed_block.style_context().add_class("settings-speed");
    let speed_header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    speed_header.pack_start(&settings_text("Скорость", "settings-label"), true, true, 0);
    let speed_value = settings_text(&format!("{:.2}×", model.borrow().speed), "speed-value");
    speed_value.set_xalign(0.5);
    speed_header.pack_end(&speed_value, false, false, 0);
    speed_block.pack_start(&speed_header, false, false, 0);
    let speed = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.5, 2.0, 0.05);
    speed.set_value(model.borrow().speed);
    speed.set_draw_value(false);
    let speed_save = Rc::new(RefCell::new(None));
    speed.connect_value_changed({
        let model = model.clone();
        move |scale| {
            let value = scale.value();
            {
                let mut state = model.borrow_mut();
                state.speed = value;
                if let Some(player) = state.mpv.as_mut() {
                    let _ = player.command(json!(["set_property", "speed", value]));
                }
            }
            schedule_session_save(&model, &speed_save);
        }
    });
    describe_control(&speed, "Скорость воспроизведения");
    speed.style_context().add_class("settings-speed-slider");
    speed.connect_value_changed(move |scale| {
        speed_value.set_text(&format!("{:.2}×", scale.value()));
    });
    speed_block.pack_start(&speed, false, false, 0);

    let presets = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    presets.set_homogeneous(true);
    presets.style_context().add_class("speed-segments");
    for value in [0.75, 1.0, 1.25, 1.5, 2.0] {
        let preset = gtk::ToggleButton::with_label(&format!("{value}×"));
        preset.style_context().add_class("speed-preset");
        let matches = move |speed: f64| (speed - value).abs() < 0.001;
        preset.set_active(matches(speed.value()));
        let group_update = Rc::new(Cell::new(false));
        let slider = speed.downgrade();
        let updating = group_update.clone();
        preset.connect_toggled(move |button| {
            if updating.get() {
                return;
            }
            if let Some(slider) = slider.upgrade() {
                slider.set_value(value);
                // Clicking the current preset keeps it selected.
                if !button.is_active() {
                    button.set_active(true);
                }
            }
        });
        let button = preset.downgrade();
        speed.connect_value_changed(move |scale| {
            if let Some(button) = button.upgrade() {
                group_update.set(true);
                button.set_active(matches(scale.value()));
                group_update.set(false);
            }
        });
        presets.pack_start(&preset, true, true, 0);
    }
    speed_block.pack_start(&presets, false, false, 0);
    playback.pack_start(&speed_block, false, false, 0);

    let library = settings_section(&content, "Библиотека");
    let (cover_row, _) = settings_row(
        "Обложки из интернета",
        "Загружать недостающие обложки из Яндекс Музыки по CSV.",
    );
    let covers = gtk::Switch::new();
    covers.set_active(model.borrow().online_covers);
    covers.set_valign(gtk::Align::Center);
    describe_control(&covers, "Загружать обложки из Яндекс Музыки");
    cover_row.pack_end(&covers, false, false, 0);
    library.pack_start(&cover_row, false, false, 0);
    let model_covers = model.clone();
    covers.connect_active_notify(move |button| {
        model_covers.borrow_mut().online_covers = button.is_active();
        save_session(&model_covers);
        if button.is_active() {
            model_covers.borrow_mut().cover_attempted.clear();
            let current = {
                let state = model_covers.borrow();
                state
                    .current
                    .and_then(|index| state.tracks.get(index).map(|track| track.path.clone()))
            };
            if let Some(path) = current {
                start_cover_job(&model_covers, path);
            }
        }
    });

    library.pack_start(
        &gtk::Separator::new(gtk::Orientation::Horizontal),
        false,
        false,
        0,
    );
    let connected = cover_art::has_index();
    let (csv_row, status) = settings_row(
        "CSV Яндекс Музыки",
        if connected {
            "Плейлист подключён"
        } else {
            "Плейлист пока не подключён"
        },
    );
    let csv_button = gtk::Button::with_label(if connected {
        "Заменить…"
    } else {
        "Выбрать…"
    });
    csv_button.set_valign(gtk::Align::Center);
    csv_button.style_context().add_class("lyrics-action");
    csv_row.pack_end(&csv_button, false, false, 0);
    library.pack_start(&csv_row, false, false, 0);
    let model_index = model.clone();
    csv_button.connect_clicked(move |button| {
        let parent = button
            .toplevel()
            .and_then(|widget| widget.downcast::<gtk::Window>().ok());
        let chooser = gtk::FileChooserDialog::with_buttons(
            Some("Выбрать CSV плейлиста"),
            parent.as_ref(),
            gtk::FileChooserAction::Open,
            &[
                ("Отмена", gtk::ResponseType::Cancel),
                ("Подключить", gtk::ResponseType::Accept),
            ],
        );
        chooser.set_modal(true);
        chooser.set_destroy_with_parent(true);
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Плейлисты CSV"));
        filter.add_pattern("*.[cC][sS][vV]");
        chooser.add_filter(filter);
        let status = status.clone();
        let button = button.clone();
        let model = model_index.clone();
        chooser.connect_response(move |chooser, response| {
            if let Some(path) = (response == gtk::ResponseType::Accept)
                .then(|| chooser.filename())
                .flatten()
            {
                match cover_art::install_index(&path) {
                    Ok(()) => {
                        status.set_text("Плейлист подключён");
                        status.style_context().remove_class("error");
                        button.set_label("Заменить…");
                        let current = {
                            let mut state = model.borrow_mut();
                            state.cover_attempted.clear();
                            state
                                .current
                                .and_then(|index| state.tracks.get(index))
                                .map(|track| track.path.clone())
                        };
                        if let Some(path) = current {
                            start_cover_job(&model, path);
                        }
                    }
                    Err(error) => {
                        status.set_text(&error);
                        status.style_context().add_class("error");
                    }
                }
            }
            chooser.close();
        });
        motion::animate_buttons(chooser.upcast_ref());
        chooser.show();
    });

    let info = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    info.style_context().add_class("settings-info");
    info.pack_start(
        &symbolize("dialog-information-symbolic", 16),
        false,
        false,
        0,
    );
    info.pack_start(
        &settings_text(
            "Если локального текста нет, плеер ищет его в LRCLIB.",
            "settings-note",
        ),
        true,
        true,
        0,
    );
    content.pack_start(&info, false, false, 0);
    scroll
}

fn settings_text(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_line_wrap(true);
    label.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.style_context().add_class(class);
    label
}

fn settings_section(content: &gtk::Box, title: &str) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    section.pack_start(
        &settings_text(title, "settings-section-title"),
        false,
        false,
        0,
    );
    let surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
    surface.style_context().add_class("settings-section");
    section.pack_start(&surface, false, false, 0);
    content.pack_start(&section, false, false, 0);
    surface
}

fn settings_row(title: &str, note: &str) -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.style_context().add_class("settings-row");
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let note = settings_text(note, "settings-note");
    labels.pack_start(&settings_text(title, "settings-label"), false, false, 0);
    labels.pack_start(&note, false, false, 0);
    row.pack_start(&labels, true, true, 0);
    (row, note)
}

fn show_soundcloud_dialog(model: &Rc<RefCell<Model>>) {
    let dialog = gtk::Dialog::with_buttons(
        Some("Скачать с SoundCloud"),
        active_window().as_ref(),
        gtk::DialogFlags::MODAL,
        &[("Закрыть", gtk::ResponseType::Close)],
    );
    dialog.set_default_size(580, 460);
    dialog.set_resizable(false);
    dialog.style_context().add_class("settings-dialog");
    let content = dialog.content_area();
    content.set_margin_top(16);
    content.set_margin_bottom(12);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_spacing(10);

    let heading = gtk::Label::new(Some("Скачать с SoundCloud"));
    heading.set_xalign(0.0);
    heading.style_context().add_class("heading");
    content.pack_start(&heading, false, false, 0);

    let note = gtk::Label::new(Some(
        "Можно скачать только оригиналы треков, для которых автор разрешил загрузку. Плеер не скачивает стримы и не обходит DRM или другие ограничения.",
    ));
    note.set_line_wrap(true);
    note.set_max_width_chars(52);
    note.set_xalign(0.0);
    note.style_context().add_class("settings-note");
    content.pack_start(&note, false, false, 0);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.style_context().add_class("settings-card");
    let url_label = gtk::Label::new(Some("Ссылка на трек"));
    url_label.set_xalign(0.0);
    url_label.style_context().add_class("settings-label");
    card.pack_start(&url_label, false, false, 0);
    let url = gtk::Entry::new();
    url.set_placeholder_text(Some("https://soundcloud.com/artist/track"));
    url.set_activates_default(false);
    card.pack_start(&url, false, false, 0);

    let folder_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let folder_title = gtk::Label::new(Some("Папка"));
    folder_title.set_xalign(0.0);
    folder_title.style_context().add_class("settings-label");
    let folder_value = gtk::Label::new(None);
    folder_value.set_xalign(1.0);
    folder_value.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    folder_value.set_max_width_chars(30);
    folder_value.style_context().add_class("settings-note");
    let folder_button = gtk::Button::with_label("Изменить…");
    folder_button.style_context().add_class("ghost-button");
    folder_header.pack_start(&folder_title, false, false, 0);
    folder_header.pack_start(&folder_value, true, true, 0);
    folder_header.pack_start(&folder_button, false, false, 0);
    card.pack_start(&folder_header, false, false, 0);
    let initial_dir = model.borrow().soundcloud_dir.clone();
    folder_value.set_text(&initial_dir.to_string_lossy());

    let auth_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let auth_title = gtk::Label::new(Some("Доступ"));
    auth_title.set_xalign(0.0);
    auth_title.style_context().add_class("settings-label");
    let auth_status = gtk::Label::new(Some(soundcloud::token_summary()));
    auth_status.set_xalign(1.0);
    auth_status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    auth_status.style_context().add_class("settings-note");
    let auth_button = gtk::Button::with_label("Авторизация…");
    auth_button.style_context().add_class("ghost-button");
    auth_header.pack_start(&auth_title, false, false, 0);
    auth_header.pack_start(&auth_status, true, true, 0);
    auth_header.pack_start(&auth_button, false, false, 0);
    card.pack_start(&auth_header, false, false, 0);
    content.pack_start(&card, false, false, 0);

    let progress = gtk::ProgressBar::new();
    progress.set_show_text(true);
    progress.set_text(Some("Готов к загрузке"));
    content.pack_start(&progress, false, false, 0);

    let status = gtk::Label::new(Some("Вставь ссылку на трек и нажми «Скачать»."));
    status.set_xalign(0.0);
    status.set_line_wrap(true);
    status.set_max_width_chars(54);
    status.set_selectable(true);
    status.style_context().add_class("download-status");
    content.pack_start(&status, false, false, 0);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_homogeneous(false);
    let cancel = gtk::Button::with_label("Отменить");
    cancel.style_context().add_class("ghost-button");
    cancel.set_no_show_all(true);
    cancel.set_visible(false);
    let download = gtk::Button::with_label("Скачать");
    download.style_context().add_class("tonal-button");
    let library = gtk::Button::with_label("Вся библиотека");
    library.style_context().add_class("ghost-button");
    library.set_tooltip_text(Some(
        "Скачать все треки из лайков, плейлистов и своих загрузок, которые авторы разрешили скачивать",
    ));
    actions.pack_end(&download, false, false, 0);
    actions.pack_end(&library, false, false, 0);
    actions.pack_end(&cancel, false, false, 0);
    content.pack_start(&actions, false, false, 4);

    let model_folder = model.clone();
    let folder_value_change = folder_value.clone();
    folder_button.connect_clicked(move |_| {
        let current = model_folder.borrow().soundcloud_dir.clone();
        let chooser = gtk::FileChooserDialog::with_buttons(
            Some("Папка для загрузок"),
            active_window().as_ref(),
            gtk::FileChooserAction::SelectFolder,
            &[
                ("Отмена", gtk::ResponseType::Cancel),
                ("Выбрать", gtk::ResponseType::Accept),
            ],
        );
        chooser.set_filename(&current);
        let selected = if chooser.run() == gtk::ResponseType::Accept {
            chooser.filename()
        } else {
            None
        };
        chooser.close();
        if let Some(path) = selected {
            model_folder.borrow_mut().soundcloud_dir = path.clone();
            folder_value_change.set_text(&path.to_string_lossy());
            save_session(&model_folder);
        }
    });

    let auth_status_refresh = auth_status.clone();
    let auth_window = dialog.clone();
    auth_button.connect_clicked(move |_| {
        show_soundcloud_token_dialog(&auth_window);
        auth_status_refresh.set_text(soundcloud::token_summary());
    });

    let (sender, receiver) = std::sync::mpsc::channel();
    let receiver = Rc::new(RefCell::new(receiver));
    let job: Rc<RefCell<Option<soundcloud::Job>>> = Rc::new(RefCell::new(None));
    let busy = Rc::new(Cell::new(false));
    let batch = Rc::new(Cell::new(false));
    let collected: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));

    let model_start = model.clone();
    let url_start = url.clone();
    let status_start = status.clone();
    let progress_start = progress.clone();
    let cancel_start = cancel.clone();
    let download_start = download.clone();
    let folder_button_start = folder_button.clone();
    let auth_button_start = auth_button.clone();
    let library_start = library.clone();
    let job_start = job.clone();
    let busy_start = busy.clone();
    let batch_start = batch.clone();
    let collected_start = collected.clone();
    let sender_download = sender.clone();
    download.connect_clicked(move |_| {
        if busy_start.get() {
            return;
        }
        let link = url_start.text().trim().to_string();
        if link.is_empty() {
            status_start.set_text("Вставь ссылку на трек SoundCloud.");
            return;
        }
        let dir = model_start.borrow().soundcloud_dir.clone();
        let token = soundcloud::get_token();
        batch_start.set(false);
        collected_start.borrow_mut().clear();
        busy_start.set(true);
        url_start.set_sensitive(false);
        folder_button_start.set_sensitive(false);
        auth_button_start.set_sensitive(false);
        library_start.set_sensitive(false);
        download_start.set_sensitive(false);
        cancel_start.set_visible(true);
        cancel_start.set_sensitive(true);
        cancel_start.set_label("Отменить");
        progress_start.set_fraction(0.0);
        progress_start.set_text(Some("Определяем трек…"));
        status_start.style_context().remove_class("error");
        status_start.set_text("Соединяемся с SoundCloud…");
        let new_job = soundcloud::start(link, dir, token, sender_download.clone());
        *job_start.borrow_mut() = Some(new_job);
    });

    let model_library = model.clone();
    let url_library = url.clone();
    let status_library = status.clone();
    let progress_library = progress.clone();
    let cancel_library = cancel.clone();
    let download_library = download.clone();
    let folder_button_library = folder_button.clone();
    let auth_button_library = auth_button.clone();
    let library_self = library.clone();
    let job_library = job.clone();
    let busy_library = busy.clone();
    let batch_library = batch.clone();
    let collected_library = collected.clone();
    let dialog_library = dialog.clone();
    let sender_library = sender.clone();
    library.connect_clicked(move |_| {
        if busy_library.get() {
            return;
        }
        let confirm = gtk::MessageDialog::new(
            Some(&dialog_library),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Question,
            gtk::ButtonsType::None,
            "Скачать всю библиотеку?",
        );
        confirm.set_secondary_text(Some(
            "Будут скачаны треки из твоих лайков, плейлистов и загрузок, которые авторы разрешили скачивать. Остальные попадут в отчёт soundcloud-skipped.txt. Это может занять много времени.",
        ));
        confirm.add_button("Отмена", gtk::ResponseType::Cancel);
        confirm.add_button("Скачать", gtk::ResponseType::Accept);
        let accepted = confirm.run() == gtk::ResponseType::Accept;
        confirm.close();
        if !accepted {
            return;
        }
        let dir = model_library.borrow().soundcloud_dir.clone();
        let token = soundcloud::get_token();
        if token.is_none() {
            status_library.style_context().add_class("error");
            status_library.set_text(
                "Для библиотеки нужен токен SoundCloud. Нажми «Авторизация…» и вставь токен своей сессии.",
            );
            return;
        }
        batch_library.set(true);
        collected_library.borrow_mut().clear();
        busy_library.set(true);
        url_library.set_sensitive(false);
        folder_button_library.set_sensitive(false);
        auth_button_library.set_sensitive(false);
        library_self.set_sensitive(false);
        download_library.set_sensitive(false);
        cancel_library.set_visible(true);
        cancel_library.set_sensitive(true);
        cancel_library.set_label("Отменить");
        progress_library.set_fraction(0.0);
        progress_library.pulse();
        progress_library.set_text(Some("Читаем библиотеку…"));
        status_library.style_context().remove_class("error");
        status_library.set_text("Ищем треки в лайках, плейлистах и загрузках…");
        let new_job = soundcloud::start_library(dir, token, sender_library.clone());
        *job_library.borrow_mut() = Some(new_job);
    });

    let job_cancel = job.clone();
    let cancel_status = status.clone();
    let cancel_button = cancel.clone();
    cancel.connect_clicked(move |_| {
        if let Some(job) = job_cancel.borrow().as_ref() {
            job.cancel();
            cancel_button.set_sensitive(false);
            cancel_button.set_label("Отменяем…");
            cancel_status.set_text("Останавливаем загрузку…");
        }
    });

    let running = Rc::new(Cell::new(true));
    let running_poll = running.clone();
    let model_poll = model.clone();
    let url_poll = url.clone();
    let status_poll = status.clone();
    let progress_poll = progress.clone();
    let cancel_poll = cancel.clone();
    let download_poll = download.clone();
    let library_poll = library.clone();
    let folder_button_poll = folder_button.clone();
    let auth_button_poll = auth_button.clone();
    let job_poll = job.clone();
    let busy_poll = busy.clone();
    let batch_poll = batch.clone();
    let collected_poll = collected.clone();
    let batch_total = Rc::new(Cell::new(0usize));
    let batch_index = Rc::new(Cell::new(0usize));
    let batch_total_poll = batch_total.clone();
    let batch_index_poll = batch_index.clone();
    let receiver_poll = receiver.clone();
    glib::timeout_add_local(Duration::from_millis(120), move || {
        if !running_poll.get() {
            return glib::ControlFlow::Break;
        }
        let mut finished = false;
        loop {
            let event = match receiver_poll.borrow().try_recv() {
                Ok(event) => event,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            };
            match event {
                soundcloud::Event::Resolving => {
                    progress_poll.set_fraction(0.0);
                    progress_poll.pulse();
                    progress_poll.set_text(Some("Определяем трек…"));
                }
                soundcloud::Event::Meta { title, artist } => {
                    if !batch_poll.get() {
                        status_poll.style_context().remove_class("error");
                        status_poll.set_text(&format!("Найден трек: {artist} — {title}"));
                    }
                }
                soundcloud::Event::Downloading { received, total } => {
                    if batch_poll.get() {
                        let index = batch_index_poll.get();
                        let count = batch_total_poll.get();
                        if count > 0 {
                            progress_poll
                                .set_fraction(index.saturating_sub(1) as f64 / count as f64);
                            progress_poll.set_text(Some(&format!(
                                "{index} из {count} — {}",
                                format_bytes(received)
                            )));
                        } else {
                            progress_poll.pulse();
                        }
                        continue;
                    }
                    match total.filter(|total| *total > 0) {
                        Some(total) => {
                            let fraction = (received as f64 / total as f64).clamp(0.0, 1.0);
                            progress_poll.set_fraction(fraction);
                            progress_poll.set_text(Some(&format!(
                                "{} из {}",
                                format_bytes(received),
                                format_bytes(total)
                            )));
                        }
                        None => {
                            progress_poll.pulse();
                            progress_poll
                                .set_text(Some(&format!("Скачано {}", format_bytes(received))));
                        }
                    }
                }
                soundcloud::Event::Processing => {
                    progress_poll.pulse();
                    progress_poll.set_text(Some("Проверяем файл и обложку…"));
                    status_poll.set_text("Обрабатываем метаданные…");
                }
                soundcloud::Event::Done(outcome) => {
                    if batch_poll.get() {
                        collected_poll.borrow_mut().push(outcome.path);
                    } else {
                        progress_poll.set_fraction(1.0);
                        progress_poll.set_text(Some("Готово"));
                        status_poll.style_context().remove_class("error");
                        if outcome.already {
                            status_poll.set_text(&format!(
                                "Этот трек уже скачан: {}",
                                outcome.path.to_string_lossy()
                            ));
                        } else {
                            status_poll.set_text(&format!(
                                "Скачано: {} — {}",
                                outcome.artist,
                                outcome
                                    .path
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| outcome.title.clone())
                            ));
                        }
                        append_paths(&model_poll, vec![outcome.path]);
                        finished = true;
                    }
                }
                soundcloud::Event::LibraryCount(found) => {
                    progress_poll.pulse();
                    progress_poll.set_text(Some(&format!("Найдено {found} записей")));
                }
                soundcloud::Event::BatchStarted { total, skipped } => {
                    batch_total_poll.set(total);
                    batch_index_poll.set(0);
                    progress_poll.set_fraction(0.0);
                    progress_poll.set_text(Some(&format!("0 из {total}")));
                    status_poll.style_context().remove_class("error");
                    status_poll
                        .set_text(&format!("К загрузке {total} треков. Пропущено: {skipped}."));
                }
                soundcloud::Event::BatchItem {
                    index,
                    total,
                    title,
                    artist,
                } => {
                    batch_index_poll.set(index);
                    if total > 0 {
                        progress_poll.set_fraction((index.saturating_sub(1)) as f64 / total as f64);
                    }
                    progress_poll.set_text(Some(&format!("{index} из {total}")));
                    status_poll.set_text(&format!("Скачиваем: {artist} — {title}"));
                }
                soundcloud::Event::BatchFinished {
                    downloaded,
                    already,
                    failed,
                    skipped,
                    cancelled,
                    report,
                } => {
                    let paths: Vec<PathBuf> = collected_poll.borrow_mut().drain(..).collect();
                    if !paths.is_empty() {
                        append_paths(&model_poll, paths);
                    }
                    status_poll.style_context().remove_class("error");
                    let headline = if cancelled {
                        "Пакетная загрузка остановлена."
                    } else if downloaded == 0 && already == 0 && failed == 0 {
                        "Нет треков, доступных для скачивания."
                    } else {
                        "Библиотека обработана."
                    };
                    let mut text = format!(
                        "{headline} Скачано: {downloaded}. Уже было: {already}. Пропущено: {skipped}. Ошибок: {failed}."
                    );
                    if let Some(report) = report {
                        text.push_str(&format!(" Отчёт: {}", report.to_string_lossy()));
                    }
                    status_poll.set_text(&text);
                    if cancelled {
                        progress_poll.set_fraction(0.0);
                        progress_poll.set_text(Some("Остановлено"));
                    } else {
                        progress_poll.set_fraction(1.0);
                        progress_poll.set_text(Some("Готово"));
                    }
                    finished = true;
                }
                soundcloud::Event::Failed(message) => {
                    if batch_poll.get() {
                        let paths: Vec<PathBuf> = collected_poll.borrow_mut().drain(..).collect();
                        if !paths.is_empty() {
                            append_paths(&model_poll, paths);
                        }
                    }
                    progress_poll.set_fraction(0.0);
                    progress_poll.set_text(Some("Ошибка"));
                    status_poll.style_context().add_class("error");
                    status_poll.set_text(&message);
                    finished = true;
                }
                soundcloud::Event::Cancelled => {
                    if batch_poll.get() {
                        let paths: Vec<PathBuf> = collected_poll.borrow_mut().drain(..).collect();
                        if !paths.is_empty() {
                            append_paths(&model_poll, paths);
                        }
                    }
                    progress_poll.set_fraction(0.0);
                    progress_poll.set_text(Some("Отменено"));
                    status_poll.set_text("Загрузка отменена, временные файлы удалены.");
                    finished = true;
                }
            }
        }
        if finished {
            busy_poll.set(false);
            batch_poll.set(false);
            url_poll.set_sensitive(true);
            folder_button_poll.set_sensitive(true);
            auth_button_poll.set_sensitive(true);
            library_poll.set_sensitive(true);
            download_poll.set_sensitive(true);
            cancel_poll.set_visible(false);
            job_poll.borrow_mut().take();
        }
        glib::ControlFlow::Continue
    });

    motion::animate_buttons(dialog.upcast_ref());
    dialog.show_all();
    cancel.set_visible(false);
    dialog.run();
    running.set(false);
    if let Some(job) = job.borrow().as_ref() {
        job.cancel();
    }
    dialog.close();
}

fn show_soundcloud_token_dialog(parent: &gtk::Dialog) {
    let dialog = gtk::Dialog::with_buttons(
        Some("Доступ к SoundCloud"),
        Some(parent),
        gtk::DialogFlags::MODAL,
        &[("Закрыть", gtk::ResponseType::Close)],
    );
    dialog.set_default_size(520, 340);
    dialog.set_resizable(false);
    dialog.style_context().add_class("settings-dialog");
    let content = dialog.content_area();
    content.set_margin_top(16);
    content.set_margin_bottom(12);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_spacing(10);

    let heading = gtk::Label::new(Some("Токен сессии"));
    heading.set_xalign(0.0);
    heading.style_context().add_class("heading");
    content.pack_start(&heading, false, false, 0);

    let note = gtk::Label::new(Some(
        "Токен нужен только для треков с кнопкой Download. Он хранится в связке ключей (secret-tool), не попадает в файлы настроек, журналы и аргументы команд. Если связка недоступна, токен живёт только до закрытия плеера.",
    ));
    note.set_line_wrap(true);
    note.set_max_width_chars(50);
    note.set_xalign(0.0);
    note.style_context().add_class("settings-note");
    content.pack_start(&note, false, false, 0);

    let entry = gtk::Entry::new();
    entry.set_visibility(false);
    entry.set_input_purpose(gtk::InputPurpose::Password);
    entry.set_placeholder_text(Some("Вставь значение токена"));
    content.pack_start(&entry, false, false, 0);

    let status = gtk::Label::new(Some(soundcloud::token_summary()));
    status.set_xalign(0.0);
    status.set_line_wrap(true);
    status.set_max_width_chars(50);
    status.style_context().add_class("settings-note");
    content.pack_start(&status, false, false, 0);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let clear = gtk::Button::with_label("Удалить сохранённый");
    clear.style_context().add_class("ghost-button");
    let save = gtk::Button::with_label("Сохранить");
    save.style_context().add_class("tonal-button");
    buttons.pack_start(&clear, false, false, 0);
    buttons.pack_end(&save, false, false, 0);
    content.pack_start(&buttons, false, false, 4);

    let entry_save = entry.clone();
    let status_save = status.clone();
    save.connect_clicked(move |_| {
        let raw = entry_save.text();
        let token = raw
            .trim()
            .strip_prefix("OAuth ")
            .unwrap_or(raw.trim())
            .to_string();
        if let Err(error) = soundcloud::validate_token(&token) {
            status_save.set_text(&error);
            return;
        }
        match soundcloud::save_token(&token) {
            Ok(()) => {
                soundcloud::set_session_token(Some(token));
                entry_save.set_text("");
                status_save.set_text(
                    "Токен сохранён в связке ключей. Проверка доступа выполнится при загрузке.",
                );
            }
            Err(error) => {
                soundcloud::set_session_token(Some(token));
                entry_save.set_text("");
                status_save.set_text(&format!(
                    "{error} Токен будет действовать только до закрытия плеера."
                ));
            }
        }
    });

    let status_clear = status.clone();
    clear.connect_clicked(move |_| {
        if let Err(error) = soundcloud::clear_stored_token() {
            status_clear.set_text(&error);
            return;
        }
        soundcloud::set_session_token(None);
        status_clear.set_text("Сохранённый токен удалён.");
    });

    motion::animate_buttons(dialog.upcast_ref());
    dialog.show_all();
    dialog.run();
    dialog.close();
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn active_window() -> Option<gtk::Window> {
    gtk::gio::Application::default()
        .and_then(|app| app.downcast::<gtk::Application>().ok())
        .and_then(|app| app.active_window())
}

fn choose_files(folder: bool) -> Option<Vec<PathBuf>> {
    let action = if folder {
        gtk::FileChooserAction::SelectFolder
    } else {
        gtk::FileChooserAction::Open
    };
    let title = if folder {
        "Добавить папку с музыкой"
    } else {
        "Добавить аудиофайлы"
    };
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some(title),
        active_window().as_ref(),
        action,
        &[
            ("Отмена", gtk::ResponseType::Cancel),
            ("Добавить", gtk::ResponseType::Accept),
        ],
    );
    if !folder {
        dialog.set_select_multiple(true);
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Аудиофайлы"));
        filter.add_mime_type("audio/*");
        for extension in [
            "mp3", "m4a", "flac", "wav", "ogg", "opus", "aac", "aiff", "ape", "oga", "wma", "webm",
        ] {
            filter.add_pattern(&format!("*.{extension}"));
            filter.add_pattern(&format!("*.{}", extension.to_uppercase()));
        }
        dialog.add_filter(filter);
        let all = gtk::FileFilter::new();
        all.set_name(Some("Все файлы"));
        all.add_pattern("*");
        dialog.add_filter(all);
    }
    let result = if dialog.run() == gtk::ResponseType::Accept {
        dialog.filenames()
    } else {
        Vec::new()
    };
    dialog.close();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn format_time(seconds: f64) -> String {
    let value = seconds.max(0.0) as u64;
    format!("{}:{:02}", value / 60, value % 60)
}

#[cfg(test)]
mod ui_tests;
