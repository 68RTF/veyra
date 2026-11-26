use super::*;

pub(super) fn build(
    model: &Rc<RefCell<Model>>,
    on_queue: impl Fn() + 'static,
) -> (gtk::Widget, gtk::Stack) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.style_context().add_class("player-stage");

    let views = gtk::Stack::new();
    views.set_hhomogeneous(false);
    views.set_vhomogeneous(false);
    views.set_transition_type(gtk::StackTransitionType::Crossfade);
    views.set_transition_duration(180);
    views.set_hexpand(true);
    page.pack_start(&views, true, true, 0);
    let upcoming = build_upcoming(model, on_queue);
    upcoming.set_no_show_all(true);
    page.pack_end(&upcoming, false, false, 0);
    let refresh_preview = Rc::new({
        let page = page.downgrade();
        let views = views.downgrade();
        let upcoming = upcoming.downgrade();
        move || {
            if let (Some(page), Some(views), Some(upcoming)) =
                (page.upgrade(), views.upgrade(), upcoming.upgrade())
            {
                let show = page.allocated_width() >= 820
                    && page.allocated_height() >= 560
                    && views.visible_child_name().as_deref() == Some("artwork");
                if upcoming.is_visible() != show {
                    upcoming.set_visible(show);
                }
            }
        }
    });
    let pending_layout = Rc::new(Cell::new(false));
    page.connect_size_allocate({
        let refresh = refresh_preview.clone();
        move |_, _| {
            if pending_layout.replace(true) {
                return;
            }
            let pending = pending_layout.clone();
            let refresh = refresh.clone();
            glib::idle_add_local_once(move || {
                pending.set(false);
                refresh();
            });
        }
    });
    views.connect_visible_child_name_notify(move |_| refresh_preview());

    let artwork = gtk::Overlay::new();
    // The base sets the minimum. The overlay can grow with the available area
    // without making the current artwork size a minimum for the next resize.
    let base = gtk::Box::new(gtk::Orientation::Vertical, 0);
    base.set_height_request(208);
    theme::attach_cover_glow(&base);
    artwork.add(&base);
    let hero_event = gtk::EventBox::new();
    hero_event.set_visible_window(false);
    hero_event.add_events(gtk::gdk::EventMask::BUTTON_PRESS_MASK);
    hero_event.set_halign(gtk::Align::Center);
    hero_event.set_valign(gtk::Align::Center);
    artwork.add_overlay(&hero_event);
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    hero.set_halign(gtk::Align::Center);
    hero.set_valign(gtk::Align::Center);

    let cover_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    cover_frame.style_context().add_class("cover-frame");
    cover_frame.set_size_request(192, 192);
    cover_frame.set_valign(gtk::Align::Center);
    let cover_stack = gtk::Stack::new();
    cover_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    cover_stack.set_transition_duration(180);
    cover_stack.add_named(&cover_placeholder(0, 64), "placeholder");
    let (cover, picture) = cover_picture();
    cover_stack.add_named(&picture, "image");
    let (cover_alt, picture) = cover_picture();
    cover_stack.add_named(&picture, "image-alt");
    cover_stack.set_visible_child_name("placeholder");
    cover_frame.pack_start(&cover_stack, true, true, 0);
    let sleeve = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let record = vinyl::build(model);
    record.set_valign(gtk::Align::Center);
    sleeve.set_halign(gtk::Align::Center);
    sleeve.pack_start(&record, false, false, 0);
    sleeve.pack_start(&cover_frame, false, false, 0);
    let sleeve_balance = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sleeve.pack_start(&sleeve_balance, false, false, 0);
    hero.pack_start(&sleeve, false, false, 0);

    let details = gtk::Box::new(gtk::Orientation::Vertical, 4);
    details.set_valign(gtk::Align::Center);
    details.set_width_request(180);
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
    let lyrics_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    lyrics_row.pack_start(
        &symbolize("format-justify-left-symbolic", 16),
        false,
        false,
        0,
    );
    let lyrics_label = gtk::Label::new(Some("Текст песни"));
    lyrics_row.pack_start(&lyrics_label, false, false, 0);
    show_lyrics.add(&lyrics_row);
    show_lyrics.set_halign(gtk::Align::Start);
    describe_control(&show_lyrics, "Открыть текст песни");
    let views_lyrics = views.downgrade();
    show_lyrics.connect_clicked(move |_| {
        if let Some(views) = views_lyrics.upgrade() {
            views.set_visible_child_name("lyrics");
        }
    });
    details.pack_start(&title, false, false, 0);
    details.pack_start(&artist, false, false, 0);
    details.pack_start(&album, false, false, 0);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.style_context().add_class("track-actions");
    actions.set_margin_top(12);
    actions.set_no_show_all(true);
    actions.pack_start(&show_lyrics, false, false, 0);
    let track_menu = gtk::Menu::new();
    let menu_target = Rc::new(RefCell::new(PathBuf::new()));
    track_menu::add_details(&track_menu, model, &menu_target);
    track_menu.show_all();
    let more = menu_button(&track_menu, "Действия с текущим треком");
    more.style_context().add_class("icon-button");
    gtk::prelude::MenuButtonExt::set_direction(&more, gtk::ArrowType::Down);
    actions.pack_start(&more, false, false, 0);
    details.pack_start(&actions, false, false, 0);
    let context_menu = track_menu.downgrade();
    let context_target = menu_target.clone();
    hero_event.connect_button_press_event(move |_, event| {
        if event.triggers_context_menu() && !context_target.borrow().as_os_str().is_empty() {
            if let Some(menu) = context_menu.upgrade() {
                menu.popup_at_pointer(Some(event));
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
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
    actions.show_all();
    more.show_all();
    show_lyrics.show_all();
    start.set_no_show_all(true);

    start.set_visible(model.borrow().current.is_none());
    actions.set_visible(model.borrow().current.is_some());
    // Keep the centered group in place as metadata changes between tracks.
    hero.pack_start(&details, false, false, 0);
    hero_event.add(&hero);
    base.connect_size_allocate({
        let hero = hero.downgrade();
        let cover = cover_frame.downgrade();
        let details = details.downgrade();
        let title = title.downgrade();
        let artist = artist.downgrade();
        let album = album.downgrade();
        let actions = actions.downgrade();
        let start = start.downgrade();
        let hero_event = hero_event.downgrade();
        move |_, allocation| {
            let (Some(hero), Some(cover), Some(details)) =
                (hero.upgrade(), cover.upgrade(), details.upgrade())
            else {
                return;
            };
            let width = allocation.width();
            let height = allocation.height();
            let panorama = width >= 1360 && height >= 580;
            let stacked = width >= 620 && height >= 460 && !panorama;
            let compact = !stacked && (width < 680 || height < 370);
            set_style_class(&hero, "stacked-hero", stacked);
            set_style_class(&hero, "compact-hero", compact);
            let orientation = if stacked {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            };
            if hero.orientation() != orientation {
                hero.set_orientation(orientation);
            }
            let spacing = if panorama {
                56
            } else if stacked {
                16
            } else if compact {
                20
            } else {
                28
            };
            let details_width = if panorama {
                440
            } else if stacked {
                (width - 64).clamp(200, 640)
            } else {
                (width / 2).clamp(200, 420)
            };
            if details.width_request() != details_width {
                details.set_width_request(details_width);
            }
            if let Some(title) = title.upgrade() {
                title.set_lines(if stacked { 2 } else { 3 });
            }
            if let Some(artist) = artist.upgrade() {
                artist.set_lines(if stacked { 1 } else { 2 });
            }
            let details_height = if stacked { 196 } else { -1 };
            if details.height_request() != details_height {
                details.set_height_request(details_height);
            }
            if let Some(start) = start.upgrade() {
                start.set_halign(if stacked {
                    gtk::Align::Center
                } else {
                    gtk::Align::Start
                });
            }
            for label in [title.upgrade(), artist.upgrade(), album.upgrade()]
                .into_iter()
                .flatten()
            {
                let align = if stacked { 0.5 } else { 0.0 };
                if label.xalign() != align {
                    label.set_xalign(align);
                }
            }
            if let Some(actions) = actions.upgrade() {
                actions.set_halign(if stacked {
                    gtk::Align::Center
                } else {
                    gtk::Align::Start
                });
            }
            let reveal_ratio = if compact { 0.16 } else { 0.24 };
            let available = if stacked {
                width - 64
            } else {
                width - details_width - spacing - 32
            };
            let edge = (f64::from(available) / (1.0 + reveal_ratio)) as i32;
            // Reserve a fixed metadata slot so switching to a short title cannot
            // move or resize the cover. Small windows retain the compact layout.
            let height_limit = if stacked { height - 240 } else { height - 56 };
            let edge = edge
                .min(height_limit)
                .clamp(112, if stacked || panorama { 384 } else { 280 });
            if let Some(event) = hero_event.upgrade() {
                event.set_valign(if stacked {
                    gtk::Align::Start
                } else {
                    gtk::Align::Center
                });
                // Anchor using the reserved slot, not the natural height of the
                // current title/album, so metadata cannot shift the cover.
                let top = if stacked {
                    ((height - edge - spacing - 220) / 2).max(16)
                } else {
                    0
                };
                if event.margin_top() != top {
                    event.set_margin_top(top);
                }
            }
            let reveal = (f64::from(edge) * reveal_ratio).round() as i32;
            let balance = if stacked { reveal } else { 0 };
            if sleeve_balance.width_request() != balance {
                sleeve_balance.set_width_request(balance);
            }
            if record.width_request() != reveal || record.height_request() != edge {
                record.set_size_request(reveal, edge);
            }
            if cover.width_request() != edge {
                cover.set_size_request(edge, edge);
            }
            if hero.spacing() != spacing {
                hero.set_spacing(spacing);
            }
        }
    });
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
    let updates = model.borrow().updates.clone();
    let metadata_fade = motion::Tween::new(1.0);
    details.connect_unmap({
        let fade = metadata_fade.clone();
        move |details| {
            fade.snap(1.0);
            details.set_opacity(1.0);
        }
    });
    let mut rendered: Option<(PathBuf, String, String, String)> = None;
    let mut rendered_cover = None;
    updates.listen(&artwork, move || {
        let state = model_update.borrow();
        if let Some(index) = state.current {
            if let Some(track) = state.tracks.get(index) {
                if rendered
                    .as_ref()
                    .map(|(path, title, artist, album)| (path, title, artist, album))
                    != Some((&track.path, &track.title, &track.artist, &track.album))
                {
                    if rendered
                        .as_ref()
                        .is_some_and(|(path, _, _, _)| *path != track.path)
                        && details.is_mapped()
                        && animations_enabled()
                    {
                        metadata_fade.snap(0.82);
                        details.set_opacity(0.82);
                        metadata_fade.animate(&details, 1.0, 170, |widget, opacity| {
                            widget.set_opacity(opacity)
                        });
                    }
                    rendered = Some((
                        track.path.clone(),
                        track.title.clone(),
                        track.artist.clone(),
                        track.album.clone(),
                    ));
                    start.hide();
                    actions.show();
                    menu_target.replace(track.path.clone());
                    title.set_text(&track.title);
                    title.set_tooltip_text(Some(&track.title));
                    artist.set_text(if track.artist.is_empty() {
                        "Неизвестный исполнитель"
                    } else {
                        &track.artist
                    });
                    artist.set_tooltip_text(Some(&track.artist));
                    album.set_text(&track.album);
                    album.set_tooltip_text(Some(&track.album));
                    album.set_visible(!track.album.is_empty());
                }
                let lyrics_status = if !track.lyrics.is_empty() {
                    if state.lyrics_pending.contains(&track.path) {
                        ("Текст песни", "Уточняем синхронизацию текста")
                    } else {
                        ("Текст песни", "Открыть текст песни")
                    }
                } else if state.lyrics_pending.contains(&track.path) {
                    ("Текст песни", "Ищем текст песни")
                } else if state.lyrics_results.contains_key(&track.path) {
                    ("Нет текста", "Открыть текст песни и повторить поиск")
                } else {
                    ("Текст песни", "Открыть текст песни")
                };
                if lyrics_label.text() != lyrics_status.0 {
                    lyrics_label.set_text(lyrics_status.0);
                }
                if show_lyrics.tooltip_text().as_deref() != Some(lyrics_status.1) {
                    describe_control(&show_lyrics, lyrics_status.1);
                }
                if let Some(image) = current_artwork(track, &mut rendered_cover, &state) {
                    let image = image.as_ref().and_then(|art| art.hero.as_ref());
                    if let Some(image) = image {
                        let (slot, name) =
                            if cover_stack.visible_child_name().as_deref() == Some("image") {
                                (&cover_alt, "image-alt")
                            } else {
                                (&cover, "image")
                            };
                        slot.replace(Some(image.clone()));
                        cover_stack.set_visible_child_name(name);
                    } else {
                        cover_stack.set_visible_child_name("placeholder");
                    }
                }
            }
        }
        if state.current.is_none() && rendered.take().is_some() {
            title.set_text("Добавь музыку");
            title.set_tooltip_text(None);
            artist.set_text("Выбери файлы или добавь папку с музыкой");
            artist.set_tooltip_text(None);
            album.hide();
            actions.hide();
            menu_target.replace(PathBuf::new());
            start.show();
            rendered_cover = None;
            cover_stack.set_visible_child_name("placeholder");
            if let Some(views) = views_update.upgrade() {
                views.set_visible_child_name("artwork");
            }
        }
    });
    (page.upcast(), views)
}

fn build_upcoming(model: &Rc<RefCell<Model>>, on_queue: impl Fn() + 'static) -> gtk::Overlay {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    panel.style_context().add_class("upcoming-panel");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let heading = gtk::Label::new(Some("Дальше в очереди"));
    heading.set_xalign(0.0);
    heading.style_context().add_class("upcoming-heading");
    header.pack_start(&heading, true, true, 0);
    panel.pack_start(&header, false, false, 0);
    let list = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    scroll.set_min_content_height(72);
    scroll.set_shadow_type(gtk::ShadowType::None);
    scroll.style_context().add_class("upcoming-scroll");
    scroll.add(&list);
    let pending_cards = Rc::new(Cell::new(false));
    scroll.connect_size_allocate({
        let list = list.downgrade();
        move |scroll, _| {
            if pending_cards.replace(true) {
                return;
            }
            let pending = pending_cards.clone();
            let list = list.clone();
            let scroll = scroll.downgrade();
            glib::idle_add_local_once(move || {
                pending.set(false);
                if let (Some(list), Some(scroll)) = (list.upgrade(), scroll.upgrade()) {
                    fit_upcoming(&list, scroll.allocated_width());
                }
            });
        }
    });
    panel.pack_start(&scroll, false, false, 0);
    let empty = gtk::Label::new(Some("Следующих треков пока нет"));
    empty.set_xalign(0.0);
    empty.style_context().add_class("secondary");
    empty.set_no_show_all(true);
    panel.pack_start(&empty, false, false, 0);
    let mut rows = Vec::new();
    for _ in 0..6 {
        let button = gtk::Button::new();
        button.style_context().add_class("upcoming-row");
        button.set_width_request(184);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let cover = gtk::Stack::new();
        cover.set_size_request(artwork::QUEUE_THUMB_PIXELS, artwork::QUEUE_THUMB_PIXELS);
        cover.add_named(
            &cover_placeholder(artwork::QUEUE_THUMB_PIXELS, 16),
            "placeholder",
        );
        let image = gtk::Image::new();
        cover.add_named(&image, "image");
        cover.set_visible_child_name("placeholder");
        row.pack_start(&cover, false, false, 0);
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(1);
        title.style_context().add_class("upcoming-title");
        let artist = gtk::Label::new(None);
        artist.set_xalign(0.0);
        artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
        artist.set_max_width_chars(1);
        artist.style_context().add_class("upcoming-artist");
        labels.pack_start(&title, false, false, 0);
        labels.pack_start(&artist, false, false, 0);
        row.pack_start(&labels, true, true, 0);
        button.add(&row);
        let path: Rc<RefCell<Option<PathBuf>>> = Rc::default();
        button.connect_clicked({
            let path = path.clone();
            let model = model.clone();
            move |_| {
                let index = path.borrow().as_ref().and_then(|path| {
                    model
                        .borrow()
                        .tracks
                        .iter()
                        .position(|track| &track.path == path)
                });
                if let Some(index) = index {
                    play_index(&model, index);
                }
            }
        });
        button.show_all();
        button.set_no_show_all(true);
        list.pack_start(&button, false, false, 0);
        rows.push((
            button,
            title,
            artist,
            cover,
            image,
            path,
            RefCell::new(None),
        ));
    }
    let all = gtk::Button::with_label("Вся очередь");
    all.style_context().add_class("queue-link");
    all.set_halign(gtk::Align::Start);
    all.connect_clicked(move |_| on_queue());
    header.pack_end(&all, false, false, 0);
    panel.show_all();
    let model = model.clone();
    let updates = model.borrow().updates.clone();
    updates.listen(&panel, {
        let panel = panel.downgrade();
        move || {
            if !panel.upgrade().is_some_and(|panel| panel.is_mapped()) {
                return;
            }
            let state = model.borrow();
            let heading_text = if state.shuffle {
                "В очереди"
            } else {
                "Дальше в очереди"
            };
            if heading.text() != heading_text {
                heading.set_text(heading_text);
            }
            let next = state
                .play_next
                .as_ref()
                .and_then(|path| state.tracks.iter().position(|track| &track.path == path));
            let start = state.current.map_or(0, |index| index + 1);
            let ordered = (start..state.tracks.len()).chain(
                (0..start.min(state.tracks.len())).filter(|_| state.repeat == 1 || state.shuffle),
            );
            let indices: Vec<_> = next
                .into_iter()
                .chain(ordered.filter(|index| Some(*index) != next))
                .filter(|index| Some(*index) != state.current)
                .take(6)
                .collect();
            empty.set_visible(indices.is_empty());
            let available = 4usize.saturating_sub(state.cover_pending.len());
            let mut jobs = Vec::new();
            for ((button, title, artist, cover, image, path, rendered_cover), index) in
                rows.iter().zip(
                    indices
                        .iter()
                        .copied()
                        .map(Some)
                        .chain(std::iter::repeat(None)),
                )
            {
                let Some(index) = index else {
                    button.hide();
                    path.replace(None);
                    continue;
                };
                let track = &state.tracks[index];
                button.show();
                if title.text() != track.title {
                    title.set_text(&track.title);
                }
                if artist.text() != track.artist {
                    artist.set_text(&track.artist);
                }
                if path.borrow().as_ref() != Some(&track.path) {
                    path.replace(Some(track.path.clone()));
                    rendered_cover.take();
                    cover.set_visible_child_name("placeholder");
                }
                let action = format!("Воспроизвести {} — {}", track.title, track.artist);
                if button.tooltip_text().as_deref() != Some(action.as_str()) {
                    describe_control(button, &action);
                }
                if *rendered_cover.borrow() != track.cover_path {
                    if let Some(art) =
                        artwork::get(track.cover_path.as_deref(), false, &state.updates)
                    {
                        if let Some(art) = art {
                            image.set_from_pixbuf(Some(&art.thumb));
                            cover.set_visible_child_name("image");
                        } else {
                            cover.set_visible_child_name("placeholder");
                        }
                        rendered_cover.replace(track.cover_path.clone());
                    }
                }
                if jobs.len() < available
                    && track.cover_path.is_none()
                    && state.metadata_ready.contains(&track.path)
                    && !state.cover_pending.contains_key(&track.path)
                    && !state.cover_attempted.contains(&track.path)
                {
                    jobs.push(track.path.clone());
                }
                set_style_class(button, "play-next", next == Some(index));
            }
            fit_upcoming(&list, scroll.allocated_width());
            drop(state);
            for path in jobs {
                start_cover_job(&model, path);
            }
        }
    });
    // Keep the shelf's contents out of the minimum size, so it can disappear
    // when the window shrinks or lyrics need the full reading surface.
    let preview = gtk::Overlay::new();
    let base = gtk::Box::new(gtk::Orientation::Vertical, 0);
    base.set_height_request(132);
    preview.add(&base);
    preview.add_overlay(&panel);
    preview.show_all();
    preview.hide();
    preview
}

fn upcoming_card_width(width: i32, count: usize) -> i32 {
    let columns = ((width + 8) / 184).max(1).min(count.max(1) as i32);
    ((width - (columns - 1) * 8) / columns).clamp(176, 320)
}

fn fit_upcoming(list: &gtk::Box, width: i32) {
    if width <= 1 {
        return;
    }
    let children = list.children();
    let count = children.iter().filter(|child| child.is_visible()).count();
    let width = upcoming_card_width(width, count);
    for child in children {
        if child.width_request() != width {
            child.set_width_request(width);
        }
    }
}

type CoverSurface = Rc<RefCell<Option<artwork::HeroImage>>>;

fn cover_picture() -> (CoverSurface, gtk::DrawingArea) {
    let area = gtk::DrawingArea::new();
    let surface: CoverSurface = Rc::new(RefCell::new(None));
    let source = surface.clone();
    area.connect_draw(move |area, cr| {
        if let Some(image) = source.borrow().as_ref() {
            let _ = cr.save();
            let width = f64::from(area.allocated_width());
            let height = f64::from(area.allocated_height());
            let radius = 16.0;
            let half_pi = std::f64::consts::FRAC_PI_2;
            cr.new_sub_path();
            cr.arc(width - radius, radius, radius, -half_pi, 0.0);
            cr.arc(width - radius, height - radius, radius, 0.0, half_pi);
            cr.arc(radius, height - radius, radius, half_pi, 2.0 * half_pi);
            cr.arc(radius, radius, radius, 2.0 * half_pi, 3.0 * half_pi);
            cr.close_path();
            cr.clip();
            cr.scale(width / image.edge, height / image.edge);
            if cr.set_source_surface(&image.surface, 0.0, 0.0).is_ok() {
                cr.source().set_filter(gtk::cairo::Filter::Good);
                let _ = cr.paint();
            }
            let _ = cr.restore();
        }
        glib::Propagation::Proceed
    });
    (surface, area)
}

#[cfg(test)]
mod layout_tests {
    use super::upcoming_card_width;
    #[test]
    fn queue_cards_fill_normal_width_without_oversizing_short_queues() {
        assert_eq!(upcoming_card_width(992, 5) * 5 + 8 * 4, 992);
        assert!((1600..=1648).contains(&(upcoming_card_width(1648, 5) * 5 + 8 * 4)));
        assert_eq!(upcoming_card_width(1648, 1), 320);
        assert!(upcoming_card_width(700, 6) >= 176);
    }
}
