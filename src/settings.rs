use super::*;

mod content {
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct SettingsContent;

    #[glib::object_subclass]
    impl ObjectSubclass for SettingsContent {
        const NAME: &'static str = "VeyraSettingsContent";
        type Type = super::SettingsContent;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for SettingsContent {}
    impl WidgetImpl for SettingsContent {
        fn preferred_width(&self) -> (i32, i32) {
            let (minimum, _) = self.parent_preferred_width();
            (minimum, minimum.max(780))
        }
    }
    impl ContainerImpl for SettingsContent {}
    impl BoxImpl for SettingsContent {}
}

glib::wrapper! {
    pub struct SettingsContent(ObjectSubclass<content::SettingsContent>)
        @extends gtk::Box, gtk::Container, gtk::Widget,
        @implements gtk::Orientable;
}

pub(super) fn build_settings_page(
    model: &Rc<RefCell<Model>>,
    sidebar: &gtk::Box,
) -> gtk::ScrolledWindow {
    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_overlay_scrolling(true);
    scroll.set_shadow_type(gtk::ShadowType::None);
    scroll.style_context().add_class("settings-page");

    let content: SettingsContent = glib::Object::new();
    content.set_orientation(gtk::Orientation::Vertical);
    content.set_spacing(28);
    content.set_halign(gtk::Align::Center);
    content.set_hexpand(true);
    content.set_margin_top(28);
    content.set_margin_bottom(28);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.style_context().add_class("settings-content");
    scroll.add(&content);
    let header = gtk::Box::new(gtk::Orientation::Vertical, 0);
    header.style_context().add_class("settings-header");
    header.pack_start(
        &settings_text("Настройки", "settings-heading"),
        false,
        false,
        0,
    );
    content.pack_start(&header, false, false, 0);
    let content = content.upcast::<gtk::Box>();

    let appearance = settings_section(
        &content,
        "Интерфейс",
        "preferences-desktop-display-symbolic",
    );
    let saved = model.borrow().appearance;
    let background = settings_switch(
        &appearance,
        "Динамический фон",
        "Цвет обложки в фоне плеера.",
        saved.dynamic_background,
    );
    background.connect_active_notify({
        let model = model.clone();
        move |switch| {
            model.borrow_mut().appearance.dynamic_background = switch.is_active();
            theme::set_dynamic(switch.is_active());
            request_ui(&model);
            save_session(&model);
        }
    });
    let animations = settings_switch(
        &appearance,
        "Анимации",
        "Плавные переходы и движение пластинки.",
        saved.animations,
    );
    animations.connect_active_notify({
        let model = model.clone();
        move |switch| {
            model.borrow_mut().appearance.animations = switch.is_active();
            set_decorative_motion(switch.is_active());
            if let Some(window) = switch.toplevel() {
                sync_stack_motion(&window);
            }
            save_session(&model);
        }
    });
    let expanded = settings_switch(
        &appearance,
        "Расширенная боковая панель",
        "Названия разделов рядом с иконками.",
        saved.expanded_sidebar,
    );
    expanded.connect_active_notify({
        let model = model.clone();
        let sidebar = sidebar.downgrade();
        move |switch| {
            model.borrow_mut().appearance.expanded_sidebar = switch.is_active();
            if let Some(sidebar) = sidebar.upgrade() {
                apply_sidebar_mode(&sidebar, switch.is_active());
            }
            save_session(&model);
        }
    });

    let library = settings_section(&content, "Библиотека", "view-list-symbolic");
    let (cover_row, _) = settings_row(
        "Обложки из интернета",
        "Недостающие обложки из Яндекс Музыки по подключённому CSV.",
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
    csv_row.style_context().add_class("settings-connection");
    set_style_class(&status, "connected", connected);
    let csv_button = gtk::Button::with_label(if connected {
        "Заменить…"
    } else {
        "Выбрать…"
    });
    csv_button.set_valign(gtk::Align::Center);
    csv_button.style_context().add_class("settings-control");
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
                        status.style_context().add_class("connected");
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
                        status.style_context().remove_class("connected");
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
    info.style_context().add_class("settings-footer");
    info.pack_start(
        &symbolize("dialog-information-symbolic", 16),
        false,
        false,
        0,
    );
    info.pack_start(
        &settings_text(
            "Плеер ищет более точную синхронизацию в LRCLIB и LyricsPlus. Найденные тексты сохраняются в кэш.",
            "settings-note",
        ),
        true,
        true,
        0,
    );
    content.pack_start(&info, false, false, 0);
    scroll
}

fn settings_switch(section: &gtk::Box, title: &str, note: &str, active: bool) -> gtk::Switch {
    if !section.children().is_empty() {
        section.add(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }
    let (row, _) = settings_row(title, note);
    let control = gtk::Switch::new();
    control.set_active(active);
    control.set_valign(gtk::Align::Center);
    describe_control(&control, title);
    row.pack_end(&control, false, false, 0);
    section.add(&row);
    control
}

fn settings_text(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_line_wrap(true);
    label.set_line_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_width_chars(1);
    label.set_max_width_chars(48);
    label.style_context().add_class(class);
    label
}

fn settings_section(content: &gtk::Box, title: &str, icon: &str) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.style_context().add_class("settings-section-header");
    let glyph = symbolize(icon, 20);
    glyph.style_context().add_class("settings-section-icon");
    header.pack_start(&glyph, false, false, 0);
    header.pack_start(
        &settings_text(title, "settings-section-title"),
        true,
        true,
        0,
    );
    section.pack_start(&header, false, false, 0);
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
