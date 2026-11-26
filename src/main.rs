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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod artwork;
mod cover_art;
mod library;
mod lyric_sources;
mod lyric_timing;
mod lyrics;
mod motion;
mod mpv;
mod now_playing;
mod playback_controls;
mod session;
mod settings;
mod shuffle;
mod theme;
mod track_menu;
mod ui_updates;
mod vinyl;
mod yandex_export;
use library::{collect_audio, extract_cover, preview_track, read_track};
#[cfg(test)]
use lyrics::online_lyrics;
use mpv::{Mpv, MpvProcess, ReadReady};
use session::{Appearance, Session};
use settings::build_settings_page;
use shuffle::ShuffleOrder;
use ui_updates::UiUpdates;

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

struct LyricsJob {
    token: u64,
    cancel: Arc<AtomicBool>,
}

struct Model {
    updates: Rc<UiUpdates>,
    tracks: Vec<Track>,
    current: Option<usize>,
    library_revision: u64,
    queue_metadata_changed: std::collections::BTreeSet<usize>,
    cover_pending: std::collections::HashMap<PathBuf, CoverLookup>,
    cover_attempted: std::collections::HashSet<PathBuf>,
    metadata_ready: std::collections::HashSet<PathBuf>,
    metadata_pending: std::collections::HashSet<PathBuf>,
    lyrics_pending: std::collections::HashSet<PathBuf>,
    lyrics_results: std::collections::HashMap<PathBuf, String>,
    lyrics_jobs: std::collections::HashMap<PathBuf, LyricsJob>,
    lyrics_sequence: u64,
    shuffle: bool,
    shuffle_order: ShuffleOrder,
    play_next: Option<PathBuf>,
    repeat: usize,
    online_covers: bool,
    volume: f64,
    muted: bool,
    appearance: Appearance,
    speed: f64,
    status: Option<String>,
    imports_pending: usize,
    import_generation: u64,
    failed_tracks: std::collections::HashSet<usize>,
    mpv: Option<Mpv>,
    mpv_process: Option<MpvProcess>,
}

fn request_ui(model: &Rc<RefCell<Model>>) {
    model.borrow().updates.request();
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "lyrics-check") {
        if let Err(error) = lyric_timing::run(&args[2..]) {
            eprintln!("Проверка текста: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).is_some_and(|arg| arg == "ym-export") {
        if let Err(error) = yandex_export::run(&args[2..]) {
            eprintln!("Не удалось экспортировать плейлист: {error}");
            std::process::exit(1);
        }
        return;
    }
    let Ok(args) = args
        .into_iter()
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("Аргументы запуска Veyra должны быть в UTF-8.");
        std::process::exit(1);
    };
    let app = gtk::Application::new(Some("local.ownplayer.desktop"), Default::default());
    app.connect_startup(|_| {
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(true);
        }
        if let Some(screen) = gtk::gdk::Screen::default() {
            theme::install(&screen);
        }
    });
    app.connect_activate(build_ui);
    app.run_with_args(&args);
}

fn build_ui(app: &gtk::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let model = Rc::new(RefCell::new(Model {
        updates: Rc::default(),
        tracks: Vec::new(),
        current: None,
        library_revision: 0,
        queue_metadata_changed: Default::default(),
        cover_pending: Default::default(),
        cover_attempted: Default::default(),
        metadata_ready: Default::default(),
        metadata_pending: Default::default(),
        lyrics_pending: Default::default(),
        lyrics_results: Default::default(),
        lyrics_jobs: Default::default(),
        lyrics_sequence: 0,
        shuffle: false,
        shuffle_order: ShuffleOrder::default(),
        play_next: None,
        repeat: 0,
        online_covers: true,
        volume: 75.0,
        muted: false,
        appearance: Appearance::default(),
        speed: 1.0,
        status: None,
        imports_pending: 0,
        import_generation: 0,
        failed_tracks: Default::default(),
        mpv: None,
        mpv_process: None,
    }));
    restore_session(&model);
    let appearance = model.borrow().appearance;
    theme::set_hue(None);
    theme::set_dynamic(appearance.dynamic_background);
    set_decorative_motion(appearance.animations);
    let window = gtk::ApplicationWindow::new(app);
    set_style_class(&window, "pointer-focus", !window.gets_focus_visible());
    window.connect_focus_visible_notify(|window| {
        set_style_class(window, "pointer-focus", !window.gets_focus_visible());
    });
    window.set_title("Veyra");
    window.set_default_size(1160, 760);
    window.set_size_request(640, 460);
    window.set_position(gtk::WindowPosition::Center);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.set_border_width(12);

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.set_size_request(80, -1);
    sidebar.style_context().add_class("sidebar");

    let brand = gtk::Box::new(gtk::Orientation::Vertical, 0);
    brand.set_halign(gtk::Align::Center);
    brand.set_size_request(40, 40);
    brand.set_margin_bottom(12);
    brand.style_context().add_class("brand-mark");
    let brand_icon = symbolize("audio-x-generic-symbolic", 20);
    brand_icon.style_context().add_class("brand-icon");
    brand.set_center_widget(Some(&brand_icon));
    sidebar.pack_start(&brand, false, false, 0);

    let pages = gtk::Stack::new();
    pages.set_hhomogeneous(false);
    pages.set_vhomogeneous(false);
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
        let button = rail_button(icon, &name, &name, "nav-item");
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
    let add_files = rail_button(
        "list-add-symbolic",
        "Файлы",
        "Добавить файлы",
        "sidebar-action",
    );
    let add_folder = rail_button(
        "folder-open-symbolic",
        "Папка",
        "Добавить папку",
        "sidebar-action",
    );
    let settings = rail_button(
        "preferences-system-symbolic",
        "Настройки",
        "Настройки",
        "sidebar-action",
    );
    sidebar.pack_start(&add_files, false, false, 0);
    sidebar.pack_start(&add_folder, false, false, 0);
    let sidebar_divider = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sidebar_divider.set_size_request(-1, 1);
    sidebar_divider.set_margin_top(8);
    sidebar_divider.set_margin_bottom(8);
    sidebar_divider.style_context().add_class("divider");
    sidebar.pack_start(&sidebar_divider, false, false, 0);
    sidebar.pack_start(&settings, false, false, 0);
    page_buttons.borrow_mut().push(settings.clone());

    apply_sidebar_mode(&sidebar, appearance.expanded_sidebar);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = gtk::Box::new(gtk::Orientation::Vertical, 3);
    header.set_margin_start(8);
    let subheading = gtk::Label::new(None);
    subheading.set_xalign(0.0);
    subheading.set_line_wrap(true);
    subheading.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
    subheading.set_max_width_chars(72);
    subheading.set_no_show_all(true);
    subheading.style_context().add_class("status-banner");
    header.pack_start(&subheading, false, false, 0);
    subheading.set_margin_bottom(10);
    content.pack_start(&header, false, false, 0);

    let (player_page, _) = now_playing::build(&model, {
        let pages = pages.downgrade();
        let buttons = Rc::downgrade(&page_buttons);
        move || {
            if let (Some(pages), Some(buttons)) = (pages.upgrade(), buttons.upgrade()) {
                set_page(&pages, &buttons, 1);
            }
        }
    });
    let (queue_page, queue_search) = build_queue_page(&model);
    pages.add_named(&player_page, "Плеер");
    pages.add_named(&queue_page, "Очередь");
    pages.add_named(&build_settings_page(&model, &sidebar), "Настройки");
    content.pack_start(&pages, true, true, 0);
    let controls = playback_controls::build(&model);

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
    let keyboard_model = model.clone();
    window.connect_key_press_event(move |window, event| {
        let key = event.keyval();
        let modifiers = event.state() & gtk::accelerator_get_default_mod_mask();
        if key == gtk::gdk::keys::constants::f && modifiers == gtk::gdk::ModifierType::CONTROL_MASK
        {
            if let (Some(pages), Some(buttons)) =
                (settings_pages.upgrade(), settings_buttons.upgrade())
            {
                set_page(&pages, &buttons, 1);
                queue_search.grab_focus();
                return glib::Propagation::Stop;
            }
        }
        if key == gtk::gdk::keys::constants::Escape {
            if let (Some(pages), Some(buttons)) =
                (settings_pages.upgrade(), settings_buttons.upgrade())
            {
                if pages.visible_child_name().as_deref() == Some(PAGE_NAMES[2]) {
                    set_page(&pages, &buttons, previous_page.get());
                    return glib::Propagation::Stop;
                }
            }
        }
        if playback_controls::shortcut(window, event, &keyboard_model) {
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    let model_close = model.clone();
    window.connect_delete_event(move |_, _| {
        save_session(&model_close);
        session::flush();
        glib::Propagation::Proceed
    });

    let model_destroy = model.clone();
    window.connect_destroy(move |_| shutdown_mpv(&mut model_destroy.borrow_mut()));
    let buttons_lifetime = page_buttons.clone();
    window.connect_destroy(move |_| buttons_lifetime.borrow_mut().clear());
    let model_count = model.clone();
    let subheading_update = subheading.clone();
    let updates = model.borrow().updates.clone();
    updates.listen(&window, move || {
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
    });
    start_mpv(&model, app);

    sync_stack_motion(window.upcast_ref());
    motion::animate_buttons(window.upcast_ref());
    window.show_all();
    window.set_focus(gtk::Widget::NONE);
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

fn rail_button(icon: &str, label: &str, tooltip: &str, class: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.style_context().add_class(class);
    button.set_focus_on_click(false);
    describe_control(&button, tooltip);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.set_halign(gtk::Align::Center);
    column.set_valign(gtk::Align::Center);
    column.pack_start(&symbolize(icon, 24), false, false, 0);
    column.pack_start(&gtk::Label::new(Some(label)), false, false, 0);
    button.add(&column);
    button
}

fn apply_sidebar_mode(sidebar: &gtk::Box, expanded: bool) {
    sidebar.set_width_request(if expanded { 156 } else { 80 });
    set_style_class(sidebar, "expanded", expanded);
    for button in sidebar
        .children()
        .into_iter()
        .filter_map(|child| child.downcast::<gtk::Button>().ok())
    {
        if let Some(content) = button
            .child()
            .and_then(|child| child.downcast::<gtk::Box>().ok())
        {
            content.set_orientation(if expanded {
                gtk::Orientation::Horizontal
            } else {
                gtk::Orientation::Vertical
            });
            content.set_spacing(if expanded { 12 } else { 4 });
            content.set_halign(if expanded {
                gtk::Align::Start
            } else {
                gtk::Align::Center
            });
        }
    }
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

type RenderedCover = Option<(Option<PathBuf>, Option<PathBuf>, i32)>;

// Keep the visible image until the selected track's cover lookup and decode settle.
fn current_artwork(
    track: &Track,
    rendered: &mut RenderedCover,
    state: &Model,
) -> Option<Option<Rc<artwork::Artwork>>> {
    let missing_cover_track = track.cover_path.is_none().then_some(&track.path);
    let pixels = artwork::hero_pixels();
    if rendered
        .as_ref()
        .map(|(cover, track, pixels)| (cover.as_ref(), track.as_ref(), *pixels))
        == Some((track.cover_path.as_ref(), missing_cover_track, pixels))
        || (missing_cover_track.is_some() && state.cover_pending.contains_key(&track.path))
    {
        return None;
    }
    let image = artwork::get(track.cover_path.as_deref(), true, &state.updates)?;
    *rendered = Some((
        track.cover_path.clone(),
        missing_cover_track.cloned(),
        pixels,
    ));
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
    const SEEK_HINT: &str = "Двойной щелчок по строке — перейти к этому моменту";
    let overlay = gtk::Overlay::new();
    overlay.style_context().add_class("lyrics-overlay");
    overlay.connect_size_allocate(|overlay, allocation| {
        set_style_class(
            overlay,
            "compact-lyrics",
            allocation.width() < 700 || allocation.height() < 440,
        );
    });

    let backdrop = gtk::DrawingArea::new();
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
            let radius = 20.0;
            let half_pi = std::f64::consts::FRAC_PI_2;
            cr.new_sub_path();
            cr.arc(width - radius, radius, radius, -half_pi, 0.0);
            cr.arc(width - radius, height - radius, radius, 0.0, half_pi);
            cr.arc(radius, height - radius, radius, half_pi, 2.0 * half_pi);
            cr.arc(radius, radius, radius, 2.0 * half_pi, 3.0 * half_pi);
            cr.close_path();
            cr.clip();
            // The stage supplies the rounded base. Paint only the faint artwork tint.
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
                let _ = cr.paint_with_alpha(opacity * 0.09);
                let _ = cr.restore();
            }
            glib::Propagation::Proceed
        });
    }
    overlay.add(&backdrop);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(12);
    page.set_margin_end(12);
    overlay.add_overlay(&page);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.set_margin_start(12);
    header.set_margin_end(12);
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
    let sync_mode = gtk::Label::new(None);
    sync_mode.set_xalign(0.0);
    sync_mode.style_context().add_class("lyrics-sync-mode");
    sync_mode.set_tooltip_text(Some("Пословные метки берутся из текста. Для построчного LRC движение внутри строки рассчитывается приблизительно."));
    titles.pack_start(&sync_mode, false, false, 0);
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
    let empty_icon = symbolize("format-justify-left-symbolic", 40);
    empty_icon.style_context().add_class("empty-glyph");
    empty_box.pack_start(&empty_icon, false, false, 0);
    let empty = gtk::Label::new(Some("Выбери трек, чтобы увидеть текст"));
    empty.set_line_wrap(true);
    empty.set_justify(gtk::Justification::Center);
    empty.set_max_width_chars(46);
    empty.style_context().add_class("empty-state");
    empty_box.pack_start(&empty, false, false, 0);
    let search_note = gtk::Label::new(Some("Можно добавить файл .lrc рядом с аудиофайлом."));
    search_note.style_context().add_class("lyrics-hint");
    search_note.set_line_wrap(true);
    empty_box.pack_start(&search_note, false, false, 0);
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
    lyrics_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
    lyrics_scroll.set_overlay_scrolling(false);
    lyrics_scroll.set_shadow_type(gtk::ShadowType::None);
    lyrics_scroll.style_context().add_class("lyrics-scroll");
    lyrics_scroll.add(&content);
    content.connect_size_allocate({
        let list = lyrics_list.downgrade();
        move |_, allocation| {
            if let Some(list) = list.upgrade() {
                let extra = (allocation.width() - 900).max(0);
                let start = extra / 2;
                let end = extra - start;
                if (list.margin_start(), list.margin_end()) != (start, end) {
                    list.set_margin_start(start);
                    list.set_margin_end(end);
                }
            }
        }
    });

    body.add_named(&lyrics_scroll, "list");
    body.set_visible_child_name("empty");
    page.pack_start(&body, true, true, 0);

    let hint = gtk::Label::new(Some(SEEK_HINT));
    hint.set_xalign(0.5);
    hint.set_line_wrap(true);
    hint.set_justify(gtk::Justification::Center);
    hint.style_context().add_class("lyrics-hint");
    hint.set_no_show_all(true);
    hint.set_halign(gtk::Align::Center);
    hint.set_valign(gtk::Align::End);
    hint.set_margin_bottom(12);
    overlay.add_overlay(&hint);
    overlay.set_overlay_pass_through(&hint, true);
    overlay.connect_unmap({
        let hint = hint.clone();
        move |_| hint.hide()
    });

    let views_back = views.downgrade();
    back.connect_clicked(move |_| {
        if let Some(views) = views_back.upgrade() {
            views.set_visible_child_name("artwork");
        }
    });

    let model_find = model.clone();
    find.connect_clicked(move |_| start_lyrics_job(&model_find, true));
    let updates = model.borrow().updates.clone();
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
        if let (Some(time), Some(player)) = (time, state.mpv.as_mut()) {
            if playback_controls::seek_absolute(player, time) {
                seek_browsing.set(Instant::now());
                row_seek.set(true);
            }
        }
        state.updates.request();
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
        let updates = updates.clone();
        let timeout = Rc::new(RefCell::new(None::<glib::SourceId>));
        let active_timeout = timeout.clone();
        lyrics_scroll.connect_destroy(move |_| {
            if let Some(source) = active_timeout.take() {
                source.remove();
            }
        });
        move || {
            let cooldown = Duration::from_secs(5);
            browsing.set(Instant::now() + cooldown);
            motion.snap(adjustment.value());
            updates.request();
            if let Some(source) = timeout.take() {
                source.remove();
            }
            let pending = timeout.clone();
            let updates = Rc::downgrade(&updates);
            timeout.replace(Some(glib::timeout_add_local_once(cooldown, move || {
                pending.take();
                if let Some(updates) = updates.upgrade() {
                    updates.request();
                }
            })));
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
    resume.connect_clicked({
        let updates = updates.clone();
        move |_| {
            browsing.set(Instant::now());
            updates.request();
        }
    });
    adjustment.connect_changed({
        let updates = updates.clone();
        move |_| updates.request()
    });
    lyrics_scroll.connect_size_allocate({
        let updates = updates.clone();
        move |_, _| updates.request()
    });

    let model_update = model.clone();
    let list_update = lyrics_list.clone();
    let lyrics_adjustment = lyrics_scroll.vadjustment();
    let label_update = empty.clone();
    let body_update = body.clone();
    let title_update = lyrics_title.clone();
    let artist_update = lyrics_artist.clone();
    let backdrop_update = backdrop.clone();
    let backdrop_source_update = backdrop_source.clone();

    let lyrics_view = overlay.downgrade();
    let mut rendered_track: Option<PathBuf> = None;
    let mut rendered_cover = None;
    let mut rendered_lines: Vec<Lyric> = Vec::new();
    let mut rendered_labels = None;
    let mut focus_style: Option<(Option<usize>, bool)> = None;
    let clock_model = Rc::downgrade(model);
    let boundary_updates = Rc::downgrade(&updates);
    let letter_motion = motion::LyricHighlight::new(
        move || {
            let model = clock_model.upgrade()?;
            let state = model.borrow();
            let player = state.mpv.as_ref()?;
            Some((
                player.position(state.speed),
                player.loaded && !player.paused,
                state.speed,
            ))
        },
        move || {
            if let Some(updates) = boundary_updates.upgrade() {
                updates.request_layout();
            }
        },
    );
    for signal in ["unmap", "destroy"] {
        let motion = Rc::downgrade(&letter_motion);
        overlay.connect_local(signal, false, move |_| {
            if let Some(motion) = motion.upgrade() {
                motion.clear();
            }
            None
        });
    }
    let mut dynamic_background = model.borrow().appearance.dynamic_background;
    let mut synchronized = false;
    let mut active = None;
    let mut lyric_times = Vec::new();
    let mut lyric_rate = 6.5;
    let mut last_position: Option<f64> = None;
    let mut snap_follow = false;
    let mut rendered_status: Option<(bool, bool, bool, Option<String>)> = None;

    updates.listen(&overlay, move || {
        if !lyrics_view.upgrade().is_some_and(|view| view.is_mapped()) {
            return;
        }
        let show_hint = {
            let state = model_update.borrow();
            !state.appearance.lyrics_seek_hint_seen
                && state
                    .current
                    .and_then(|i| state.tracks.get(i))
                    .is_some_and(|track| track.lyrics.iter().any(|line| line.time.is_some()))
        };
        if show_hint {
            model_update.borrow_mut().appearance.lyrics_seek_hint_seen = true;
            save_session(&model_update);
            hint.show();
            let hint = hint.downgrade();
            glib::timeout_add_local_once(Duration::from_secs(5), move || {
                if let Some(hint) = hint.upgrade() {
                    hint.hide();
                }
            });
        }
        let state = model_update.borrow();
        if dynamic_background != state.appearance.dynamic_background {
            dynamic_background = state.appearance.dynamic_background;
            rendered_cover = None;
            backdrop_source_update.replace(None);
            backdrop_previous.replace(None);
            backdrop_mix.snap(1.0);
            backdrop_update.queue_draw();
        }
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
                            status.3.unwrap_or("Готовим поиск текста…")
                        });
                        hint.hide();
                    }
                    body_update.set_visible_child_name(if status.0 { "empty" } else { "list" });
                }
                if let Some(image) = dynamic_background
                    .then(|| current_artwork(track, &mut rendered_cover, &state))
                    .flatten()
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
                    let upgrade_only = same_lyric_shape(&rendered_lines, &track.lyrics);
                    let was_synchronized = synchronized;
                    rendered_lines.clone_from(&track.lyrics);
                    lyric_rate = lyric_timing::typical_rate(&rendered_lines);
                    lyric_times.clear();
                    lyric_times.extend(
                        rendered_lines
                            .iter()
                            .enumerate()
                            .filter_map(|(index, line)| line.time.map(|time| (time, index))),
                    );
                    lyric_times.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                    synchronized = !lyric_times.is_empty();
                    sync_mode.set_text(if synchronized {
                        "По строкам · приблизительно"
                    } else {
                        "Без таймкодов"
                    });
                    if !synchronized {
                        hint.hide();
                    }
                    if upgrade_only {
                        // Word cues arrived for the same lines. Keep the
                        // scroll position, focus and row widgets intact; only
                        // the active line's timing needs to change.
                        for (index, line) in rendered_lines.iter().enumerate() {
                            let Some(row) = list_update.row_at_index(index as i32) else {
                                continue;
                            };
                            row.set_tooltip_text(line.time.map(|_| SEEK_HINT));
                            let Some(text) = row
                                .child()
                                .and_then(|child| child.downcast::<gtk::Label>().ok())
                            else {
                                continue;
                            };
                            set_style_class(&text, "lyric-plain", !synchronized);
                            let clean = lyric_timing::markup(&line.text).0;
                            if text.text() != clean {
                                text.set_text(&clean);
                            }
                        }
                        if synchronized != was_synchronized {
                            last_position = None;
                            focus_style = None;
                        }
                    } else {
                        last_position = None;
                        focus_style = None;
                        letter_motion.clear();
                        for child in list_update.children() {
                            list_update.remove(&child);
                        }
                        for line in &track.lyrics {
                            let row = gtk::ListBoxRow::new();
                            row.style_context().add_class("lyric-row");
                            let text = gtk::Label::new(Some(&lyric_timing::markup(&line.text).0));
                            text.set_xalign(0.0);
                            text.set_line_wrap(true);
                            text.set_justify(gtk::Justification::Left);
                            text.set_margin_top(12);
                            text.set_margin_bottom(12);
                            text.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
                            text.set_halign(gtk::Align::Start);
                            if line.time.is_some() {
                                row.set_tooltip_text(Some(SEEK_HINT));
                            }
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
                }
                if synchronized && !track.lyrics.is_empty() && lyrics_scroll.is_mapped() {
                    let position = state
                        .mpv
                        .as_ref()
                        .map_or(0.0, |player| player.position(state.speed));
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
                    let highlighted = active.and_then(|index| {
                        if browsing || !animations_enabled() {
                            return None;
                        }
                        let line = &rendered_lines[index];
                        let start = line.time?;
                        let end = lyric_times
                            .iter()
                            .find(|(time, _)| *time > start)
                            .map_or_else(
                                || {
                                    state
                                        .mpv
                                        .as_ref()
                                        .filter(|mpv| {
                                            mpv.duration.is_finite() && mpv.duration > start
                                        })
                                        .map_or(track.duration, |mpv| mpv.duration)
                                },
                                |(time, _)| *time,
                            );
                        Some((index, start, end))
                    });
                    if let Some((index, start, end)) = highlighted {
                        if let Some(label) = lyric_label(&list_update, index) {
                            letter_motion.update_timed(
                                &label,
                                &rendered_lines[index].text,
                                start,
                                end,
                                lyric_rate,
                            );
                            let mode = if letter_motion.is_exact() {
                                "Пословная синхронизация"
                            } else {
                                "По строкам · приблизительно"
                            };
                            if sync_mode.text() != mode {
                                sync_mode.set_text(mode);
                            }
                        }
                    } else {
                        letter_motion.clear();
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
                            follow.animate_smooth(&lyrics_scroll, goal, 420, |scroll, value| {
                                scroll.vadjustment().set_value(value);
                            });
                        }
                    }
                } else {
                    letter_motion.clear();
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
                letter_motion.clear();
                rendered_status = None;
                resume.hide();
                find.hide();
                title_update.set_text("Текст песни");
                artist_update.set_text("Выбери трек");
                rendered_labels = None;
                label_update.set_text("Выбери трек");
                hint.hide();
                body_update.set_visible_child_name("empty");
            }
            if rendered_cover.take().is_some() {
                backdrop_source_update.replace(None);
                backdrop_previous.replace(None);
                backdrop_mix.snap(1.0);
                backdrop_update.queue_draw();
            }
        }
    });
    overlay.upcast()
}

fn same_lyric_shape(previous: &[Lyric], next: &[Lyric]) -> bool {
    previous.len() == next.len()
        && previous.iter().zip(next).all(|(old, new)| {
            lyric_timing::markup(&old.text).0 == lyric_timing::markup(&new.text).0
        })
}

fn lyric_label(list: &gtk::ListBox, index: usize) -> Option<gtk::Label> {
    list.row_at_index(index as i32)?.child()?.downcast().ok()
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

thread_local! {
    static DECORATIVE_MOTION: Cell<bool> = const { Cell::new(true) };
}

fn animations_enabled() -> bool {
    DECORATIVE_MOTION.with(Cell::get)
        && gtk::Settings::default()
            .is_none_or(|settings| settings.property::<bool>("gtk-enable-animations"))
}

fn set_decorative_motion(enabled: bool) {
    DECORATIVE_MOTION.with(|setting| setting.set(enabled));
    theme::set_motion(enabled);
}

fn sync_stack_motion(widget: &gtk::Widget) {
    if let Some(stack) = widget.downcast_ref::<gtk::Stack>() {
        stack.set_transition_duration(if DECORATIVE_MOTION.with(Cell::get) {
            180
        } else {
            0
        });
    }
    if let Some(container) = widget.downcast_ref::<gtk::Container>() {
        for child in container.children() {
            sync_stack_motion(&child);
        }
    }
}

struct QueueRow {
    row: gtk::ListBoxRow,
    path: Rc<RefCell<PathBuf>>,
    cover: gtk::Image,
    cover_stack: gtk::Stack,
    play: gtk::Button,
    rendered_cover: RefCell<Option<PathBuf>>,
    title: gtk::Label,
    artist: gtk::Label,
    album: gtk::Label,
    status: gtk::Label,
    duration: gtk::Label,
}

const QUEUE_OVERSCAN: usize = 4;

fn make_queue_row(
    model: &Rc<RefCell<Model>>,
    menu: &gtk::Menu,
    menu_target: &Rc<RefCell<PathBuf>>,
) -> QueueRow {
    let row = gtk::ListBoxRow::new();
    row.style_context().add_class("queue-row");
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let cover_stack = gtk::Stack::new();
    cover_stack.style_context().add_class("queue-cover");
    cover_stack.add_named(
        &cover_placeholder(artwork::QUEUE_THUMB_PIXELS, 16),
        "placeholder",
    );
    let cover = gtk::Image::new();
    cover.set_size_request(artwork::QUEUE_THUMB_PIXELS, artwork::QUEUE_THUMB_PIXELS);
    cover_stack.add_named(&cover, "image");
    cover_stack.set_visible_child_name("placeholder");
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
    titles.style_context().add_class("queue-track-copy");
    titles.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(None);
    title.set_xalign(0.0);
    title.set_single_line_mode(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(1);
    title.style_context().add_class("queue-title");
    let artist = gtk::Label::new(None);
    artist.set_xalign(0.0);
    artist.set_single_line_mode(true);
    artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    artist.set_max_width_chars(1);
    artist.style_context().add_class("queue-artist");
    let title_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let status = gtk::Label::new(None);
    status.style_context().add_class("queue-state");
    status.set_no_show_all(true);
    title_line.pack_start(&title, true, true, 0);
    title_line.pack_end(&status, false, false, 0);
    titles.pack_start(&title_line, false, false, 0);
    titles.pack_start(&artist, false, false, 0);
    let album = gtk::Label::new(None);
    album.set_xalign(0.0);
    album.set_single_line_mode(true);
    album.set_ellipsize(gtk::pango::EllipsizeMode::End);
    album.set_width_chars(1);
    album.set_max_width_chars(1);
    album.set_width_request(184);
    album.set_no_show_all(true);
    album.style_context().add_class("queue-album");
    let duration = gtk::Label::new(None);
    duration.style_context().add_class("queue-duration");
    duration.set_xalign(1.0);
    let artwork = gtk::Overlay::new();
    artwork.add(&cover_stack);
    let play = icon_button(
        "media-playback-start-symbolic",
        "Воспроизвести трек",
        "queue-play",
    );
    let path = Rc::new(RefCell::new(PathBuf::new()));
    play.connect_clicked({
        let model = model.clone();
        let path = path.clone();
        move |_| {
            let (index, current) = {
                let state = model.borrow();
                let index = state
                    .tracks
                    .iter()
                    .position(|track| track.path == *path.borrow());
                (
                    index,
                    state
                        .current
                        .filter(|_| state.mpv.as_ref().is_some_and(Mpv::has_track)),
                )
            };
            if let Some(index) = index {
                if current == Some(index) {
                    let mut state = model.borrow_mut();
                    if let Some(player) = state.mpv.as_mut() {
                        let _ = player.command(json!(["cycle", "pause"]));
                    }
                } else {
                    play_index(&model, index);
                }
            }
        }
    });
    artwork.add_overlay(&play);
    line.pack_start(&artwork, false, false, 0);
    line.pack_start(&titles, true, true, 0);
    line.pack_start(&album, false, false, 0);
    line.pack_start(&duration, false, false, 0);
    let actions = icon_button("view-more-symbolic", "Действия с треком", "queue-menu");
    actions.set_focus_on_click(true);
    actions.set_valign(gtk::Align::Center);
    let context_menu = menu.downgrade();
    let context_target = menu_target.clone();
    let context_path = path.clone();
    row.connect_button_press_event(move |row, event| {
        if event.triggers_context_menu() {
            if let Some(menu) = context_menu.upgrade() {
                context_target.replace(context_path.borrow().clone());
                menu.set_attach_widget(Some(row));
                menu.popup_at_pointer(Some(event));
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
    let menu = menu.downgrade();
    let menu_target = menu_target.clone();
    let menu_path = path.clone();
    actions.connect_clicked(move |button| {
        if let Some(menu) = menu.upgrade() {
            menu_target.replace(menu_path.borrow().clone());
            menu.set_attach_widget(Some(button));
            if let Some(event) = gtk::current_event() {
                menu.popup_at_widget(
                    button,
                    gtk::gdk::Gravity::SouthEast,
                    gtk::gdk::Gravity::NorthEast,
                    Some(&event),
                );
            } else {
                menu.popup_easy(0, gtk::current_event_time());
            }
        }
    });
    line.pack_start(&actions, false, false, 0);
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
    // Keep a press on the drag handle out of the list's double-click gesture.
    handle.connect_button_press_event(|_, event| {
        if event.button() == 1 {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
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

    row.show_all();
    QueueRow {
        row,
        path,
        cover,
        cover_stack,
        play,
        rendered_cover: RefCell::new(None),
        title,
        artist,
        album,
        status,
        duration,
    }
}

impl QueueRow {
    fn update(&self, track: &Track, index: usize, current: bool, next: bool) {
        if *self.path.borrow() != track.path {
            self.path.replace(track.path.clone());
            if self.rendered_cover.borrow().as_ref() != track.cover_path.as_ref() {
                self.clear_cover();
            }
        }
        set_style_class(&self.row, "queued-next", next);
        self.highlight(index, current);
        let subtitle = &track.artist;
        if self.album.text() != track.album {
            self.album.set_text(&track.album);
            self.album.set_tooltip_text(Some(&track.album));
        }
        let description = format!("{} — {} · {}", track.title, track.artist, track.album);
        if let Some(accessible) = self.row.accessible() {
            accessible.set_name(&description);
        }
        let duration = if track.duration > 0.0 {
            format_time(track.duration)
        } else {
            String::new()
        };
        if self.title.text() != track.title {
            self.title.set_text(&track.title);
            self.title.set_tooltip_text(Some(&track.title));
        }
        if self.artist.text().as_str() != subtitle {
            self.artist.set_text(subtitle);
            self.artist.set_tooltip_text(Some(subtitle));
        }
        if self.duration.text() != duration {
            self.duration.set_text(&duration);
        }
    }

    fn highlight(&self, _index: usize, current: bool) {
        self.row.set_tooltip_text(current.then_some("Текущий трек"));
        let status = if current {
            "Сейчас"
        } else if self.row.style_context().has_class("queued-next") {
            "Далее"
        } else {
            ""
        };
        if self.status.text() != status {
            self.status.set_text(status);
        }
        self.status.set_visible(!status.is_empty());
        set_style_class(&self.row, "playing", current);
    }

    fn clear_cover(&self) {
        self.rendered_cover.replace(None);
        self.cover_stack.set_visible_child_name("placeholder");
        self.cover.clear();
    }
}

fn queue_search_key(track: &Track) -> String {
    format!("{} {} {}", track.title, track.artist, track.album).to_lowercase()
}

fn set_style_class(widget: &impl IsA<gtk::Widget>, class: &str, enabled: bool) {
    if enabled {
        widget.style_context().add_class(class);
    } else {
        widget.style_context().remove_class(class);
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
    state.updates.request();
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
            if state.play_next.as_deref() != Some(path) {
                let previous = state.play_next.replace(path.to_path_buf());
                if let Some(previous) = previous {
                    if let Some(index) =
                        state.tracks.iter().position(|track| track.path == previous)
                    {
                        state.queue_metadata_changed.insert(index);
                    }
                }
                state.queue_metadata_changed.insert(index);
            }
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
        forget_cover_lookup(&mut state, path);
        forget_lyrics_lookup(&mut state, path);
        state.metadata_ready.remove(path);
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
        && state
            .mpv
            .as_ref()
            .is_some_and(|p| p.has_track() && !p.paused))
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
    state.import_generation = state.import_generation.wrapping_add(1);
    state.status = None;
    state.tracks.clear();
    for job in state.lyrics_jobs.values() {
        job.cancel.store(true, Ordering::Relaxed);
    }
    state.lyrics_jobs.clear();
    state.lyrics_pending.clear();
    state.lyrics_results.clear();
    state.metadata_ready.clear();
    state.cover_attempted.clear();
    for lookup in state.cover_pending.values_mut() {
        *lookup = CoverLookup::Removed;
    }
    state.current = None;
    state.play_next = None;
    state.shuffle_order = ShuffleOrder::default();
    state.failed_tracks.clear();
    state.library_revision += 1;
    drop(state);
    save_session(model);
}

fn build_queue_page(model: &Rc<RefCell<Model>>) -> (gtk::Widget, gtk::SearchEntry) {
    let row_menu = gtk::Menu::new();
    let menu_target = Rc::new(RefCell::new(PathBuf::new()));
    for (label, action) in [
        ("Воспроизвести", QueueEdit::Play),
        ("Поставить следующим", QueueEdit::PlayNext),
        ("Переместить выше", QueueEdit::Up),
        ("Переместить ниже", QueueEdit::Down),
        ("Убрать из очереди", QueueEdit::Remove),
    ] {
        let item = gtk::MenuItem::with_label(label);
        if matches!(action, QueueEdit::PlayNext) {
            let item = item.downgrade();
            let model = model.clone();
            let target = menu_target.clone();
            row_menu.connect_map(move |_| {
                let state = model.borrow();
                let current = state.current.and_then(|index| state.tracks.get(index));
                if let Some(item) = item.upgrade() {
                    item.set_visible(current.is_none_or(|track| track.path != *target.borrow()));
                }
            });
        }
        let model = model.clone();
        let path = menu_target.clone();
        item.connect_activate(move |_| {
            edit_queue(&model, &path.borrow(), action);
        });
        row_menu.append(&item);
    }
    row_menu.append(&gtk::SeparatorMenuItem::new());
    track_menu::add_details(&row_menu, model, &menu_target);
    row_menu.show_all();

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.style_context().add_class("queue-page");
    let summary = gtk::Label::new(None);
    summary.set_xalign(0.0);
    summary.style_context().add_class("queue-summary");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.style_context().add_class("queue-header");
    let heading = gtk::Label::new(Some("Очередь"));
    heading.style_context().add_class("queue-heading");
    heading.set_xalign(0.0);
    let heading_copy = gtk::Box::new(gtk::Orientation::Vertical, 4);
    heading_copy.style_context().add_class("queue-heading-copy");
    heading_copy.pack_start(&heading, false, false, 0);
    heading_copy.pack_start(&summary, false, false, 0);
    header.pack_start(&heading_copy, true, true, 0);
    page.pack_start(&header, false, false, 0);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.style_context().add_class("queue-toolbar");
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Найти в очереди"));
    describe_control(&search, "Поиск по названию, исполнителю и альбому");
    search.connect_stop_search(|entry| entry.set_text(""));
    search.style_context().add_class("search");
    search.set_width_chars(1);
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
    let add = gtk::Button::with_label("Добавить");
    add.set_image(Some(&symbolize("list-add-symbolic", 18)));
    add.set_always_show_image(true);
    add.set_valign(gtk::Align::Center);
    add.style_context().add_class("queue-add");
    describe_control(&add, "Добавить музыку в очередь");
    queue_actions.set_valign(gtk::Align::Center);
    let model_add = model.clone();
    add.connect_clicked(move |_| {
        if let Some(paths) = choose_files(false) {
            append_paths(&model_add, paths);
        }
    });
    header.pack_end(&queue_actions, false, false, 0);
    header.pack_end(&add, false, false, 0);
    page.pack_start(&toolbar, false, false, 0);
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    columns.style_context().add_class("queue-columns");
    let track_heading = gtk::Label::new(Some("Трек"));
    track_heading.set_xalign(0.0);
    columns.pack_start(&track_heading, true, true, 0);
    let album_heading = gtk::Label::new(Some("Альбом"));
    album_heading.set_xalign(0.0);
    album_heading.set_width_request(184);
    album_heading.set_no_show_all(true);
    columns.pack_start(&album_heading, false, false, 0);
    let duration_heading = symbolize("preferences-system-time-symbolic", 14);
    duration_heading.set_tooltip_text(Some("Длительность"));
    duration_heading
        .style_context()
        .add_class("queue-duration-heading");
    columns.pack_start(&duration_heading, false, false, 0);
    page.pack_start(&columns, false, false, 0);
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
    let empty_add = gtk::Button::with_label("Выбрать музыку");
    empty_add.style_context().add_class("queue-empty-action");
    empty_add.connect_clicked({
        let add = add.clone();
        move |_| add.emit_clicked()
    });
    empty_box.pack_start(&empty_add, false, false, 8);
    let empty_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    empty_panel.style_context().add_class("queue-panel");
    empty_panel.pack_start(&empty_box, true, true, 0);
    body.add_named(&empty_panel, "empty");
    let no_results = gtk::Box::new(gtk::Orientation::Vertical, 12);
    no_results.set_halign(gtk::Align::Center);
    no_results.set_valign(gtk::Align::Center);
    let no_icon = symbolize("system-search-symbolic", 32);
    no_icon.style_context().add_class("empty-glyph");
    no_results.pack_start(&no_icon, false, false, 0);
    let no_title = gtk::Label::new(Some("Ничего не нашлось"));
    no_title.style_context().add_class("empty-title");
    let no_note = gtk::Label::new(Some("Попробуй другое название, исполнителя или альбом"));
    no_note.set_justify(gtk::Justification::Center);
    no_note.set_line_wrap(true);
    no_note.set_max_width_chars(36);
    no_note.style_context().add_class("secondary");
    let clear = gtk::Button::with_label("Сбросить поиск");
    clear.set_halign(gtk::Align::Center);
    let search_clear = search.clone();
    clear.connect_clicked(move |_| search_clear.set_text(""));
    no_results.pack_start(&no_title, false, false, 0);
    no_results.pack_start(&no_note, false, false, 0);
    no_results.pack_start(&clear, false, false, 8);
    let results_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    results_panel.style_context().add_class("queue-panel");
    results_panel.pack_start(&no_results, true, true, 0);
    body.add_named(&results_panel, "no-results");

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.style_context().add_class("queue-card");
    scroll.set_overlay_scrolling(true);
    scroll.set_shadow_type(gtk::ShadowType::None);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    list.style_context().add_class("queue-list");
    let layout = gtk::Layout::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    layout.put(&list, 4, 0);
    scroll.add(&layout);
    // ListBox focus uses local row coordinates. The layout adjustment spans the full queue.
    list.set_adjustment(gtk::Adjustment::NONE);
    body.add_named(&scroll, "list");
    body.set_visible_child_name("empty");
    page.pack_start(&body, true, true, 0);

    let updates = model.borrow().updates.clone();
    let jump_button = gtk::Button::with_label("К текущему треку");
    jump_button.style_context().add_class("queue-jump");
    toolbar.pack_end(&jump_button, false, false, 0);
    let jump = gtk::MenuItem::with_label("Показать текущий трек");
    menu.prepend(&jump);
    jump.show();
    let jump_requested = Rc::new(Cell::new(false));
    let jump_layout_pending = Rc::new(Cell::new(false));
    let request = jump_requested.clone();
    let jump_search = search.clone();
    jump.connect_activate({
        let updates = updates.clone();
        move |_| {
            jump_search.set_text("");
            request.set(true);
            updates.request();
        }
    });

    jump_button.connect_clicked({
        let jump = jump.clone();
        move |_| {
            jump.activate();
        }
    });
    updates.listen(&jump_button, {
        let model = Rc::downgrade(model);
        let button = jump_button.downgrade();
        move || {
            if let (Some(model), Some(button)) = (model.upgrade(), button.upgrade()) {
                button.set_sensitive(model.borrow().current.is_some());
            }
        }
    });

    let rows = Rc::new(RefCell::new(
        std::collections::BTreeMap::<usize, QueueRow>::new(),
    ));
    let navigation = Rc::new(Cell::new(None));
    let match_count = Rc::new(Cell::new(0_usize));
    list.connect_key_press_event({
        let navigation = navigation.clone();
        let match_count = match_count.clone();
        let rows = rows.clone();
        let adjustment = scroll.vadjustment();
        let updates = updates.clone();
        move |list, event| {
            use gtk::gdk::keys::constants as key;
            if event.state().intersects(
                gtk::gdk::ModifierType::SHIFT_MASK
                    | gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::MOD1_MASK,
            ) {
                return glib::Propagation::Proceed;
            }
            let rows = rows.borrow();
            let Some((&first, item)) = rows.first_key_value() else {
                return glib::Propagation::Proceed;
            };
            let height = f64::from(item.row.allocated_height().max(1));
            let page = (adjustment.page_size() / height).floor().max(1.0) as usize;
            let slot = navigation
                .get()
                .or_else(|| list.selected_row().map(|row| first + row.index() as usize))
                .unwrap_or_else(|| (adjustment.value() / height) as usize);
            let target = match event.keyval() {
                key::Up | key::KP_Up => slot.saturating_sub(1),
                key::Down | key::KP_Down => slot + 1,
                key::Page_Up | key::KP_Page_Up => slot.saturating_sub(page),
                key::Page_Down | key::KP_Page_Down => slot + page,
                key::Home | key::KP_Home => 0,
                key::End | key::KP_End => match_count.get().saturating_sub(1),
                _ => return glib::Propagation::Proceed,
            };
            navigation.set(Some(target.min(match_count.get().saturating_sub(1))));
            updates.request_layout();
            glib::Propagation::Stop
        }
    });

    let model_play = model.clone();
    let rows_play = rows.clone();
    list.connect_row_activated(move |_, row| {
        let rows = rows_play.borrow();
        let index = rows
            .first_key_value()
            .and_then(|(&first, _)| rows.get(&(first + row.index() as usize)))
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

    let query = Rc::new(RefCell::new(String::new()));
    let search_query = query.clone();
    search.connect_search_changed({
        let updates = updates.clone();
        move |entry| {
            search_query.replace(entry.text().trim().to_lowercase());
            updates.request();
        }
    });
    let adjustment = scroll.vadjustment();
    adjustment.connect_value_changed({
        let updates = updates.clone();
        move |_| updates.request_layout()
    });
    adjustment.connect_changed({
        let updates = updates.clone();
        move |_| updates.request_layout()
    });

    layout.connect_size_allocate({
        let list = list.downgrade();
        let updates = updates.clone();
        let pending = jump_layout_pending.clone();
        move |_, bounds| {
            if pending.replace(false) {
                updates.request_layout();
            }
            if list
                .upgrade()
                .is_some_and(|list| list.width_request() != (bounds.width() - 12).max(1))
            {
                updates.request_layout();
            }
        }
    });

    let model_update = model.clone();
    let rows_update = rows.clone();
    let page_update = page.downgrade();
    let mut rendered_revision = u64::MAX;
    let mut rendered_query = String::new();
    let mut search_keys = Vec::new();
    let mut matches = Vec::new();
    let mut row_height = 44;
    let mut rendered_top = usize::MAX;
    updates.listen(&page, move || {
        if !page_update.upgrade().is_some_and(|page| page.is_mapped()) {
            return;
        }
        let mut state = model_update.borrow_mut();
        let track_count = state.tracks.len();
        let order_changed = rendered_revision != state.library_revision;
        rendered_revision = state.library_revision;
        let changed = std::mem::take(&mut state.queue_metadata_changed);
        let query = query.borrow();
        let query_changed = rendered_query != *query;
        if order_changed {
            search_keys.clear();
        }
        for index in changed.iter().copied() {
            if let Some((key, track)) = search_keys.get_mut(index).zip(state.tracks.get(index)) {
                *key = queue_search_key(track);
            }
        }
        if order_changed || query_changed || (!query.is_empty() && !changed.is_empty()) {
            matches.clear();
            if query.is_empty() {
                matches.extend(0..track_count);
            } else {
                if search_keys.is_empty() {
                    search_keys.extend(state.tracks.iter().map(queue_search_key));
                }
                matches.extend(
                    search_keys
                        .iter()
                        .enumerate()
                        .filter_map(|(index, key)| key.contains(query.as_str()).then_some(index)),
                );
            }
            rendered_query.clone_from(&query);
        }

        let visible = if track_count == 0 {
            "empty"
        } else if matches.is_empty() {
            "no-results"
        } else {
            "list"
        };
        if body.visible_child_name().as_deref() != Some(visible) {
            if jump_requested.get() {
                jump_layout_pending.set(true);
            }
            body.set_visible_child_name(visible);
        }
        jump.set_sensitive(state.current.is_some());
        match_count.set(matches.len());
        if query_changed {
            row_menu.popdown();
            list.unselect_all();
            adjustment.set_value(0.0);
        }

        let wide = layout.allocated_width() >= 820;
        album_heading.set_visible(wide);
        if columns.is_visible() != !matches.is_empty() && jump_requested.get() {
            jump_layout_pending.set(true);
        }
        columns.set_visible(!matches.is_empty());
        let mut rows = rows_update.borrow_mut();
        // Measure a populated row after GTK applies its inherited font and styles.
        if let Some(&index) = matches.first().filter(|_| rows.is_empty()) {
            let item = make_queue_row(&model_update, &row_menu, &menu_target);
            let track = &state.tracks[index];
            item.update(
                track,
                index,
                state.current == Some(index),
                state.play_next.as_ref() == Some(&track.path),
            );
            list.add(&item.row);
            rows.insert(0, item);
            layout.queue_allocate();
        }
        if let Some(item) = rows.values().next() {
            row_height = item.row.preferred_height().1.max(1);
        }
        let stride = f64::from(row_height);
        let content_height = matches.len() * row_height as usize;
        if layout.size().1 as usize != content_height {
            layout.set_size(1, content_height as u32);
        }
        let viewport = f64::from(layout.allocated_height().max(1));
        let mut focus_slot = navigation.take().filter(|&slot| slot < matches.len());
        let mut top = adjustment.value();
        if jump_requested.get() && !jump_layout_pending.get() && query.is_empty() {
            jump_requested.set(false);
            focus_slot = state.current;
            if let Some(slot) = focus_slot {
                top = (slot as f64 + 0.5) * stride - viewport / 2.0;
            }
        } else if let Some(slot) = focus_slot {
            top = top.clamp(
                (slot + 1) as f64 * stride - viewport.max(stride),
                slot as f64 * stride,
            );
        }
        top = top.clamp(0.0, (content_height as f64 - viewport).max(0.0));
        if adjustment.value() != top {
            adjustment.set_value(top);
        }
        let first = ((top / stride) as usize).saturating_sub(QUEUE_OVERSCAN);
        let end = (((top + viewport) / stride).ceil() as usize + QUEUE_OVERSCAN).min(matches.len());
        let range = first..end;

        // Keep overlapping rows in place, including their focus and decoded covers.
        // Recycling the rest bounds widget count even when dragging the scrollbar.
        let obsolete: Vec<_> = rows
            .keys()
            .copied()
            .filter(|slot| !range.contains(slot))
            .collect();
        if !obsolete.is_empty() && row_menu.is_mapped() {
            row_menu.popdown();
        }
        let mut recycled = Vec::new();
        for slot in obsolete {
            let item = rows.remove(&slot).unwrap();
            list.unselect_row(&item.row);
            list.remove(&item.row);
            layout.queue_allocate();
            recycled.push(item);
        }
        let width = (layout.allocated_width() - 12).max(1);
        if list.width_request() != width {
            layout.queue_allocate();
            list.set_width_request(width);
        }
        let row_y = first * row_height as usize;
        if rendered_top != row_y {
            layout.queue_allocate();
            layout.move_(&list, 4, row_y as i32);
            rendered_top = row_y;
        }
        let available = 4usize.saturating_sub(state.cover_pending.len());
        let mut jobs = Vec::new();
        for slot in range {
            let index = matches[slot];
            let track = &state.tracks[index];
            let current = state.current == Some(index);
            let entry = rows.entry(slot);
            let needs_binding = matches!(entry, std::collections::btree_map::Entry::Vacant(_));
            let item = entry.or_insert_with(|| {
                let item = recycled
                    .pop()
                    .unwrap_or_else(|| make_queue_row(&model_update, &row_menu, &menu_target));
                list.insert(&item.row, (slot - first) as i32);
                layout.queue_allocate();
                item
            });
            if needs_binding
                || order_changed
                || query_changed
                || changed.contains(&index)
                || *item.path.borrow() != track.path
            {
                item.update(
                    track,
                    index,
                    current,
                    state.play_next.as_ref() == Some(&track.path),
                );
            } else {
                item.highlight(index, current);
            }
            item.album.set_visible(wide);
            let playing = current
                && state
                    .mpv
                    .as_ref()
                    .is_some_and(|player| !player.paused && player.has_track());
            let action = if playing {
                "Приостановить трек"
            } else {
                "Воспроизвести трек"
            };
            if item.play.tooltip_text().as_deref() != Some(action) {
                describe_control(&item.play, action);
                item.play.set_image(Some(&symbolize(
                    if playing {
                        "media-playback-pause-symbolic"
                    } else {
                        "media-playback-start-symbolic"
                    },
                    18,
                )));
            }
            if !scroll.is_mapped() {
                continue;
            }
            if *item.rendered_cover.borrow() != track.cover_path {
                if let Some(image) =
                    artwork::get(track.cover_path.as_deref(), false, &state.updates)
                {
                    match image {
                        Some(image) => {
                            item.cover.set_from_pixbuf(Some(&image.thumb));
                            item.cover_stack.set_visible_child_name("image");
                        }
                        None => item.clear_cover(),
                    }
                    item.rendered_cover.replace(track.cover_path.clone());
                }
            }
            if available > 0
                && track.cover_path.is_none()
                && state.metadata_ready.contains(&track.path)
                && !state.cover_pending.contains_key(&track.path)
                && !state.cover_attempted.contains(&track.path)
            {
                let y = slot as f64 * stride;
                let distance = (top - y - stride).max(y - top - viewport).max(0.0);
                jobs.push((distance, &track.path));
            }
        }
        if let Some(item) = focus_slot.and_then(|slot| rows.get(&slot)) {
            list.select_row(Some(&item.row));
            item.row.grab_focus();
        }
        drop(rows);
        if order_changed || query_changed || !changed.is_empty() {
            let total: f64 = matches
                .iter()
                .map(|&index| state.tracks[index].duration)
                .sum();
            let hours = (total / 3600.0) as u64;
            let minutes = (total / 60.0) as u64 % 60;
            let duration = if hours > 0 {
                format!(" · {hours} ч {minutes} мин")
            } else if minutes > 0 {
                format!(" · {minutes} мин")
            } else {
                String::new()
            };
            let text = if query.is_empty() {
                format!("{}{duration}", tracks_phrase(track_count))
            } else {
                format!("{} из {track_count}{duration}", matches.len())
            };
            if summary.text() != text {
                summary.set_text(&text);
            }
        }
        let can_start = track_count > 0 && state.mpv.is_some();
        if start.is_sensitive() != can_start {
            start.set_sensitive(can_start);
        }
        if clear_all.is_sensitive() != (track_count > 0) {
            clear_all.set_sensitive(track_count > 0);
        }
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
    });
    (page.upcast(), search)
}

fn start_mpv(model: &Rc<RefCell<Model>>, app: &gtk::Application) {
    const START_ERROR: &str = "Не удалось запустить mpv. Проверь, что он установлен.";
    request_ui(model);
    let Some(process) = MpvProcess::spawn(Some(app.upcast_ref())) else {
        model.borrow_mut().status = Some(START_ERROR.into());
        return;
    };
    let task = process.connect_task();
    model.borrow_mut().mpv_process = Some(process);
    let model = Rc::downgrade(model);
    glib::MainContext::default().spawn_local(async move {
        let connected = task.await.ok().flatten();
        let Some(model) = model.upgrade() else { return };
        let mut state = model.borrow_mut();
        if state.mpv_process.is_none() {
            // The window was destroyed while the connection was being established.
            return;
        }
        if let Some(socket) = connected {
            let mut player = Mpv::new(socket);
            watch_mpv(&mut player, &model);
            player.observe();
            let _ = player.set_volume(state.volume);
            let _ = player.set_muted(state.muted);
            let _ = player.command(json!(["set_property", "speed", state.speed]));
            let _ = player.command(json!([
                "set_property",
                "loop-file",
                if state.repeat == 2 { "inf" } else { "no" }
            ]));
            state.mpv = Some(player);
        } else {
            shutdown_mpv(&mut state);
            state.status = Some(START_ERROR.into());
        }
        state.updates.request();
    });
}

fn shutdown_mpv(state: &mut Model) {
    state.mpv.take();
    state.mpv_process.take();
}

fn watch_mpv(player: &mut Mpv, model: &Rc<RefCell<Model>>) {
    let model = Rc::downgrade(model);
    player.watch(move |condition, output| {
        let Some(model) = model.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if output {
            model
                .borrow_mut()
                .mpv
                .as_mut()
                .map_or(glib::ControlFlow::Break, |player| {
                    player.write_ready(condition)
                })
        } else {
            tick_mpv(&model, condition)
        }
    });
}

fn tick_mpv(model: &Rc<RefCell<Model>>, condition: glib::IOCondition) -> glib::ControlFlow {
    let (ended, failed) = {
        let mut state = model.borrow_mut();
        let Some(player) = state.mpv.as_mut() else {
            return glib::ControlFlow::Break;
        };
        let outcome = player.read_ready(condition);
        let volume = player.volume.take();
        let muted = player.muted.take();
        // HUP can accompany readable data. Keep the playback result until the
        // socket is drained, without asking a disconnected mpv to load.
        let (ended, failed, command_error) = if matches!(outcome, ReadReady::Closing) {
            (false, false, None)
        } else {
            (
                std::mem::take(&mut player.ended),
                std::mem::take(&mut player.failed),
                player.error.take(),
            )
        };
        if let Some(volume) = volume {
            state.volume = volume.clamp(0.0, 100.0);
        }
        if let Some(muted) = muted {
            state.muted = muted;
        }
        if failed {
            if let Some(current) = state.current {
                state.failed_tracks.insert(current);
            }
        } else if ended {
            state.failed_tracks.clear();
        }
        if let ReadReady::Disconnected(error) = outcome {
            shutdown_mpv(&mut state);
            let status = format!("Связь с mpv прервалась: {error}. Перезапусти плеер.");
            state.status = Some(match command_error {
                Some(message) => format!("{message} {status}"),
                None => status,
            });
            state.updates.request();
            return glib::ControlFlow::Break;
        }
        if let Some(error) = command_error {
            state.status = Some(error);
            state.updates.request();
        } else {
            state.updates.request_progress();
        }
        (ended, failed)
    };
    if failed {
        let next = advance_index(&mut model.borrow_mut(), 1, true);
        if let Some(next) = next {
            load_index(model, next);
            let mut state = model.borrow_mut();
            if state.status.is_none() {
                state.status = Some(
                    "Не удалось воспроизвести предыдущий файл. Перешли к следующему треку.".into(),
                );
            }
        }
    } else if ended {
        next_track(model, 1, true);
    }
    glib::ControlFlow::Continue
}

fn play_index(model: &Rc<RefCell<Model>>, index: usize) {
    model.borrow_mut().failed_tracks.clear();
    load_index(model, index);
}

fn load_index(model: &Rc<RefCell<Model>>, index: usize) {
    let mut state = model.borrow_mut();
    state.updates.request();
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
    let Some(mpv_path) = path.to_str() else {
        state.status = Some("Имя файла содержит символы, которые mpv IPC не принимает.".into());
        return;
    };
    state.status = None;
    if let Some(player) = state.mpv.as_mut() {
        if player.load(mpv_path).is_err() {
            state.status = Some("Не удалось передать трек в mpv. Перезапусти плеер.".into());
            return;
        }
        let _ = player.command(json!(["set_property", "pause", false]));
    }
    state.current = Some(index);
    for (job_path, job) in &state.lyrics_jobs {
        if job_path != &path {
            // The user moved on. An in-flight result may still land in its own
            // track, but further fallback requests are no longer worthwhile.
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    if state.shuffle {
        state.shuffle_order.record(&path);
    }
    if state.play_next.as_ref() == Some(&path) {
        state.play_next = None;
        state.queue_metadata_changed.insert(index);
    }
    drop(state);
    save_session(model);
    if !model.borrow().metadata_ready.contains(&path) && path.is_file() {
        start_metadata_job(model, vec![path.clone()]);
    }
    start_cover_job(model, path);
    start_lyrics_job(model, false);
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
            state.shuffle_order.can_advance(state.tracks.len())
        } else {
            current + 1 < state.tracks.len()
        }
}

fn advance_index(state: &mut Model, direction: isize, skip_failed: bool) -> Option<usize> {
    if direction > 0 {
        if let Some(path) = &state.play_next {
            if let Some(index) = state.tracks.iter().position(|track| &track.path == path) {
                if !skip_failed || !state.failed_tracks.contains(&index) {
                    return Some(index);
                }
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
    state.updates.request();
    if let Some(player) = state.mpv.as_mut() {
        let _ = player.command(json!(["stop"]));
    }
}

fn append_paths(model: &Rc<RefCell<Model>>, paths: Vec<PathBuf>) {
    let generation = {
        let mut state = model.borrow_mut();
        state.imports_pending += 1;
        state.updates.request();
        state.status = None;
        state.import_generation
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut found = Vec::new();
        for path in paths {
            collect_audio(&path, &mut found);
        }
        found.sort();
        found.dedup();
        let total = found.len();
        // Session and mpv IPC use UTF-8 paths. Keep unsupported names out of saved queues.
        found.retain(|path| path.to_str().is_some());
        let unsupported = total - found.len();
        let _ = sender.send((found, unsupported));
    });
    let model_result = model.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        match receiver.try_recv() {
            Ok((paths, unsupported)) => {
                let mut state = model_result.borrow_mut();
                state.imports_pending = state.imports_pending.saturating_sub(1);
                state.updates.request();
                if generation != state.import_generation {
                    return glib::ControlFlow::Break;
                }
                let found = !paths.is_empty();
                let skipped = || format!("Пропущено файлов с именами вне UTF-8: {unsupported}.");
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
                    state.status = Some(if unsupported > 0 {
                        skipped()
                    } else if found {
                        "Эти треки уже есть в очереди".into()
                    } else {
                        "Поддерживаемые аудиофайлы не найдены. Проверь папку и форматы файлов."
                            .into()
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
                if unsupported > 0 {
                    model_result.borrow_mut().status = Some(skipped());
                }
                start_metadata_job(&model_result, fresh);
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let mut state = model_result.borrow_mut();
                state.imports_pending = state.imports_pending.saturating_sub(1);
                state.updates.request();
                if generation == state.import_generation {
                    state.status =
                        Some("Импорт прервался. Попробуй добавить файлы ещё раз.".into());
                }
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
    state.muted = saved.muted;
    state.appearance = saved.appearance;
    state.volume = saved.volume.clamp(0.0, 100.0);
    state.speed = saved.speed.clamp(0.5, 2.0);
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
                Ok(track) => {
                    let mut state = model_result.borrow_mut();
                    let selected = state
                        .current
                        .and_then(|index| state.tracks.get(index))
                        .filter(|current| current.path == track.path)
                        .map(|current| current.path.clone());
                    merge_metadata(&mut state, &indices, track);
                    drop(state);
                    if let Some(path) = selected {
                        start_cover_job(&model_result, path);
                        start_lyrics_job(&model_result, false);
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

fn merge_metadata(
    state: &mut Model,
    indices: &std::collections::HashMap<PathBuf, usize>,
    mut track: Track,
) {
    state.metadata_pending.remove(&track.path);
    let index = indices
        .get(&track.path)
        .copied()
        .filter(|&index| {
            state
                .tracks
                .get(index)
                .is_some_and(|existing| existing.path == track.path)
        })
        .or_else(|| {
            state
                .tracks
                .iter()
                .position(|existing| existing.path == track.path)
        });
    if let Some(index) = index {
        let existing = &mut state.tracks[index];
        track.cover_path = existing.cover_path.take();
        let first_metadata = state.metadata_ready.insert(track.path.clone());
        if track.cover_path.is_none()
            && state.online_covers
            && (first_metadata || cover_identity(existing) != cover_identity(&track))
        {
            state.cover_attempted.remove(&track.path);
        }
        if lyric_timing::rank(&track.lyrics) < lyric_timing::rank(&existing.lyrics) {
            track.lyrics = std::mem::take(&mut existing.lyrics);
        }
        *existing = track;
        state.queue_metadata_changed.insert(index);
        state.updates.request();
    }
}

fn forget_lyrics_lookup(state: &mut Model, path: &Path) {
    if let Some(job) = state.lyrics_jobs.remove(path) {
        job.cancel.store(true, Ordering::Relaxed);
    }
    state.lyrics_pending.remove(path);
    state.lyrics_results.remove(path);
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
            if let Some(old) = state.lyrics_jobs.remove(&path) {
                old.cancel.store(true, Ordering::Relaxed);
                state.lyrics_pending.remove(&path);
            }
            state.lyrics_results.remove(&path);
        }
        let track = &state.tracks[state.current.unwrap()];
        if !state.metadata_ready.contains(&path)
            || lyric_timing::quality(&track.lyrics) >= 3
            || state.lyrics_pending.contains(&path)
            || state.lyrics_results.contains_key(&path)
            || state.lyrics_pending.len() >= 2
        {
            return;
        }
        let track = track.clone();
        state.lyrics_sequence = state.lyrics_sequence.wrapping_add(1);
        let token = state.lyrics_sequence;
        let cancel = Arc::new(AtomicBool::new(false));
        state.lyrics_jobs.insert(
            path.clone(),
            LyricsJob {
                token,
                cancel: cancel.clone(),
            },
        );
        state.lyrics_pending.insert(path.clone());
        state.updates.request();
        (track, token, cancel)
    };
    let (track, token, cancel) = track;
    let path = track.path.clone();
    if retry {
        lyrics::clear_lookup(&track);
    }

    // Stream each quality upgrade to the main loop. A line-synced result can
    // become visible while word-level cues are still in flight.
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<Lyric>>();
    let events = Rc::new(RefCell::new(receiver));
    let event_model = Rc::downgrade(model);
    let event_path = path.clone();
    glib::timeout_add_local(Duration::from_millis(20), move || loop {
        let next = events.borrow_mut().try_recv();
        match next {
            Ok(lines) => {
                let Some(model) = event_model.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let mut state = model.borrow_mut();
                if state
                    .lyrics_jobs
                    .get(&event_path)
                    .is_some_and(|job| job.token == token)
                {
                    if let Some(track) = state
                        .tracks
                        .iter_mut()
                        .find(|track| track.path == event_path)
                    {
                        if lyric_timing::rank(&lines) > lyric_timing::rank(&track.lyrics) {
                            track.lyrics = lines;
                        }
                    }
                    state.updates.request();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                return glib::ControlFlow::Continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return glib::ControlFlow::Break;
            }
        }
    });

    let task_cancel = cancel.clone();
    let task = gtk::gio::spawn_blocking(move || {
        let mut emit = |lines: &[Lyric]| {
            let _ = sender.send(lines.to_vec());
        };
        lyrics::online_lyrics_with(&track, task_cancel, &mut emit)
    });
    let model = Rc::downgrade(model);
    glib::MainContext::default().spawn_local(async move {
        let result = task
            .await
            .unwrap_or_else(|_| Err("Поиск прервался. Нажми «Искать снова».".into()));
        let Some(model) = model.upgrade() else { return };
        let mut state = model.borrow_mut();
        if !state
            .lyrics_jobs
            .get(&path)
            .is_some_and(|job| job.token == token)
        {
            return;
        }
        state.lyrics_pending.remove(&path);
        state.lyrics_jobs.remove(&path);
        state.updates.request();
        let cancelled = cancel.load(Ordering::Relaxed);
        let message = match result {
            Ok(lines) if !lines.is_empty() => {
                let quality = lyric_timing::quality(&lines);
                if let Some(track) = state.tracks.iter_mut().find(|track| track.path == path) {
                    if lyric_timing::rank(&lines) > lyric_timing::rank(&track.lyrics) {
                        track.lyrics = lines;
                    }
                }
                (!cancelled).then(|| {
                    if quality >= 3 {
                        "Текст найден".to_string()
                    } else if quality >= 2 {
                        "Найдена построчная синхронизация".to_string()
                    } else {
                        "Текст найден".to_string()
                    }
                })
            }
            Ok(_) => {
                (!cancelled).then(|| "Текст пока не найден. Можно попробовать ещё раз.".to_string())
            }
            Err(error) => (!cancelled).then_some(error),
        };
        if let Some(message) = message {
            state.lyrics_results.insert(path.clone(), message);
        }
        drop(state);
        start_lyrics_job(&model, false);
    });
}

#[derive(PartialEq, Eq)]
enum CoverLookup {
    Active,
    Removed,
}

fn forget_cover_lookup(state: &mut Model, path: &Path) {
    state.cover_attempted.remove(path);
    if let Some(lookup) = state.cover_pending.get_mut(path) {
        // Keep counting the job until it finishes and reserve this path for its completion.
        *lookup = CoverLookup::Removed;
    }
}

fn cover_identity(track: &Track) -> (&str, &str, &str) {
    (&track.title, &track.artist, &track.album)
}

fn begin_cover_lookup(state: &mut Model, path: &Path) -> Option<(Track, bool)> {
    let track = state.tracks.iter().find(|track| track.path == path)?;
    if track.cover_path.is_some()
        || state.cover_attempted.contains(path)
        || state.cover_pending.contains_key(path)
    {
        return None;
    }
    state
        .cover_pending
        .insert(path.to_owned(), CoverLookup::Active);
    state.cover_attempted.insert(path.to_owned());
    state.updates.request();
    // Use the actual tags, including the album, when searching the CSV.
    Some((
        track.clone(),
        state.online_covers && state.metadata_ready.contains(path),
    ))
}

// Return whether the current queue entry needs a fresh lookup.
fn finish_cover_lookup(
    state: &mut Model,
    path: &Path,
    identity: (&str, &str, &str),
    online: bool,
    result: Option<Option<PathBuf>>,
) -> bool {
    let Some(pending) = state.cover_pending.remove(path) else {
        return false;
    };
    state.updates.request();
    let Some(track) = state.tracks.iter_mut().find(|track| track.path == path) else {
        state.cover_attempted.remove(path);
        return false;
    };
    if pending == CoverLookup::Removed {
        return true;
    }
    let Some(cover) = result else {
        state.cover_attempted.remove(path);
        return false;
    };
    let retry = cover.is_none()
        && state.online_covers
        && state.metadata_ready.contains(path)
        && (!online || cover_identity(track) != identity);
    track.cover_path = cover;
    if retry {
        state.cover_attempted.remove(path);
    } else {
        state.cover_attempted.insert(path.to_owned());
    }
    retry
}

fn start_cover_job(model: &Rc<RefCell<Model>>, path: PathBuf) {
    let Some((selected, online)) = begin_cover_lookup(&mut model.borrow_mut(), &path) else {
        return;
    };
    let (title, artist, album) = cover_identity(&selected);
    let identity = (title.to_owned(), artist.to_owned(), album.to_owned());
    let task = gtk::gio::spawn_blocking(move || {
        extract_cover(&selected.path)
            .or_else(|| online.then(|| cover_art::online_cover(&selected)).flatten())
    });
    let model = Rc::downgrade(model);
    glib::MainContext::default().spawn_local(async move {
        let result = task.await.ok();
        let Some(model) = model.upgrade() else { return };
        let retry = finish_cover_lookup(
            &mut model.borrow_mut(),
            &path,
            (&identity.0, &identity.1, &identity.2),
            online,
            result,
        );
        if retry {
            start_cover_job(&model, path);
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
        muted: state.muted,
        appearance: state.appearance,
        volume: state.volume,
        speed: state.speed,
    };
    if let Err(error) = session::save(saved) {
        eprintln!("Не удалось сохранить очередь: {error}");
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
