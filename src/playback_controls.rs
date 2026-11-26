use super::*;

mod timeline {
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct Timeline;

    #[glib::object_subclass]
    impl ObjectSubclass for Timeline {
        const NAME: &'static str = "VeyraTimeline";
        type Type = super::Timeline;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for Timeline {}
    impl WidgetImpl for Timeline {
        fn preferred_width(&self) -> (i32, i32) {
            let (minimum, _) = self.parent_preferred_width();
            (minimum, minimum.max(360))
        }
    }
    impl ContainerImpl for Timeline {}
    impl BoxImpl for Timeline {}
}

glib::wrapper! {
    pub struct Timeline(ObjectSubclass<timeline::Timeline>)
        @extends gtk::Box, gtk::Container, gtk::Widget;
}

pub(super) fn build(model: &Rc<RefCell<Model>>) -> gtk::Widget {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 4);
    panel.style_context().add_class("player-bar");
    panel.style_context().add_class("compact");

    let seek_row: Timeline = glib::Object::new();
    seek_row.set_spacing(12);
    seek_row.set_halign(gtk::Align::Fill);
    seek_row.set_hexpand(true);
    let time = gtk::Label::new(Some("0:00"));
    time.style_context().add_class("timecode");
    let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, SEEK_STEPS, 1.0);
    seek.set_draw_value(false);
    describe_control(&seek, "Позиция воспроизведения");
    seek.set_has_tooltip(true);
    seek.style_context().add_class("seek");
    let duration = gtk::Label::new(Some("—"));
    time.set_tooltip_text(Some("Прошло"));
    duration.set_tooltip_text(Some("Осталось"));
    time.set_xalign(0.0);
    duration.set_xalign(1.0);
    duration.style_context().add_class("timecode");
    seek_row.pack_start(&time, false, false, 0);
    seek_row.pack_start(&seek, true, true, 0);
    seek_row.pack_start(&duration, false, false, 0);
    seek_row.style_context().add_class("timeline");

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let now_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
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
    small_frame.show_all();
    small_frame.set_no_show_all(true);
    now_box.pack_start(&small_frame, false, false, 0);
    let now_text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    now_text.set_valign(gtk::Align::Center);
    let now = gtk::Label::new(Some("Трек не выбран"));
    now.set_xalign(0.0);
    now.set_ellipsize(gtk::pango::EllipsizeMode::End);
    now.set_width_chars(1);
    now.set_max_width_chars(1);
    now.style_context().add_class("now-title");
    let now_artist = gtk::Label::new(Some(""));
    now_artist.set_xalign(0.0);
    now_artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    now_artist.set_width_chars(1);
    now_artist.set_max_width_chars(1);
    now_artist.style_context().add_class("now-artist");
    now_text.pack_start(&now, false, false, 0);
    now_text.pack_start(&now_artist, false, false, 0);
    now_box.pack_start(&now_text, true, true, 0);
    now_text.set_width_request(32);
    row.pack_start(&now_box, true, true, 0);

    let shuffle = gtk::ToggleButton::new();
    shuffle.set_focus_on_click(false);
    shuffle.add(&toggle_content(&symbolize(
        "media-playlist-shuffle-symbolic",
        18,
    )));
    describe_control(&shuffle, "Перемешать");
    shuffle.style_context().add_class("transport-button");
    shuffle.style_context().add_class("mode");
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
    repeat.style_context().add_class("mode");
    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    transport.style_context().add_class("transport");
    transport.set_halign(gtk::Align::Center);
    transport.set_valign(gtk::Align::Center);
    for button in [
        shuffle.upcast_ref::<gtk::Button>(),
        &prev,
        &play,
        &next,
        &repeat,
    ] {
        button.set_valign(gtk::Align::Center);
        transport.pack_start(button, false, false, 0);
    }
    let center = gtk::Box::new(gtk::Orientation::Vertical, 0);
    center.style_context().add_class("playback-center");
    center.set_hexpand(true);
    center.pack_start(&transport, false, false, 0);
    row.pack_start(&center, true, true, 0);

    let volume_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    volume_box.style_context().add_class("volume-group");
    volume_box.set_halign(gtk::Align::End);
    volume_box.set_valign(gtk::Align::Center);
    let speed_button = gtk::MenuButton::new();
    speed_button.style_context().add_class("speed-button");
    speed_button.set_label(&speed_caption(model.borrow().speed));
    speed_button.set_image(Some(&symbolize("power-profile-performance-symbolic", 16)));
    speed_button.set_always_show_image(true);
    speed_button.set_valign(gtk::Align::Center);
    describe_control(&speed_button, "Скорость воспроизведения");
    let speed_popover = gtk::Popover::new(Some(&speed_button));
    speed_popover
        .style_context()
        .add_class("playback-speed-popover");
    speed_popover.add(&build_speed_control(model, &speed_button));
    speed_popover.child().unwrap().show_all();
    speed_button.set_popover(Some(&speed_popover));
    volume_box.pack_start(&speed_button, false, false, 0);
    let mute = icon_button(
        "audio-volume-high-symbolic",
        "Выключить звук",
        "icon-button",
    );
    mute.set_focus_on_click(false);
    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume.set_value(model.borrow().volume);
    volume.set_increments(2.0, 5.0);
    volume.set_draw_value(false);
    describe_control(&volume, "Громкость");
    volume.set_width_request(32);
    volume.style_context().add_class("volume");
    volume_box.pack_start(&mute, false, false, 0);
    volume_box.pack_start(&volume, false, false, 0);
    row.pack_end(&volume_box, false, false, 0);
    panel.pack_start(&row, false, false, 0);
    panel.pack_start(&seek_row, false, false, 0);

    // Keep the progress bar beneath the whole row in compact windows. On a
    // wide window it belongs to the transport column between track and volume.
    let compact_layout = Rc::new(Cell::new(true));
    let deferred_layout = Rc::new(Cell::new(false));
    panel.connect_size_allocate({
        let row = row.downgrade();
        let now_box = now_box.downgrade();
        let center = center.downgrade();
        let seek_row = seek_row.downgrade();
        let volume = volume.downgrade();
        let now_text = now_text.downgrade();
        let compact_layout = compact_layout.clone();
        move |panel, allocation| {
            let compact = allocation.width() < 840;
            if compact == compact_layout.get() || deferred_layout.replace(true) {
                return;
            }
            let pending = deferred_layout.clone();
            let compact_layout = compact_layout.clone();
            let panel = panel.downgrade();
            let row = row.clone();
            let now_box = now_box.clone();
            let center = center.clone();
            let seek_row = seek_row.clone();
            let volume = volume.clone();
            let now_text = now_text.clone();
            glib::idle_add_local_once(move || {
                pending.set(false);
                let (
                    Some(panel),
                    Some(row),
                    Some(now_box),
                    Some(center),
                    Some(seek_row),
                    Some(volume),
                    Some(now_text),
                ) = (
                    panel.upgrade(),
                    row.upgrade(),
                    now_box.upgrade(),
                    center.upgrade(),
                    seek_row.upgrade(),
                    volume.upgrade(),
                    now_text.upgrade(),
                )
                else {
                    return;
                };
                let compact = panel.allocated_width() < 840;
                if compact == compact_layout.replace(compact) {
                    return;
                }
                if let Some(parent) = seek_row.parent().and_downcast::<gtk::Box>() {
                    parent.remove(&seek_row);
                }
                if compact {
                    panel.pack_start(&seek_row, false, false, 0);
                } else {
                    center.pack_start(&seek_row, false, false, 0);
                }
                now_text.set_width_request(if compact { 32 } else { 128 });
                // Share spare compact-row width with the title rather than
                // leaving all of it around the transport buttons.
                row.set_child_packing(&now_box, compact, true, 0, gtk::PackType::Start);
                volume.set_width_request(if compact { 32 } else { 80 });
                set_style_class(&panel, "compact", compact);
                seek_row.show_all();
            });
        }
    });
    apply_repeat_label(&repeat, model.borrow().repeat);

    let m = model.clone();
    play.connect_clicked(move |_| {
        toggle_play(&m);
    });
    let m = model.clone();
    mute.connect_clicked(move |_| toggle_mute(&m));
    let m = model.clone();
    next.connect_clicked(move |_| next_track(&m, 1, false));
    let m = model.clone();
    prev.connect_clicked(move |_| next_track(&m, -1, false));
    let m = model.clone();
    shuffle.connect_toggled(move |button| {
        let mut state = m.borrow_mut();
        state.shuffle = button.is_active();
        state.shuffle_order = ShuffleOrder::default();
        state.updates.request();
        drop(state);
        save_session(&m);
    });
    let m = model.clone();
    repeat.connect_clicked(move |button| {
        let mut state = m.borrow_mut();
        state.repeat = (state.repeat + 1) % 3;
        state.updates.request();
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
    let syncing_volume = Rc::new(Cell::new(false));
    let syncing = syncing_volume.clone();
    volume.connect_value_changed(move |scale| {
        if syncing.get() {
            return;
        }
        let value = scale.value();
        let mut state = m.borrow_mut();
        state.volume = value;
        state.updates.request();
        if let Some(player) = state.mpv.as_mut() {
            let _ = player.set_volume(value);
        }
        drop(state);
        schedule_session_save(&m, &volume_save);
    });
    volume.add_events(gtk::gdk::EventMask::SCROLL_MASK | gtk::gdk::EventMask::SMOOTH_SCROLL_MASK);
    let scroll_remainder = Cell::new(0.0);
    volume.connect_scroll_event(move |scale, event| {
        let steps = match event.direction() {
            gtk::gdk::ScrollDirection::Up | gtk::gdk::ScrollDirection::Right => 1.0,
            gtk::gdk::ScrollDirection::Down | gtk::gdk::ScrollDirection::Left => -1.0,
            gtk::gdk::ScrollDirection::Smooth => {
                let (_, vertical) = event.delta();
                let delta = scroll_remainder.get() - vertical;
                scroll_remainder.set(delta.fract());
                delta.trunc().clamp(-1.0, 1.0)
            }
            _ => return glib::Propagation::Proceed,
        };
        if steps != 0.0 {
            scale.set_value(scale.value() + steps * 2.0);
        }
        glib::Propagation::Stop
    });
    let dragging = connect_seek(&seek, model);
    let m = model.clone();
    let now_clone = now.clone();
    let now_artist_clone = now_artist.clone();
    let seek_tick = seek.clone();
    let time_tick = time.clone();
    let duration_tick = duration.clone();
    let play_tick = play.clone();
    let small_image_tick = small_image.clone();
    let small_stack_tick = small_stack.clone();
    let updates = model.borrow().updates.clone();
    let mut rendered_cover = None;
    let mut rendered_track: Option<(PathBuf, String, String)> = None;
    let mut rendered_time = None;
    let mut rendered_remaining = None;
    let mut rendered_volume = None;
    let mut rendered_playing: Option<bool> = None;

    updates.listen(&panel, move || {
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
                if let Some(image) = current_artwork(track, &mut rendered_cover, &state) {
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
        if (volume.value() - state.volume).abs() >= 0.01 && !volume.has_grab() {
            syncing_volume.set(true);
            volume.set_value(state.volume);
            syncing_volume.set(false);
        }
        let icon = match (state.muted, state.volume as u32) {
            (true, _) | (_, 0) => "audio-volume-muted-symbolic",
            (_, 1..=32) => "audio-volume-low-symbolic",
            (_, 33..=66) => "audio-volume-medium-symbolic",
            _ => "audio-volume-high-symbolic",
        };
        if rendered_volume != Some((icon, state.muted)) {
            rendered_volume = Some((icon, state.muted));
            if let Some(image) = mute
                .image()
                .and_then(|image| image.downcast::<gtk::Image>().ok())
            {
                image.set_from_icon_name(Some(icon), gtk::IconSize::Button);
            }
            describe_control(
                &mute,
                if state.muted {
                    "Включить звук"
                } else {
                    "Выключить звук"
                },
            );
            set_style_class(&volume_box, "muted", state.muted);
        }
        let player = state.mpv.as_ref().filter(|player| player.loaded);
        let length = player.map_or(0.0, |player| player.duration);
        let previewing = player.is_some_and(|player| dragging.get() == Some(player.generation));
        let position = if previewing {
            length * seek_tick.value() / SEEK_STEPS
        } else {
            player.map_or(0.0, |player| player.position(state.speed))
        };
        let second = position as u64;
        if rendered_time != Some(second) {
            rendered_time = Some(second);
            time_tick.set_text(&format_time(position));
        }
        let remaining = (length > 0.0).then(|| (length - position).max(0.0).ceil() as u64);
        if rendered_remaining != remaining {
            rendered_remaining = remaining;
            duration_tick.set_text(&remaining.map_or_else(
                || "—".into(),
                |seconds| format!("−{}", format_time(seconds as f64)),
            ));
        }
        let playing = state
            .mpv
            .as_ref()
            .is_some_and(|player| player.has_track() && !player.paused);
        if rendered_playing != Some(playing) {
            rendered_playing = Some(playing);
            set_play_icon(&play_tick, playing);
        }
        if !previewing {
            update_seek_display(&seek_tick, position, length);
        }
    });
    panel.upcast()
}

const SEEK_STEPS: f64 = 1000.0;

fn pointer_fraction(scale: &gtk::Scale, x: f64) -> f64 {
    let rect = scale.range_rect();
    let (start, end) = scale.slider_range();
    let half_thumb = f64::from(end - start) / 2.0;
    let width = (f64::from(rect.width()) - 2.0 * half_thumb).max(1.0);
    let fraction = ((x - f64::from(rect.x()) - half_thumb) / width).clamp(0.0, 1.0);
    let rtl = gtk::prelude::WidgetExt::direction(scale) == gtk::TextDirection::Rtl;
    if scale.is_inverted() ^ rtl {
        1.0 - fraction
    } else {
        fraction
    }
}

fn connect_seek(scale: &gtk::Scale, model: &Rc<RefCell<Model>>) -> Rc<Cell<Option<u64>>> {
    let dragging = Rc::new(Cell::new(None));
    let updates = model.borrow().updates.clone();
    scale.connect_value_changed({
        let updates = updates.clone();
        let dragging = dragging.clone();
        move |_| {
            if dragging.get().is_some() {
                updates.request();
            }
        }
    });
    scale.connect_button_press_event({
        let model = model.clone();
        let dragging = dragging.clone();
        move |scale, event| {
            if event.button() == 1 {
                let state = model.borrow();
                if let Some(player) = state
                    .mpv
                    .as_ref()
                    .filter(|player| player.loaded && player.duration > 0.0)
                {
                    dragging.set(Some(player.generation));
                    let x = event.position().0;
                    let (thumb_start, thumb_end) = scale.slider_range();
                    if x < f64::from(thumb_start) || x > f64::from(thumb_end) {
                        scale.set_value(pointer_fraction(scale, x) * SEEK_STEPS);
                    }
                }
            }
            glib::Propagation::Proceed
        }
    });
    scale.connect_event_after({
        let model = model.clone();
        let dragging = dragging.clone();
        move |scale, event| {
            if let Some(release) = event.downcast_ref::<gtk::gdk::EventButton>() {
                if event.event_type() == gtk::gdk::EventType::ButtonRelease && release.button() == 1
                {
                    if let Some(generation) = dragging.take() {
                        let mut state = model.borrow_mut();
                        state.updates.request();
                        if let Some(player) = state
                            .mpv
                            .as_mut()
                            .filter(|player| player.generation == generation)
                        {
                            seek_absolute(player, player.duration * scale.value() / SEEK_STEPS);
                        }
                    }
                }
            }
        }
    });
    scale.connect_grab_broken_event({
        let dragging = dragging.clone();
        let updates = updates.clone();
        move |_, _| {
            if dragging.take().is_some() {
                updates.request();
            }
            glib::Propagation::Proceed
        }
    });
    scale.connect_change_value({
        let model = model.clone();
        let dragging = dragging.clone();
        move |_, _, value| {
            if dragging.get().is_none() {
                let mut state = model.borrow_mut();
                state.updates.request();
                if let Some(player) = state.mpv.as_mut() {
                    seek_absolute(player, player.duration * value / SEEK_STEPS);
                }
            }
            glib::Propagation::Proceed
        }
    });
    scale.connect_query_tooltip({
        let model = model.clone();
        move |scale, x, _, keyboard, tooltip| {
            let state = model.borrow();
            let Some(player) = state
                .mpv
                .as_ref()
                .filter(|player| player.loaded && player.duration > 0.0)
            else {
                return false;
            };
            let fraction = if keyboard {
                scale.value() / SEEK_STEPS
            } else {
                pointer_fraction(scale, f64::from(x))
            };
            tooltip.set_text(Some(&format_time(fraction * player.duration)));
            true
        }
    });
    dragging
}

pub(super) fn seek_absolute(player: &mut Mpv, position: f64) -> bool {
    if !player.loaded {
        return false;
    }
    let at = if player.duration > 0.0 {
        position.clamp(0.0, player.duration)
    } else {
        position.max(0.0)
    };
    if player
        .command(json!(["seek", at, "absolute+exact"]))
        .is_err()
    {
        return false;
    }
    if player.paused {
        player.paused_lyric_position = Some(at);
    }
    player.set_position(at);
    true
}

fn toggle_play(model: &Rc<RefCell<Model>>) -> bool {
    let mut state = model.borrow_mut();
    let selected = state
        .current
        .or_else(|| (!state.tracks.is_empty()).then_some(0));
    let Some(player) = state.mpv.as_mut() else {
        return false;
    };
    if player.has_track() {
        if player.command(json!(["cycle", "pause"])).is_ok() {
            player.paused = !player.paused;
            state.updates.request();
            return true;
        }
    } else if let Some(index) = selected {
        drop(state);
        play_index(model, index);
        return true;
    }
    false
}

fn toggle_mute(model: &Rc<RefCell<Model>>) {
    let mut state = model.borrow_mut();
    let muted = !state.muted;
    if let Some(player) = state.mpv.as_mut() {
        if player.set_muted(muted).is_err() {
            return;
        }
    }
    state.muted = muted;
    state.updates.request();
    drop(state);
    save_session(model);
}

pub(super) fn shortcut(
    window: &gtk::ApplicationWindow,
    event: &gtk::gdk::EventKey,
    model: &Rc<RefCell<Model>>,
) -> bool {
    use gtk::gdk::{keys::constants as key, ModifierType};
    let focus = window.focused_widget();
    if focus
        .as_ref()
        .is_some_and(|widget| widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>())
    {
        return false;
    }
    let modifiers = event.state() & gtk::accelerator_get_default_mod_mask();
    let control = ModifierType::CONTROL_MASK;
    let folder = control | ModifierType::SHIFT_MASK;
    if matches!(event.keyval(), key::o | key::O) && (modifiers == control || modifiers == folder) {
        if let Some(paths) = choose_files(modifiers == folder) {
            append_paths(model, paths);
        }
        return true;
    }
    if !modifiers.is_empty() {
        return false;
    }
    let interactive = focus.as_ref().is_some_and(|widget| widget.can_focus());
    match event.keyval() {
        key::space
            if !focus
                .as_ref()
                .is_some_and(|widget| widget.has_visible_focus()) =>
        {
            toggle_play(model)
        }
        key::Left | key::Right if !interactive => {
            let mut state = model.borrow_mut();
            let speed = state.speed;
            state.updates.request();
            state.mpv.as_mut().is_some_and(|player| {
                let delta = if event.keyval() == key::Left {
                    -5.0
                } else {
                    5.0
                };
                seek_absolute(player, player.position(speed) + delta)
            })
        }
        key::m | key::M => {
            toggle_mute(model);
            true
        }
        _ => false,
    }
}

fn update_seek_display(scale: &gtk::Scale, position: f64, duration: f64) {
    let value = if duration > 0.0 {
        (position / duration * SEEK_STEPS).clamp(0.0, SEEK_STEPS)
    } else {
        0.0
    };
    if (scale.value() - value).abs() >= 0.05 || (value == 0.0 && scale.value() != 0.0) {
        scale.set_value(value);
    }
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

fn speed_caption(speed: f64) -> String {
    format!(
        "{}×",
        format!("{speed:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
    )
}

fn speed_text(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.style_context().add_class(class);
    label
}

fn build_speed_control(model: &Rc<RefCell<Model>>, button: &gtk::MenuButton) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.style_context().add_class("playback-speed");
    content.pack_start(&speed_text("Скорость", "speed-heading"), false, false, 0);
    let speed = gtk::Adjustment::new(model.borrow().speed, 0.5, 2.0, 0.05, 0.25, 0.0);
    let save = Rc::new(RefCell::new(None));
    speed.connect_value_changed({
        let model = model.clone();
        let button = button.downgrade();
        move |speed| {
            let value = speed.value();
            {
                let mut state = model.borrow_mut();
                state.speed = value;
                state.updates.request();
                if let Some(player) = state.mpv.as_mut() {
                    let _ = player.command(json!(["set_property", "speed", value]));
                }
            }
            if let Some(button) = button.upgrade() {
                button.set_label(&speed_caption(value));
            }
            schedule_session_save(&model, &save);
        }
    });
    for value in [0.75, 1.0, 1.25, 1.5, 2.0] {
        let label = if value == 1.0 {
            "1×  ·  Обычная".into()
        } else {
            speed_caption(value)
        };
        let preset = gtk::ToggleButton::new();
        preset.style_context().add_class("speed-preset");
        describe_control(&preset, &label);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        row.pack_start(&speed_text(&label, "speed-option"), true, true, 0);
        let mark = symbolize("object-select-symbolic", 16);
        row.pack_end(&mark, false, false, 0);
        preset.add(&row);
        let selected = (speed.value() - value).abs() < 0.001;
        preset.set_active(selected);
        mark.set_opacity(if selected { 1.0 } else { 0.0 });
        let updating = Rc::new(Cell::new(false));
        preset.connect_toggled({
            let speed = speed.clone();
            let updating = updating.clone();
            move |preset| {
                if updating.get() {
                    return;
                }
                speed.set_value(value);
                if !preset.is_active() {
                    preset.set_active(true);
                }
            }
        });
        speed.connect_value_changed({
            let preset = preset.downgrade();
            move |speed| {
                if let Some(preset) = preset.upgrade() {
                    let selected = (speed.value() - value).abs() < 0.001;
                    updating.set(true);
                    preset.set_active(selected);
                    updating.set(false);
                    mark.set_opacity(if selected { 1.0 } else { 0.0 });
                }
            }
        });
        content.pack_start(&preset, false, false, 0);
    }
    content.pack_start(
        &gtk::Separator::new(gtk::Orientation::Horizontal),
        false,
        false,
        6,
    );
    let fine = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    fine.style_context().add_class("speed-fine");
    let value = speed_text(&speed_caption(speed.value()), "speed-value");
    value.set_xalign(0.5);
    let minus = icon_button("list-remove-symbolic", "Уменьшить скорость", "speed-step");
    let plus = icon_button("list-add-symbolic", "Увеличить скорость", "speed-step");
    for (button, step) in [(&minus, -0.05), (&plus, 0.05)] {
        let speed = speed.clone();
        button.connect_clicked(move |_| {
            speed.set_value(((speed.value() + step) * 100.0).round() / 100.0)
        });
    }
    let update_fine = {
        let minus = minus.downgrade();
        let plus = plus.downgrade();
        let value = value.downgrade();
        move |speed: &gtk::Adjustment| {
            if let Some(value) = value.upgrade() {
                value.set_text(&speed_caption(speed.value()));
            }
            if let Some(minus) = minus.upgrade() {
                minus.set_sensitive(speed.value() > 0.5);
            }
            if let Some(plus) = plus.upgrade() {
                plus.set_sensitive(speed.value() < 2.0);
            }
        }
    };
    update_fine(&speed);
    speed.connect_value_changed(update_fine);
    fine.pack_start(&minus, false, false, 0);
    fine.pack_start(&value, true, true, 0);
    fine.pack_start(&plus, false, false, 0);
    content.pack_start(&fine, false, false, 0);
    content
}
