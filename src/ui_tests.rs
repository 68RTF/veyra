use super::*;

pub(super) fn test_model() -> Rc<RefCell<Model>> {
    Rc::new(RefCell::new(Model {
        updates: Rc::default(),
        tracks: vec![
            preview_track("/fixture/first.mp3".into()),
            preview_track("/fixture/second.mp3".into()),
        ],
        current: Some(0),
        library_revision: 1,
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
        repeat: 2,
        online_covers: false,
        volume: 0.0,
        muted: false,
        appearance: Appearance::default(),
        speed: 1.0,
        status: None,
        imports_pending: 0,
        import_generation: 0,
        failed_tracks: Default::default(),
        mpv: None,
        mpv_process: None,
    }))
}

#[test]
fn manual_next_advances_even_with_repeat_one() {
    // Avoid persistence/cover work by checking the extracted navigation decision.
    assert_eq!(next_index(0, 2, 1, 2), Some(1));
    assert_eq!(next_index(1, 2, 1, 2), None);
    assert_eq!(next_index(1, 2, 1, 1), Some(0));
    assert_eq!(next_index(0, 2, -1, 1), Some(1));
    assert_eq!(next_index(0, 0, 1, 0), None);
}

#[test]
fn failed_play_next_does_not_override_error_recovery() {
    let model = test_model();
    let mut state = model.borrow_mut();
    state
        .tracks
        .push(preview_track("/fixture/third.mp3".into()));
    state.play_next = Some(state.tracks[0].path.clone());
    state.current = Some(1);
    state.failed_tracks.insert(0);
    state.failed_tracks.insert(1);
    assert_eq!(advance_index(&mut state, 1, true), Some(2));
    // A manual retry must remain possible.
    assert_eq!(advance_index(&mut state, 1, false), Some(0));
}

#[test]
fn removing_track_cancels_lyrics_and_forgets_its_result() {
    let model = test_model();
    let path = model.borrow().tracks[1].path.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut state = model.borrow_mut();
        state.lyrics_pending.insert(path.clone());
        state
            .lyrics_results
            .insert(path.clone(), "Старая ошибка".into());
        state.lyrics_jobs.insert(
            path.clone(),
            LyricsJob {
                token: 1,
                cancel: cancel.clone(),
            },
        );
    }
    // Exercise removal cleanup without writing a session in a unit test.
    forget_lyrics_lookup(&mut model.borrow_mut(), &path);
    let state = model.borrow();
    assert!(cancel.load(Ordering::Relaxed));
    assert!(!state.lyrics_pending.contains(&path));
    assert!(!state.lyrics_results.contains_key(&path));
    assert!(!state.lyrics_jobs.contains_key(&path));
}

#[test]
fn word_upgrade_keeps_rows_only_while_line_text_is_unchanged() {
    let lines = vec![
        Lyric {
            time: Some(1.0),
            text: "Hello world".into(),
        },
        Lyric {
            time: Some(3.0),
            text: "Again".into(),
        },
    ];
    let word_upgrade = vec![
        Lyric {
            time: Some(1.0),
            text: "<00:01.00>Hello <00:01.40>world".into(),
        },
        Lyric {
            time: Some(3.0),
            text: "Again".into(),
        },
    ];
    assert!(same_lyric_shape(&lines, &word_upgrade));

    let different_text = vec![
        Lyric {
            time: Some(1.0),
            text: "<00:01.00>Hello <00:01.40>there".into(),
        },
        Lyric {
            time: Some(3.0),
            text: "Again".into(),
        },
    ];
    assert!(!same_lyric_shape(&lines, &different_text));
    assert!(!same_lyric_shape(&lines, &lines[..1]));
}

#[test]
fn metadata_follows_the_file_after_queue_edits() {
    let model = test_model();
    let mut state = model.borrow_mut();
    let indices = state
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.path.clone(), index))
        .collect();
    let mut result = state.tracks[0].clone();
    let path = result.path.clone();
    result.title = "Теги из файла".into();
    state.metadata_pending.insert(path.clone());
    state.tracks.swap(0, 1);
    let other = state.tracks[0].path.clone();

    merge_metadata(&mut state, &indices, result.clone());
    assert_eq!(state.tracks[0].path, other);
    assert_eq!(state.tracks[1].title, result.title);
    assert!(state.metadata_ready.contains(&path));
    assert!(!state.metadata_pending.contains(&path));

    state.tracks.pop();
    state.metadata_pending.insert(path.clone());
    merge_metadata(&mut state, &indices, result);
    assert_eq!(state.tracks.len(), 1);
    assert_eq!(state.tracks[0].path, other);
    assert!(!state.metadata_pending.contains(&path));
}

#[test]
fn local_cover_lookup_can_run_again_after_reimport() {
    let model = test_model();
    let mut state = model.borrow_mut();
    let path = state.tracks[0].path.clone();
    let (track, online) = begin_cover_lookup(&mut state, &path).unwrap();
    let cover = Some(PathBuf::from("/fixture/cover.jpg"));
    assert!(!finish_cover_lookup(
        &mut state,
        &path,
        cover_identity(&track),
        online,
        Some(cover.clone())
    ));
    assert_eq!(state.tracks[0].cover_path, cover);
    assert!(state.cover_attempted.contains(&path));

    state.tracks.remove(0);
    forget_cover_lookup(&mut state, &path);
    state.tracks.push(preview_track(path.clone()));
    let (track, online) = begin_cover_lookup(&mut state, &path).unwrap();
    assert!(!finish_cover_lookup(
        &mut state,
        &path,
        cover_identity(&track),
        online,
        Some(cover.clone())
    ));
    assert_eq!(state.tracks[1].cover_path, cover);
}

#[test]
fn late_metadata_keeps_partial_word_timings() {
    let model = test_model();
    let mut state = model.borrow_mut();
    let partial = library::parse_lyrics("[00:01]<00:01>One <00:02>two\n[00:04]Three four");
    state.tracks[0].lyrics = partial.clone();
    let mut metadata = state.tracks[0].clone();
    metadata.lyrics = library::parse_lyrics("[00:01]One two\n[00:04]Three four");
    merge_metadata(&mut state, &Default::default(), metadata);
    assert_eq!(state.tracks[0].lyrics, partial);
}

#[test]
fn removed_cover_lookup_cannot_update_reimported_track() {
    let model = test_model();
    let mut state = model.borrow_mut();
    let path = state.tracks[0].path.clone();
    let (old, online) = begin_cover_lookup(&mut state, &path).unwrap();
    state.tracks.remove(0);
    forget_cover_lookup(&mut state, &path);
    state.tracks.push(preview_track(path.clone()));
    assert!(begin_cover_lookup(&mut state, &path).is_none());
    assert_eq!(state.cover_pending.len(), 1);
    let old_cover = Some(PathBuf::from("/fixture/old.jpg"));
    assert!(finish_cover_lookup(
        &mut state,
        &path,
        cover_identity(&old),
        online,
        Some(old_cover)
    ));
    assert!(state.tracks[1].cover_path.is_none());

    let (current, online) = begin_cover_lookup(&mut state, &path).unwrap();
    let cover = Some(PathBuf::from("/fixture/new.jpg"));
    assert!(!finish_cover_lookup(
        &mut state,
        &path,
        cover_identity(&current),
        online,
        Some(cover.clone())
    ));
    assert_eq!(state.tracks[1].cover_path, cover);
}

#[test]
fn online_cover_lookup_waits_for_album_metadata() {
    let model = test_model();
    let mut state = model.borrow_mut();
    state.online_covers = true;
    let path = state.tracks[0].path.clone();
    let (preview, online) = begin_cover_lookup(&mut state, &path).unwrap();
    assert!(!online);
    let mut tagged = preview.clone();
    tagged.album = "Live Album".into();
    merge_metadata(&mut state, &Default::default(), tagged);
    assert!(finish_cover_lookup(
        &mut state,
        &path,
        cover_identity(&preview),
        online,
        Some(None)
    ));
    let (current, online) = begin_cover_lookup(&mut state, &path).unwrap();
    assert!(online);
    assert_eq!(current.album, "Live Album");
}

#[test]
fn metadata_enables_online_cover_after_a_completed_local_lookup() {
    let model = test_model();
    let mut state = model.borrow_mut();
    state.online_covers = true;
    let path = state.tracks[0].path.clone();
    let (track, online) = begin_cover_lookup(&mut state, &path).unwrap();
    assert!(!finish_cover_lookup(
        &mut state,
        &path,
        cover_identity(&track),
        online,
        Some(None)
    ));
    assert!(begin_cover_lookup(&mut state, &path).is_none());
    merge_metadata(&mut state, &Default::default(), track);
    assert!(begin_cover_lookup(&mut state, &path).unwrap().1);
}

fn read_saved_session() -> std::io::Result<Session> {
    session::flush();
    session::load()
}

fn pump(milliseconds: u64) {
    let until = Instant::now() + Duration::from_millis(milliseconds);
    let context = glib::MainContext::default();
    while Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pump_until(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() && Instant::now() < deadline {
        pump(25);
    }
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![widget.clone()];
    if let Some(container) = widget.downcast_ref::<gtk::Container>() {
        for child in container.children() {
            result.extend(descendants(&child));
        }
    }
    result
}

#[test]
#[ignore = "Requires an isolated X11 display; see scripts/check-ui.sh"]
fn gtk_smoke() {
    gtk::init().unwrap();
    theme::install(&gtk::gdk::Screen::default().unwrap());
    let app = gtk::Application::new(
        Some("local.ownplayer.test"),
        gtk::gio::ApplicationFlags::NON_UNIQUE,
    );
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    build_ui(&app);
    pump(800);
    let window = app.windows().into_iter().next().unwrap();
    let capture = |window: &gtk::Window, name: &str| {
        if let Ok(directory) = std::env::var("OWN_PLAYER_SCREENSHOTS") {
            window.move_(0, 0);
            pump(30);
            window
                .window()
                .unwrap()
                .pixbuf(0, 0, window.allocated_width(), window.allocated_height())
                .unwrap()
                .savev(
                    Path::new(&directory).join(format!("{name}.png")),
                    "png",
                    &[],
                )
                .unwrap();
        }
    };
    let stage_color = || window.style_context().lookup_color("md_stage").unwrap();
    let artwork_stage = stage_color();
    theme::set_cover(None);
    assert_ne!(artwork_stage, stage_color(), "artwork must tint the stage");
    let artwork = read_saved_session().unwrap().paths[0].with_file_name("cover.png");
    // Restore the hue from the actual artwork decode. Directly decoding at
    // 48 px can choose a different dominant bin for a detailed illustration.
    let cover = artwork::get(Some(&artwork), true, &Rc::default())
        .unwrap()
        .unwrap();
    theme::set_hue(cover.hue);
    assert_eq!(artwork_stage, stage_color());
    pump(240);
    capture(window.upcast_ref(), "player");
    let upcoming = descendants(window.upcast_ref())
        .into_iter()
        .find(|widget| widget.style_context().has_class("upcoming-panel"))
        .unwrap();
    assert!(upcoming.is_mapped());
    let preview_tracks = descendants(&upcoming)
        .into_iter()
        .filter(|widget| widget.style_context().has_class("upcoming-row") && widget.is_mapped())
        .count();
    assert_eq!(preview_tracks, 5);
    // The queue page has never been opened: the preview must fetch its own covers.
    let preview_covers = || {
        descendants(&upcoming)
            .iter()
            .filter_map(|widget| widget.downcast_ref::<gtk::Stack>())
            .filter(|stack| {
                stack.is_mapped() && stack.visible_child_name().as_deref() == Some("image")
            })
            .count()
    };
    pump_until(|| preview_covers() == preview_tracks);
    assert_eq!(preview_covers(), 5);
    capture(window.upcast_ref(), "upcoming-covers");

    let widgets = descendants(window.upcast_ref());
    for (label, tooltip) in [
        ("Файлы", "Добавить файлы"),
        ("Папка", "Добавить папку"),
        ("Настройки", "Настройки"),
    ] {
        let button = widgets
            .iter()
            .find(|widget| {
                widget.style_context().has_class("sidebar-action")
                    && widget.tooltip_text().as_deref() == Some(tooltip)
            })
            .unwrap();
        assert!(descendants(button)
            .iter()
            .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
            .any(|text| text.is_mapped() && text.text() == label));
    }
    let play = widgets
        .iter()
        .filter(|w| w.style_context().has_class("play-fab"))
        .find_map(|w| w.clone().downcast::<gtk::Button>().ok())
        .unwrap();
    let next = widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
        .find(|button| button.tooltip_text().as_deref() == Some("Следующий трек"))
        .unwrap();
    let shuffle = widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::ToggleButton>())
        .find(|button| button.tooltip_text().as_deref() == Some("Перемешать"))
        .unwrap();
    shuffle.emit_clicked();
    assert!(shuffle.is_active() && read_saved_session().unwrap().shuffle);
    shuffle.emit_clicked();
    assert!(!shuffle.is_active() && !read_saved_session().unwrap().shuffle);
    let repeat = widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .find(|button| button.tooltip_text().as_deref() == Some("Без повтора"))
        .unwrap();
    for (mode, caption) in [
        (1, "Повторять очередь"),
        (2, "Повторять трек"),
        (0, "Без повтора"),
    ] {
        repeat.emit_clicked();
        assert_eq!(repeat.tooltip_text().as_deref(), Some(caption));
        assert_eq!(repeat.style_context().has_class("active"), mode != 0);
        assert_eq!(read_saved_session().unwrap().repeat, mode);
    }
    let play_position = || play.translate_coordinates(&window, 0, 0).unwrap();
    let now_title = widgets
        .iter()
        .find(|widget| widget.style_context().has_class("now-title"))
        .and_then(|widget| widget.downcast_ref::<gtk::Label>())
        .unwrap();
    assert!(now_title.is_mapped());
    let timeline = widgets
        .iter()
        .find(|widget| widget.style_context().has_class("timeline"))
        .unwrap();
    let center_x = |widget: &gtk::Widget| {
        widget
            .translate_coordinates(&window, widget.allocated_width() / 2, 0)
            .unwrap()
            .0
    };
    assert!((center_x(timeline) - center_x(play.upcast_ref())).abs() <= 1);
    assert!((280..=720).contains(&timeline.allocated_width()));
    let original_title = now_title.text();
    let mute = widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .find(|button| button.tooltip_text().as_deref() == Some("Выключить звук"))
        .unwrap();
    mute.emit_clicked();
    pump(150);
    assert_eq!(mute.tooltip_text().as_deref(), Some("Включить звук"));
    assert!(read_saved_session().unwrap().muted);
    mute.emit_clicked();
    pump(150);
    assert_eq!(mute.tooltip_text().as_deref(), Some("Выключить звук"));
    assert!(!read_saved_session().unwrap().muted);

    let original_position = play_position();
    next.emit_clicked();
    pump(400);
    assert_ne!(now_title.text(), original_title);
    assert_eq!(play_position(), original_position);
    let previous = widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
        .find(|button| button.tooltip_text().as_deref() == Some("Предыдущий трек"))
        .unwrap();
    previous.emit_clicked();
    pump(400);
    assert_eq!(now_title.text(), original_title);
    assert_eq!(play_position(), original_position);
    for _ in 0..3 {
        next.emit_clicked();
        pump(45);
        previous.emit_clicked();
        pump(45);
    }
    pump(550);
    assert_eq!(now_title.text(), original_title);
    assert_eq!(play_position(), original_position);
    play.emit_clicked();
    pump(300);

    let transport_buttons: Vec<_> = widgets
        .iter()
        .filter(|widget| widget.style_context().has_class("transport-button"))
        .collect();
    assert_eq!(transport_buttons.len(), 4);
    for button in transport_buttons {
        assert_eq!(
            (button.allocated_width(), button.allocated_height()),
            if button.style_context().has_class("mode") {
                (40, 40)
            } else {
                (48, 48)
            }
        );
    }
    next.set_state_flags(gtk::StateFlags::PRELIGHT, false);
    pump(200);
    capture(window.upcast_ref(), "transport-next-hover");
    next.unset_state_flags(gtk::StateFlags::PRELIGHT);

    assert!(!play.gets_focus_on_click());
    assert!(play.can_focus());
    play.grab_focus();
    pump(240);
    let play_size = (play.allocated_width(), play.allocated_height());
    play.set_state_flags(gtk::StateFlags::ACTIVE, false);
    pump(190);
    assert_eq!((play.allocated_width(), play.allocated_height()), play_size);
    capture(window.upcast_ref(), "transport-pressed");
    play.unset_state_flags(gtk::StateFlags::ACTIVE);
    pump(190);
    assert!(play.is_focus());
    capture(window.upcast_ref(), "transport-focus");
    play.emit_clicked();
    pump(700);
    assert_eq!(play.tooltip_text().as_deref(), Some("Пауза"));
    window.set_focus(gtk::Widget::NONE);
    pump(100);
    capture(window.upcast_ref(), "player-playing");
    let record = widgets
        .iter()
        .find(|w| w.style_context().has_class("vinyl"))
        .unwrap();
    let record_pixels = || {
        let mut surface = gtk::cairo::ImageSurface::create(
            gtk::cairo::Format::ARgb32,
            record.allocated_width(),
            record.allocated_height(),
        )
        .unwrap();
        record.draw(&gtk::cairo::Context::new(&surface).unwrap());
        let pixels = surface.data().unwrap().to_vec();
        pixels
    };
    let moving = record_pixels();
    pump(160);
    assert!(
        moving != record_pixels(),
        "the record should rotate during playback"
    );
    assert!(widgets
        .iter()
        .filter(|w| w.style_context().has_class("timecode"))
        .filter_map(|w| w.downcast_ref::<gtk::Label>())
        .any(|label| label.text().starts_with("−0:")));
    play.emit_clicked();
    pump(550);
    let stopped = record_pixels();
    pump(160);
    assert!(stopped == record_pixels(), "the paused record should stop");
    assert_eq!(play.tooltip_text().as_deref(), Some("Воспроизвести"));
    assert!(widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Label>())
        .any(|label| label.text() == "Текст песни" && label.is_visible()));

    let nav: Vec<gtk::Button> = widgets
        .iter()
        .filter(|w| w.style_context().has_class("nav-item"))
        .filter_map(|w| w.clone().downcast().ok())
        .collect();
    assert_eq!(nav.len(), 2);
    widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
        .find(|button| button.label().as_deref() == Some("Вся очередь"))
        .unwrap()
        .emit_clicked();
    pump(350);
    assert!(nav[1].style_context().has_class("selected"));
    pump(600);
    let queue_play = descendants(window.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.tooltip_text().as_deref() == Some("Воспроизвести трек"))
        .unwrap();
    queue_play.emit_clicked();
    pump(200);
    assert_eq!(
        queue_play.tooltip_text().as_deref(),
        Some("Приостановить трек")
    );
    queue_play.emit_clicked();
    pump(200);
    assert_eq!(
        queue_play.tooltip_text().as_deref(),
        Some("Воспроизвести трек")
    );
    let covers: Vec<_> = descendants(window.upcast_ref())
        .into_iter()
        .filter(|w| w.style_context().has_class("queue-cover"))
        .filter_map(|w| w.downcast::<gtk::Stack>().ok())
        .collect();
    assert_eq!(covers.len(), 6);
    assert!(covers
        .iter()
        .take(4)
        .all(|cover| cover.visible_child_name().as_deref() == Some("image")));
    assert!(now_title.is_mapped());
    capture(window.upcast_ref(), "queue");
    let action = |index: usize, name: &str| {
        let list = descendants(window.upcast_ref())
            .into_iter()
            .find(|widget| widget.style_context().has_class("queue-list"))
            .unwrap()
            .downcast::<gtk::ListBox>()
            .unwrap();
        let row = list.row_at_index(index as i32).unwrap();
        let button = descendants(row.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.tooltip_text().as_deref() == Some("Действия с треком"))
            .unwrap();
        button.emit_clicked();
        let menu = gtk::Menu::for_attach_widget(&button)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::Menu>().ok())
            .unwrap();
        let item = menu
            .children()
            .into_iter()
            .find_map(|widget| {
                let item = widget.downcast::<gtk::MenuItem>().ok()?;
                (item.label().as_deref() == Some(name)).then_some(item)
            })
            .unwrap();
        assert!(item.is_visible());
        item.activate();
        menu.popdown();
        pump(180);
    };
    let original_queue = read_saved_session().unwrap().paths;
    action(0, "Скопировать название");
    assert_eq!(
        gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD)
            .wait_for_text()
            .as_deref(),
        Some(original_title.as_str())
    );
    action(2, "Переместить выше");
    assert_eq!(read_saved_session().unwrap().paths[1], original_queue[2]);
    action(1, "Поставить следующим");
    assert_eq!(
        read_saved_session().unwrap().play_next.as_ref(),
        Some(&original_queue[2])
    );
    action(1, "Убрать из очереди");
    let edited = read_saved_session().unwrap();
    assert!(!edited.paths.contains(&original_queue[2]));
    assert!(edited.play_next.is_none());
    assert_eq!(edited.current_path.as_ref(), Some(&original_queue[0]));

    let search = widgets
        .iter()
        .find_map(|w| w.clone().downcast::<gtk::SearchEntry>().ok())
        .unwrap();
    search.set_text(original_title.as_str());
    pump(450);
    capture(window.upcast_ref(), "queue-single-result");
    search.set_text("this track does not exist");
    pump(450);
    assert!(widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Stack>())
        .any(|stack| stack.visible_child_name().as_deref() == Some("no-results")));
    assert!(!widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Label>())
        .any(|label| label.text().starts_with("Двойной щелчок или Enter") && label.is_visible()));
    capture(window.upcast_ref(), "search-empty");
    search.emit_by_name::<()>("stop-search", &[]);
    assert!(search.text().is_empty());
    search.set_text("   ");
    pump(350);
    assert!(!widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Stack>())
        .any(|stack| stack.visible_child_name().as_deref() == Some("no-results")));
    nav[0].emit_clicked();
    pump(300);
    let lyrics_action = widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
        .find(|button| button.style_context().has_class("lyrics-action") && button.is_mapped())
        .unwrap();
    assert!(!lyrics_action.gets_focus_on_click());
    lyrics_action.grab_focus();
    window.set_focus_visible(true);
    assert!(lyrics_action.has_visible_focus());
    pump(220);
    capture(window.upcast_ref(), "lyrics-action-keyboard");
    lyrics_action.emit_clicked();
    pump(350);
    assert!(now_title.is_mapped());
    let seek_hint = widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Label>())
        .find(|label| label.text().starts_with("Двойной щелчок"))
        .unwrap();
    assert!(seek_hint.is_mapped());
    assert!(
        read_saved_session()
            .unwrap()
            .appearance
            .lyrics_seek_hint_seen
    );
    capture(window.upcast_ref(), "lyrics");
    let back = widgets
        .iter()
        .find(|w| w.style_context().has_class("lyrics-back"))
        .and_then(|w| w.downcast_ref::<gtk::Button>())
        .unwrap();
    assert_eq!((back.allocated_width(), back.allocated_height()), (44, 44));
    let focus_visible = window.gets_focus_visible();
    window.set_focus_visible(false);
    back.grab_focus();
    assert!(!back.has_visible_focus());
    pump(200);
    let back_fill = || {
        back.style_context()
            .style_property_for_state("background-color", back.state_flags())
            .get::<gtk::gdk::RGBA>()
            .unwrap()
            .alpha()
    };
    assert!(
        back_fill() < 0.5 / 255.0,
        "pointer focus alone must leave the arrow clear"
    );
    capture(window.upcast_ref(), "lyrics-back-normal");
    back.set_state_flags(gtk::StateFlags::PRELIGHT, false);
    pump(220);
    assert!(back_fill() > 0.04);
    capture(window.upcast_ref(), "lyrics-back-hover");
    back.set_state_flags(gtk::StateFlags::ACTIVE, false);
    pump(220);
    capture(window.upcast_ref(), "lyrics-back-pressed");
    back.unset_state_flags(gtk::StateFlags::PRELIGHT | gtk::StateFlags::ACTIVE);
    window.set_focus_visible(true);
    assert!(back.has_visible_focus());
    pump(220);
    assert!(back_fill() > 0.04);
    capture(window.upcast_ref(), "lyrics-back-keyboard");
    window.set_focus_visible(focus_visible);
    let lyrics_list = widgets
        .iter()
        .filter(|w| w.style_context().has_class("lyrics-list"))
        .find_map(|w| w.downcast_ref::<gtk::ListBox>())
        .unwrap();
    assert!(
        !descendants(lyrics_list.upcast_ref())
            .iter()
            .any(|w| w.is::<gtk::DrawingArea>()),
        "lyrics must have no progress bars"
    );
    let scroll = widgets
        .iter()
        .filter(|w| w.style_context().has_class("lyrics-scroll"))
        .find_map(|w| w.downcast_ref::<gtk::ScrolledWindow>())
        .unwrap();
    let expected_width = scroll.allocated_width().min(900);
    assert!((expected_width - 4..=expected_width).contains(&lyrics_list.allocated_width()));
    let lyric_row = lyrics_list.row_at_index(2).unwrap();
    assert_eq!(lyric_row.tooltip_text(), Some(seek_hint.text()));
    lyrics_list.emit_by_name::<()>("row-activated", &[&lyric_row]);
    play.emit_clicked();
    pump(1300);
    assert!(lyric_row.style_context().has_class("active"));
    assert_eq!(scroll.vscrollbar_policy(), gtk::PolicyType::External);
    assert!(!scroll.vscrollbar().unwrap().is_mapped());
    let adjustment = scroll.vadjustment();
    let centered = |row: &gtk::ListBoxRow| {
        let content = scroll
            .child()
            .unwrap()
            .downcast::<gtk::Viewport>()
            .unwrap()
            .child()
            .unwrap();
        let (_, y) = row.translate_coordinates(&content, 0, 0).unwrap();
        (f64::from(y) + f64::from(row.allocation().height()) / 2.0
            - adjustment.value()
            - adjustment.page_size() * 0.32)
            .abs()
            < 3.0
    };
    assert!(
        centered(&lyric_row),
        "active lyric should settle at the reading line"
    );
    let played_row = lyrics_list.row_at_index(1).unwrap();
    let played_label = descendants(played_row.upcast_ref())
        .into_iter()
        .find(|widget| widget.style_context().has_class("lyric-text"))
        .unwrap();
    let played_color = || {
        played_label
            .style_context()
            .color(played_label.state_flags())
    };
    let past_color = played_color();
    assert_eq!(past_color.alpha(), 1.0, "played lines stay readable");
    assert!(played_row.style_context().has_class("past"));
    capture(window.upcast_ref(), "lyrics-focused");
    play.emit_clicked();
    pump(250);
    window.resize(860, 620);
    pump(650);
    assert!(
        centered(&lyric_row),
        "resize should recenter the active lyric"
    );
    capture(window.upcast_ref(), "compact-lyrics");
    assert!(descendants(window.upcast_ref()).iter().any(|widget| {
        widget.style_context().has_class("sidebar") && widget.allocated_width() <= 84
    }));

    let resume = widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .find(|button| button.label().as_deref() == Some("К текущей строке"))
        .unwrap();
    let mut event = gtk::gdk::Event::new(gtk::gdk::EventType::Scroll);
    let pointer = scroll
        .display()
        .default_seat()
        .and_then(|seat| seat.pointer());
    event.set_device(pointer.as_ref());
    scroll.emit_by_name::<bool>("scroll-event", &[&event]);
    adjustment.set_value(0.0);
    pump(180);
    assert!(resume.is_visible());
    assert!(
        !played_row.style_context().has_class("past"),
        "manual browsing should remove emphasis from the playback boundary"
    );
    assert!(
        adjustment.value() < 3.0,
        "manual scrolling should suspend follow"
    );
    resume.emit_clicked();
    pump(1100);
    assert!(!resume.is_visible());
    assert!(centered(&lyric_row));

    let settings = gtk::Settings::default().unwrap();
    let animations = settings.property::<bool>("gtk-enable-animations");
    settings.set_property("gtk-enable-animations", false);
    for index in [3, 0] {
        let row = lyrics_list.row_at_index(index).unwrap();
        lyrics_list.emit_by_name::<()>("row-activated", &[&row]);
        for _ in 0..12 {
            pump(35);
            if row.style_context().has_class("active") {
                break;
            }
        }
        assert!(
            row.style_context().has_class("active"),
            "seek should activate row {index}, active: {:?}, clocks: {:?}",
            lyrics_list
                .children()
                .iter()
                .position(|w| w.style_context().has_class("active")),
            widgets
                .iter()
                .filter(|w| w.style_context().has_class("timecode"))
                .filter_map(|w| w.downcast_ref::<gtk::Label>())
                .map(|label| label.text())
                .collect::<Vec<_>>()
        );
        assert!(
            centered(&row),
            "edge lyric should snap to the reading line in reduced motion"
        );
    }
    settings.set_property("gtk-enable-animations", animations);
    let first = lyrics_list.row_at_index(0).unwrap();
    let last = lyrics_list.row_at_index(3).unwrap();
    for row in [&last, &first, &lyric_row] {
        lyrics_list.emit_by_name::<()>("row-activated", &[row]);
        pump(145);
    }
    pump(600);
    assert!(
        centered(&lyric_row),
        "latest seek wins during rapid retargeting"
    );
    let settled = adjustment.value();
    pump(500);
    assert_eq!(
        settled,
        adjustment.value(),
        "paused lyrics stay still after settling"
    );
    let redraws = Rc::new(Cell::new(0));
    let count = redraws.clone();
    let paint = window.connect_draw(move |_, _| {
        count.set(count.get() + 1);
        glib::Propagation::Proceed
    });
    pump(180);
    redraws.set(0);
    pump(500);
    assert_eq!(
        redraws.get(),
        0,
        "paused lyrics must stop drawing after settling"
    );
    window.disconnect(paint);

    let seek = widgets
        .iter()
        .find(|w| w.style_context().has_class("seek"))
        .and_then(|w| w.downcast_ref::<gtk::Scale>())
        .unwrap();
    for value in [650.0, 25.0, 460.0] {
        seek.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &value]);
    }
    pump(250);
    assert!(lyric_row.style_context().has_class("active") && centered(&lyric_row));

    let active_label = lyric_label(lyrics_list, 2).unwrap();
    let attrs = active_label
        .attributes()
        .expect("active line has letter highlighting");
    let pending = attrs
        .iterator()
        .into_iter()
        .flatten()
        .find(|attr| attr.type_() == gtk::pango::AttrType::ForegroundAlpha)
        .expect("remaining letters are dimmed");
    assert!(pending.start_index() > 0);
    assert!((pending.start_index() as usize) < active_label.text().len());
    assert!(lyric_label(lyrics_list, 0).unwrap().attributes().is_none());
    assert!(lyric_label(lyrics_list, 3).unwrap().attributes().is_none());
    seek.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &420.0]);
    pump(250);
    let earlier_attrs = active_label.attributes().unwrap();
    let earlier = earlier_attrs
        .iterator()
        .into_iter()
        .flatten()
        .find(|attr| attr.type_() == gtk::pango::AttrType::ForegroundAlpha)
        .unwrap();
    assert!(
        earlier.start_index() < pending.start_index(),
        "seek rewinds letter highlighting"
    );

    let long_run = std::env::var_os("OWN_PLAYER_TEST_FULL_PLAYBACK").is_some();
    if long_run {
        lyrics_list.emit_by_name::<()>("row-activated", &[&first]);
        play.emit_clicked();
        pump(5000);
        let second = lyrics_list.row_at_index(1).unwrap();
        assert!(second.style_context().has_class("active") && centered(&second));
        pump(8000);
        assert!(last.style_context().has_class("active") && centered(&last));
        pump(8000);
        assert_ne!(
            now_title.text(),
            original_title,
            "full playback advances the queue"
        );
        assert_eq!(play.tooltip_text().as_deref(), Some("Пауза"));
        play.emit_clicked();
        pump(250);
    }
    for widget in &widgets {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.tooltip_text().as_deref() == Some("К плееру") {
                button.emit_clicked();
            }
        }
    }
    pump(350);
    capture(window.upcast_ref(), "compact-player");
    assert!(
        now_title.allocated_width() >= 80,
        "compact transport should use spare width for the track title"
    );
    lyrics_action.emit_clicked();
    pump(350);
    assert!(!seek_hint.is_mapped());
    assert!(lyrics_list.allocated_width() < 900);
    // Full playback can advance to a different track and rebuild lyric rows.
    // Use the row in the current view rather than the detached previous one.
    pump_until(|| lyrics_list.row_at_index(0).is_some());
    let first = lyrics_list.row_at_index(0).unwrap();
    let label = first.child().unwrap().downcast::<gtk::Label>().unwrap();
    label.set_text(&"Очень длинная строка песни ".repeat(12));
    pump(220);
    assert!(label.layout().unwrap().line_count() > 1);
    assert!(label.allocated_width() <= lyrics_list.allocated_width());
    first.grab_focus();
    assert!(first.is_focus());
    back.emit_clicked();
    pump(350);
    let settings = widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .find(|button| button.tooltip_text().as_deref() == Some("Настройки"))
        .unwrap();
    let page = widgets
        .iter()
        .find(|w| w.style_context().has_class("settings-page"))
        .unwrap();
    let controls = descendants(page);
    assert!(!controls.iter().any(|widget| widget.is::<gtk::Scale>()));
    let speed_button = widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::MenuButton>())
        .find(|button| button.style_context().has_class("speed-button"))
        .unwrap();
    let speed_popover = speed_button.popover().unwrap();
    speed_button.set_active(true);
    pump(200);
    assert!(speed_popover.is_mapped());
    capture(window.upcast_ref(), "playback-speed");
    let speed_controls = descendants(speed_popover.upcast_ref());
    let plus = speed_controls
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
        .find(|button| button.tooltip_text().as_deref() == Some("Увеличить скорость"))
        .unwrap();
    let preset = speed_controls
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::ToggleButton>())
        .find(|button| button.tooltip_text().as_deref() == Some("1.25×"))
        .unwrap();
    speed_popover.popdown();
    let named_switch = |label: &str| {
        controls
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Switch>())
            .find(|switch| switch.tooltip_text().as_deref() == Some(label))
            .unwrap()
    };
    let covers = named_switch("Загружать обложки из Яндекс Музыки");
    let dynamic = named_switch("Динамический фон");
    let animate = named_switch("Анимации");
    let expanded = named_switch("Расширенная боковая панель");
    let csv = controls
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .find(|button| button.label().as_deref() == Some("Выбрать…"))
        .unwrap();
    settings.emit_clicked();
    pump(350);
    assert!(page.is_mapped() && settings.style_context().has_class("selected"));
    let rail = widgets
        .iter()
        .find(|w| w.style_context().has_class("sidebar"))
        .unwrap();
    let rail_controls = descendants(rail);
    let rail_buttons: Vec<_> = rail_controls
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
        .collect();
    assert_eq!(rail_buttons.len(), 5);
    for button in rail_buttons {
        assert!(button.allocated_width() >= 56);
        assert_eq!(button.allocated_height(), 56);
        assert!(descendants(button.upcast_ref())
            .iter()
            .filter_map(|widget| widget.downcast_ref::<gtk::Image>())
            .any(|image| image.pixel_size() == 24));
    }
    let clock = widgets
        .iter()
        .find(|w| w.style_context().has_class("timecode"))
        .and_then(|w| w.downcast_ref::<gtk::Label>())
        .unwrap();
    let before = clock.text();
    play.emit_clicked();
    nav[1].emit_clicked();
    pump(350);
    assert!(nav[1].style_context().has_class("selected"));
    settings.emit_clicked();
    pump(1250);
    assert!(page.is_mapped());
    assert_eq!(play.tooltip_text().as_deref(), Some("Пауза"));
    assert_ne!(clock.text(), before);
    play.emit_clicked();

    let directory = read_saved_session().unwrap().paths[0]
        .parent()
        .unwrap()
        .to_path_buf();
    let luminance = |color: gtk::gdk::RGBA| {
        let linear = |channel: f64| {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.red())
            + 0.7152 * linear(color.green())
            + 0.0722 * linear(color.blue())
    };
    for (name, rgba) in [
        ("warm", 0xc33250ff),
        ("cool", 0x3078d5ff),
        ("neutral", 0x808080ff),
    ] {
        let path = directory.join(format!("palette-{name}.png"));
        let pixbuf = Pixbuf::new(Colorspace::Rgb, false, 8, 16, 16).unwrap();
        pixbuf.fill(rgba);
        pixbuf.savev(&path, "png", &[]).unwrap();
        let original = stage_color();
        theme::set_cover(Some(&path));
        pump(240);
        assert_ne!(stage_color(), original);
        assert!(page.is_mapped() && settings.style_context().has_class("selected"));
        for (foreground, background) in [
            ("md_on_surface", "md_surface"),
            ("md_on_surface_variant", "md_surface_container"),
            ("md_on_primary", "md_primary"),
            ("md_on_secondary_container", "md_secondary_container"),
        ] {
            let context = window.style_context();
            let fg = luminance(context.lookup_color(foreground).unwrap());
            let bg = luminance(context.lookup_color(background).unwrap());
            assert!(
                (fg.max(bg) + 0.05) / (fg.min(bg) + 0.05) >= 4.5,
                "{foreground} on {background} for {name}"
            );
        }
        capture(window.upcast_ref(), &format!("settings-{name}"));
    }
    theme::set_cover(Some(&artwork));
    dynamic.set_active(false);
    assert_eq!(
        stage_color(),
        window
            .style_context()
            .lookup_color("md_surface_low")
            .unwrap()
    );
    animate.set_active(false);
    expanded.set_active(true);
    pump(220);
    assert!(rail.allocated_width() >= 156);
    window.resize(780, 510);
    pump(350);
    assert!(
        window.size().0 <= 800 && window.size().1 <= 530,
        "{:?}",
        window.size()
    );

    plus.emit_clicked();
    plus.emit_clicked();
    assert!(!preset.is_active());
    pump(350);
    assert!((read_saved_session().unwrap().speed - 1.10).abs() < 0.001);
    preset.emit_clicked();
    assert!(preset.is_active());
    assert_eq!(speed_button.label().as_deref(), Some("1.25×"));
    preset.emit_clicked();
    assert!(preset.is_active());
    covers.set_active(true);
    assert!(read_saved_session().unwrap().online_covers);
    covers.set_active(false);
    assert!(!read_saved_session().unwrap().online_covers);
    covers.set_active(true);
    pump(400);
    assert!(read_saved_session().unwrap().online_covers);
    assert!((read_saved_session().unwrap().speed - 1.25).abs() < 0.001);

    let source = read_saved_session().unwrap().paths[0].with_file_name("playlist-test.csv");
    std::fs::write(
        &source,
        "id,title,artists,album,x,y,album_id\n1,Test,Artist,Album,,,42\n",
    )
    .unwrap();
    let install_csv = |source: &Path| {
        csv.emit_clicked();
        pump(300);
        let chooser = gtk::Window::list_toplevels()
            .into_iter()
            .find_map(|w| {
                w.downcast::<gtk::FileChooserDialog>()
                    .ok()
                    .filter(|w| w.is_mapped())
            })
            .unwrap();
        assert!(chooser.set_filename(source));
        pump(600);
        chooser.response(gtk::ResponseType::Accept);
        pump(400);
        assert!(cover_art::has_index());
        assert_eq!(csv.label().as_deref(), Some("Заменить…"));
    };
    install_csv(&source);
    let replacement = source.with_file_name("playlist-replacement.csv");
    let payload = "id,title,artists,album,x,y,album_id\n2,Other,Artist,Album,,,84\n";
    std::fs::write(&replacement, payload).unwrap();
    install_csv(&replacement);
    assert_eq!(
        std::fs::read_to_string(
            PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
                .join("own-player/playlist.csv")
        )
        .unwrap(),
        payload
    );
    let idle_draws = Rc::new(Cell::new(0));
    let draws = idle_draws.clone();
    let handler = page.connect_draw(move |_, _| {
        draws.set(draws.get() + 1);
        glib::Propagation::Proceed
    });
    pump(400);
    let settled_draws = idle_draws.get();
    pump(750);
    assert_eq!(
        idle_draws.get(),
        settled_draws,
        "paused settings should not redraw at idle"
    );
    page.disconnect(handler);
    assert!(now_title.is_mapped());
    capture(window.upcast_ref(), "settings");
    settings.emit_clicked();
    pump(300);
    assert!(!page.is_mapped());
    assert!(
        window.size().0 <= 860,
        "layout must fit minimum width: {:?}",
        window.size()
    );
    assert!(
        window.size().1 <= 620,
        "layout must fit minimum height: {:?}",
        window.size()
    );
    window.close();
    pump(300);
    build_ui(&app);
    pump(600);
    let restarted = app.windows().into_iter().next().unwrap();
    let restarted_widgets = descendants(restarted.upcast_ref());
    let reopened = restarted_widgets
        .iter()
        .find(|w| w.style_context().has_class("settings-page"))
        .unwrap();
    let restarted_controls = descendants(reopened);
    let restored_speed = restarted_widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::MenuButton>())
        .find(|button| button.style_context().has_class("speed-button"))
        .unwrap();
    assert_eq!(restored_speed.label().as_deref(), Some("1.25×"));
    let restored_popover = restored_speed.popover().unwrap();
    assert!(descendants(restored_popover.upcast_ref())
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::ToggleButton>())
        .any(|preset| preset.tooltip_text().as_deref() == Some("1.25×") && preset.is_active()));
    for (name, active) in [
        ("Загружать обложки из Яндекс Музыки", true),
        ("Динамический фон", false),
        ("Анимации", false),
        ("Расширенная боковая панель", true),
    ] {
        let switch = restarted_controls
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Switch>())
            .find(|switch| switch.tooltip_text().as_deref() == Some(name))
            .unwrap();
        assert_eq!(switch.is_active(), active, "{name}");
    }
    assert!(restarted_controls
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .any(|b| b.label().as_deref() == Some("Заменить…")));
    let hero_cover = restarted_widgets
        .iter()
        .find(|w| w.style_context().has_class("cover-frame"))
        .unwrap();
    for (width, height) in [(1920, 1080), (1280, 720), (900, 600), (700, 500)] {
        restarted.resize(width, height);
        pump(250);
        assert_eq!(restarted.size(), (width, height));
        assert_eq!(hero_cover.allocated_width(), hero_cover.allocated_height());
        assert!(hero_cover.allocated_height() <= height - 120);
        capture(restarted.upcast_ref(), &format!("layout-{width}x{height}"));
    }
    restarted.close();

    let long_model = test_model();
    {
        let mut state = long_model.borrow_mut();
        state.tracks[0].title =
            "Очень длинное название трека, которое должно аккуратно переноситься и обрезаться"
                .into();
        state.tracks[0].artist = "Исполнитель с длинным именем и приглашёнными музыкантами".into();
        state.tracks[0].album =
            "Очень длинное название альбома с дополнительной информацией".into();
    }
    let image_dir = read_saved_session().unwrap().paths[0]
        .parent()
        .unwrap()
        .to_path_buf();
    for (index, (width, height, color)) in [(84, 252, 0xbd315aff), (320, 92, 0x246abaff)]
        .into_iter()
        .enumerate()
    {
        let path = image_dir.join(format!("geometry-cover-{index}.png"));
        let cover = Pixbuf::new(Colorspace::Rgb, false, 8, width, height).unwrap();
        cover.fill(color);
        cover.savev(&path, "png", &[]).unwrap();
        long_model.borrow_mut().tracks[index].cover_path = Some(path);
    }
    {
        let mut state = long_model.borrow_mut();
        state.tracks[1].title = "Я".into();
        state.tracks[1].artist = "А".into();
    }
    let (long_page, views) = now_playing::build(&long_model, || {});
    let long_window = gtk::Window::new(gtk::WindowType::Toplevel);
    long_window.set_default_size(604, 390);
    long_window.add(&long_page);
    long_window.show_all();
    pump(400);
    assert!(
        long_window.size().0 <= 604,
        "long titles must not widen the stage: {:?}",
        long_window.size()
    );
    let long_title = descendants(&long_page)
        .into_iter()
        .find(|widget| widget.style_context().has_class("track-title"))
        .unwrap()
        .downcast::<gtk::Label>()
        .unwrap();
    assert!((2..=3).contains(&long_title.layout().unwrap().line_count()));
    assert_eq!(
        long_title.tooltip_text().as_deref(),
        Some(long_title.text().as_str())
    );
    capture(&long_window, "long-title");
    let elements = descendants(&long_page);
    let frame = elements
        .iter()
        .find(|w| w.style_context().has_class("cover-frame"))
        .unwrap();
    let details = elements
        .iter()
        .find(|widget| widget.style_context().has_class("track-details"))
        .unwrap();
    let bounds = || {
        let (x, y) = frame.translate_coordinates(&long_window, 0, 0).unwrap();
        (x, y, frame.allocated_width(), frame.allocated_height())
    };
    for (width, height) in [(820, 450), (560, 370)] {
        long_window.resize(width, height);
        pump(400);
        assert_eq!(long_window.size(), (width, height));
        let initial = bounds();
        assert_eq!(initial.2, initial.3);
        assert!((112..=280).contains(&initial.2));
        let (details_x, _) = details.translate_coordinates(&long_window, 0, 0).unwrap();
        assert_eq!(
            details_x - (initial.0 + initial.2),
            if width < 680 { 20 } else { 28 }
        );
        let right_margin = width - details_x - details.allocated_width();
        let record = elements
            .iter()
            .find(|w| w.style_context().has_class("vinyl"))
            .unwrap();
        assert!(record.is_mapped());
        assert!(
            (initial.0 - record.allocated_width() - right_margin).abs() <= 1,
            "the track group must be centered"
        );

        for index in [1, 0, 1, 0] {
            long_model.borrow_mut().current = Some(index);
            request_ui(&long_model);
            pump(380);
            assert_eq!(long_window.size(), (width, height));
            assert_eq!(bounds(), initial);
            assert_eq!(long_title.text(), long_model.borrow().tracks[index].title);
        }
        capture(&long_window, &format!("cover-stable-{width}"));
    }
    for height in [650, 850] {
        long_window.resize(1024, height);
        pump(400);
        assert_eq!(long_window.size(), (1024, height));
        let shelf = descendants(&long_page)
            .into_iter()
            .find(|widget| widget.style_context().has_class("upcoming-panel"))
            .unwrap();
        assert!(shelf.is_mapped());
        let (_, shelf_y) = shelf.translate_coordinates(&long_window, 0, 0).unwrap();
        let (_, details_y) = details.translate_coordinates(&long_window, 0, 0).unwrap();
        assert!(
            details_y + details.allocated_height() <= shelf_y,
            "long metadata must fit above the horizontal queue shelf"
        );
        let initial = bounds();
        let (_, details_y) = details.translate_coordinates(&long_window, 0, 0).unwrap();
        assert!(
            details_y >= initial.1 + initial.3,
            "stacked metadata belongs below the cover"
        );
        for index in [1, 0] {
            long_model.borrow_mut().current = Some(index);
            request_ui(&long_model);
            pump(380);
            assert_eq!(
                bounds(),
                initial,
                "stacked cover stays stable across title lengths"
            );
        }
        capture(&long_window, &format!("stacked-long-title-{height}"));
    }
    let lyrics = elements
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .find(|w| w.style_context().has_class("lyrics-action"))
        .unwrap();
    lyrics.emit_clicked();
    assert_eq!(views.visible_child_name().as_deref(), Some("lyrics"));
    pump(350);
    long_window.close();
    // Verify empty state using a separate player model without touching the session.
    let empty = test_model();
    empty.borrow_mut().tracks.clear();
    empty.borrow_mut().current = None;
    let (page, _) = now_playing::build(&empty, || {});
    let empty_window = gtk::Window::new(gtk::WindowType::Toplevel);
    empty_window.set_default_size(860, 620);
    empty_window.add(&page);
    empty_window.show_all();
    pump(200);
    assert!(descendants(&page)
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Button>())
        .any(|button| button.label().as_deref() == Some("Добавить музыку") && button.is_visible()));
    capture(&empty_window, "empty-player");
    empty_window.close();
    let large_queue = test_model();
    {
        let mut state = large_queue.borrow_mut();
        state.tracks = (0..10_000)
            .map(|i| {
                let mut track = preview_track(image_dir.join(format!("queue-{i}.wav")));
                track.title = format!("Трек {i} {}", "длинное название ".repeat(8));
                track.artist = "Первый исполнитель, Второй исполнитель".into();
                track.duration = 123.0;
                track.cover_path = Some(artwork.clone());
                track
            })
            .collect();
        state.current = Some(9_999);
    }
    let (queue, search) = build_queue_page(&large_queue);
    let queue_window = gtk::Window::new(gtk::WindowType::Toplevel);
    queue_window.set_default_size(640, 420);
    queue_window.add(&queue);
    queue_window.show_all();
    pump(450);
    let queue_widgets = descendants(&queue);
    let list = queue_widgets
        .iter()
        .find_map(|w| w.downcast_ref::<gtk::ListBox>())
        .unwrap();
    let scroll = queue_widgets
        .iter()
        .find(|widget| widget.style_context().has_class("queue-card"))
        .and_then(|widget| widget.downcast_ref::<gtk::ScrolledWindow>())
        .unwrap();
    let adjustment = scroll.vadjustment();
    let first_track = || {
        list.row_at_index(0).and_then(|row| {
            descendants(row.upcast_ref())
                .into_iter()
                .find_map(|widget| {
                    widget
                        .style_context()
                        .has_class("queue-title")
                        .then(|| widget.downcast::<gtk::Label>().unwrap().text())
                })
        })
    };
    let queue_actions = queue_widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::MenuButton>())
        .find(|button| button.tooltip_text().as_deref() == Some("Действия с очередью"))
        .unwrap();
    let jump = queue_actions
        .popup()
        .unwrap()
        .children()
        .into_iter()
        .find_map(|w| {
            let item = w.downcast::<gtk::MenuItem>().ok()?;
            (item.label().as_deref() == Some("Показать текущий трек")).then_some(item)
        })
        .unwrap();

    assert!(queue_widgets
        .iter()
        .filter(|widget| widget.style_context().has_class("queue-album"))
        .all(|widget| !widget.is_mapped()));
    queue_window.resize(1040, 620);
    pump(300);
    assert!(queue_widgets
        .iter()
        .filter(|widget| widget.style_context().has_class("queue-album"))
        .any(|widget| widget.is_mapped()));
    capture(&queue_window, "queue-wide");
    queue_window.resize(640, 420);
    pump(300);
    let row_height = f64::from(list.row_at_index(0).unwrap().allocated_height());
    assert!((56.0..=64.0).contains(&row_height));
    assert_eq!(adjustment.upper(), 10_000.0 * row_height);
    assert_eq!(
        list.allocated_height(),
        list.children().len() as i32 * row_height as i32
    );
    let rendered_limit =
        (adjustment.page_size() / row_height).ceil() as usize + QUEUE_OVERSCAN * 2 + 1;
    assert!(list.children().len() <= rendered_limit);
    let first_row = list.row_at_index(0).unwrap();
    adjustment.set_value(row_height * 3.5);
    pump(100);
    assert_eq!(list.row_at_index(0).as_ref(), Some(&first_row));
    adjustment.set_value(row_height * 6_000.0);
    pump_until(|| first_track().is_some_and(|title| title.starts_with("Трек 5996 ")));
    assert!(list.children().len() <= rendered_limit);
    jump.activate();
    pump(450);
    assert_eq!(
        adjustment.value(),
        adjustment.upper() - adjustment.page_size()
    );
    let last = list.row_at_index(list.children().len() as i32 - 1).unwrap();
    assert!(last.style_context().has_class("playing"));
    assert!(descendants(last.upcast_ref())
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Stack>())
        .any(|stack| stack.visible_child_name().as_deref() == Some("image")));
    assert!(last.is_selected() && last.is_focus());
    let window_handle = queue_window.window().unwrap();
    window_handle.focus(0);
    let pointer = queue_window
        .display()
        .default_seat()
        .unwrap()
        .pointer()
        .unwrap();
    let (x, y) = last.translate_coordinates(&queue_window, 12, 12).unwrap();
    let (root_x, root_y) = window_handle.root_origin();
    pointer.warp(
        &gtk::gdk::Screen::default().unwrap(),
        root_x + x,
        root_y + y,
    );
    pump_until(|| last.state_flags().contains(gtk::StateFlags::PRELIGHT));
    let press = |keyval: gtk::gdk::keys::Key| {
        for event in [
            gtk::gdk::EventType::KeyPress,
            gtk::gdk::EventType::KeyRelease,
        ] {
            assert!(gtk::gdk::test_simulate_key(
                &window_handle,
                -1,
                -1,
                *keyval,
                gtk::gdk::ModifierType::empty(),
                event,
            ));
        }
    };
    capture(&queue_window, "queue-large");
    press(gtk::gdk::keys::constants::Home);
    pump_until(|| adjustment.value() == 0.0);
    press(gtk::gdk::keys::constants::Page_Down);
    pump_until(|| adjustment.value() > 0.0);
    press(gtk::gdk::keys::constants::End);
    pump_until(|| adjustment.value() == adjustment.upper() - adjustment.page_size());
    assert_eq!(list.selected_rows().len(), 1);
    search.set_text("трек 5678 длинное");
    pump(400);
    assert_eq!(adjustment.value(), 0.0);
    assert_eq!(list.children().len(), 1);
    assert!(first_track().is_some_and(|title| title.starts_with("Трек 5678 ")));
    search.emit_by_name::<()>("stop-search", &[]);
    pump(400);
    assert!(first_track().is_some_and(|title| title.starts_with("Трек 0 ")));
    assert!(list.children().len() <= rendered_limit);
    search.set_text("нет такого трека");
    pump_until(|| list.children().is_empty());
    assert!(queue_widgets
        .iter()
        .filter(|widget| widget.style_context().has_class("queue-columns"))
        .all(|widget| !widget.is_mapped()));
    jump.activate();
    pump_until(|| {
        list.selected_row()
            .is_some_and(|row| row.style_context().has_class("playing"))
    });
    assert_eq!(
        adjustment.value(),
        adjustment.upper() - adjustment.page_size()
    );
    {
        let mut state = large_queue.borrow_mut();
        state.tracks.clear();
        state.current = None;
        state.library_revision += 1;
        state.updates.request();
    }
    pump(300);
    assert!(queue_widgets
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Stack>())
        .any(|stack| stack.visible_child_name().as_deref() == Some("empty")));
    queue_window.close();

    // Auto lookup waits for metadata, deduplicates jobs and attaches by path.
    let automatic = test_model();
    start_lyrics_job(&automatic, false);
    assert!(automatic.borrow().lyrics_pending.is_empty());
    {
        let mut state = automatic.borrow_mut();
        let paths = state
            .tracks
            .iter()
            .map(|track| track.path.clone())
            .collect();
        state.metadata_ready = paths;
    }
    start_lyrics_job(&automatic, false);
    start_lyrics_job(&automatic, false);
    assert_eq!(automatic.borrow().lyrics_pending.len(), 1);
    automatic.borrow_mut().current = Some(1);
    start_lyrics_job(&automatic, false);
    assert_eq!(automatic.borrow().lyrics_pending.len(), 2);
    pump_until(|| automatic.borrow().lyrics_pending.is_empty());
    {
        let state = automatic.borrow();
        assert!(state.lyrics_pending.is_empty());
        assert!(
            !state.tracks[0].lyrics.is_empty(),
            "lookup results: {:?}",
            state.lyrics_results
        );
        assert_eq!(state.tracks[0].lyrics[0].text, "first");
        assert_eq!(state.tracks[1].lyrics[0].text, "second");
    }
    // No repeated network attempts after a failure; explicit retry remains available.
    {
        let mut state = automatic.borrow_mut();
        state.tracks[1].lyrics.clear();
        state.tracks[1].title.clear();
    }
    start_lyrics_job(&automatic, true);
    pump_until(|| automatic.borrow().lyrics_pending.is_empty());
    assert!(automatic.borrow().lyrics_pending.is_empty());
    start_lyrics_job(&automatic, false);
    assert!(automatic.borrow().lyrics_pending.is_empty());
    automatic.borrow_mut().tracks[1].title = "second".into();
    start_lyrics_job(&automatic, true);
    pump_until(|| automatic.borrow().lyrics_pending.is_empty());
    assert_eq!(automatic.borrow().tracks[1].lyrics[0].text, "second");
    window.close();
    pump(450);

    let saved = read_saved_session().unwrap();
    let folder = saved.paths[0].parent().unwrap().to_path_buf();
    let gone = folder.join("temporarily-unavailable.wav");
    let broken = folder.join("broken.wav");
    std::fs::write(&broken, "broken audio").unwrap();
    let good = saved.paths[0].clone();
    session::save(Session {
        paths: vec![broken.clone(), good.clone(), gone.clone()],
        current_path: Some(gone.clone()),
        repeat: 0,
        ..saved
    })
    .unwrap();
    session::flush();
    let restored = test_model();
    restored.borrow_mut().tracks.clear();
    restore_session(&restored);
    assert_eq!(restored.borrow().current, Some(2));
    assert_eq!(restored.borrow().tracks[2].path, gone);
    save_session(&restored);
    assert_eq!(
        read_saved_session().unwrap().paths,
        vec![broken.clone(), good.clone(), gone.clone()]
    );

    start_mpv(&restored, &app);
    pump_until(|| restored.borrow().mpv.is_some());
    play_index(&restored, 0);
    for _ in 0..30 {
        pump(35);
        if restored.borrow().current == Some(1) {
            break;
        }
    }
    assert_eq!(
        restored.borrow().current,
        Some(1),
        "broken file should be skipped"
    );
    play_index(&restored, 2);
    for _ in 0..20 {
        pump(30);
        if !restored.borrow().mpv.as_ref().unwrap().has_track() {
            break;
        }
    }
    std::fs::copy(&restored.borrow().tracks[1].path, &gone).unwrap();
    play_index(&restored, 2);
    pump(300);
    next_track(&restored, 1, false);
    assert!(
        restored.borrow().mpv.as_ref().unwrap().loaded,
        "manual next must preserve playback at the end"
    );
    let before = read_saved_session().unwrap();
    let current = before.current_path.as_ref().unwrap();
    let first = &before.paths[0];
    assert!(edit_queue(
        &restored,
        current,
        QueueEdit::Drop(first, false)
    ));
    let edited = read_saved_session().unwrap();
    assert_eq!(edited.paths.first(), Some(current));
    assert_eq!(edited.current_path, before.current_path);
    assert!(restored.borrow().mpv.as_ref().unwrap().loaded);
    let next = edited.paths.last().unwrap();
    assert!(edit_queue(&restored, next, QueueEdit::PlayNext));
    next_track(&restored, 1, false);
    let advanced = read_saved_session().unwrap();
    assert_eq!(advanced.current_path.as_ref(), Some(next));
    assert!(advanced.play_next.is_none());
    // Clear must also cancel imports whose worker has not delivered paths yet.
    append_paths(&restored, vec![good.clone()]);
    clear_queue(&restored);
    pump_until(|| restored.borrow().imports_pending == 0);
    assert!(restored.borrow().tracks.is_empty());
    assert!(read_saved_session().unwrap().paths.is_empty());
    assert!(!restored.borrow().mpv.as_ref().unwrap().loaded);
    append_paths(&restored, vec![folder.join("empty")]);
    pump(350);
    assert!(restored
        .borrow()
        .status
        .as_deref()
        .unwrap()
        .contains("не найдены"));
    let short_c = folder.join("quick-c.wav");
    let short_d = folder.join("quick-d.wav");
    for path in [&short_c, &short_d] {
        assert!(std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(&good)
            .args(["-t", "1.2"])
            .arg(path)
            .status()
            .unwrap()
            .success());
    }
    {
        let mut state = restored.borrow_mut();
        state.tracks = [good, broken, short_c, short_d]
            .into_iter()
            .map(preview_track)
            .collect();
    }
    play_index(&restored, 0);
    play_index(&restored, 1);
    play_index(&restored, 2);
    pump_until(|| {
        let state = restored.borrow();
        state.current == Some(2) && state.mpv.as_ref().unwrap().loaded
    });
    pump_until(|| {
        let state = restored.borrow();
        state.current == Some(3) && state.mpv.as_ref().unwrap().loaded
    });
    pump_until(|| !restored.borrow().mpv.as_ref().unwrap().has_track());
    assert!(restored.borrow().failed_tracks.is_empty());
    shutdown_mpv(&mut restored.borrow_mut());
}

#[test]
#[ignore = "Run under Xvfb to measure construction of a 10000-track queue"]
fn gtk_queue_benchmark() {
    gtk::init().unwrap();
    theme::install(&gtk::gdk::Screen::default().unwrap());
    for _ in 0..3 {
        let model = test_model();
        model.borrow_mut().tracks = (0..10_000)
            .map(|index| {
                let mut track = preview_track(PathBuf::from(format!("/fixture/track-{index}.wav")));
                track.title = format!("Трек {index} {}", "длинное название ".repeat(8));
                track.duration = 120.0;
                track
            })
            .collect();
        let (page, _) = build_queue_page(&model);
        let list = descendants(&page)
            .into_iter()
            .find_map(|w| w.downcast::<gtk::ListBox>().ok())
            .unwrap();
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_default_size(900, 620);
        window.add(&page);
        let scroll = descendants(&page)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
            .unwrap();
        let adjustment = scroll.vadjustment();
        let start = Instant::now();
        let longest_gap = Rc::new(Cell::new(Duration::ZERO));
        let maximum = longest_gap.clone();
        let mut last = start;
        let timer = glib::timeout_add_local(Duration::from_millis(16), move || {
            let now = Instant::now();
            maximum.set(maximum.get().max(now - last));
            last = now;
            glib::ControlFlow::Continue
        });
        window.show_all();
        pump_until(|| {
            list.row_at_index(0)
                .is_some_and(|row| row.allocated_height() > 1)
                && adjustment.upper() > adjustment.page_size()
        });
        let initial = start.elapsed();
        let row_height = f64::from(list.row_at_index(0).unwrap().allocated_height());
        let bound = (adjustment.page_size() / row_height).ceil() as usize + QUEUE_OVERSCAN * 2 + 1;
        assert!(list.children().len() <= bound);
        let jump = Instant::now();
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        pump_until(|| {
            let count = list.children().len();
            count > 0
                && descendants(list.row_at_index(count as i32 - 1).unwrap().upcast_ref())
                    .into_iter()
                    .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
                    .any(|label| label.text().starts_with("Трек 9999 "))
        });
        let jump = jump.elapsed();
        assert!(list.children().len() <= bound);
        pump(80);
        assert_eq!(
            list.allocated_width(),
            scroll.child().unwrap().allocated_width() - 12
        );
        assert_eq!(
            list.allocated_height(),
            list.children().len() as i32 * row_height as i32
        );
        let last_row = list.row_at_index(list.children().len() as i32 - 1).unwrap();
        let (_, y) = last_row.translate_coordinates(&scroll, 0, 0).unwrap();
        assert_eq!(y + last_row.allocated_height(), scroll.allocated_height());
        eprintln!(
            "queue=10000 initial_ms={} end_jump_ms={} live_rows={} longest_gtk_gap_ms={}",
            initial.as_millis(),
            jump.as_millis(),
            list.children().len(),
            longest_gap.get().as_millis()
        );
        timer.remove();
        window.close();
        pump(100);
    }
}

#[test]
#[ignore = "Requires an isolated X11 display"]
fn gtk_lyric_motion() {
    gtk::init().unwrap();
    set_decorative_motion(true);
    gtk::Settings::default()
        .unwrap()
        .set_property("gtk-enable-animations", true);
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    let label = gtk::Label::new(Some("Тихо"));
    window.add(&label);
    window.show_all();
    pump(100);
    let follow = motion::Tween::new(0.0);
    let visible = Rc::new(Cell::new(0.0));
    let scrolling = visible.clone();
    follow.animate_smooth(&label, 1.0, 420, move |_, value| scrolling.set(value));
    pump(100);
    assert!(
        visible.get() > 0.0 && visible.get() < 0.25,
        "lyric scrolling should accelerate gently instead of jumping forward"
    );
    pump(180);
    assert!(
        visible.get() > 0.5 && visible.get() < 1.0,
        "line scrolling should still be easing toward its destination"
    );
    pump(240);
    assert_eq!(visible.get(), 1.0);
    let position = Rc::new(Cell::new(0.85));
    let playing = Rc::new(Cell::new(true));
    let samples = Rc::new(Cell::new(0));
    let boundaries = Rc::new(Cell::new(0));
    let motion = motion::LyricHighlight::new(
        {
            let position = position.clone();
            let playing = playing.clone();
            let samples = samples.clone();
            move || {
                samples.set(samples.get() + 1);
                Some((position.get(), playing.get(), 1.0))
            }
        },
        {
            let boundaries = boundaries.clone();
            move || boundaries.set(boundaries.get() + 1)
        },
    );
    let weak_motion = Rc::downgrade(&motion);
    window.connect_unmap(move |_| {
        if let Some(motion) = weak_motion.upgrade() {
            motion.clear();
        }
    });
    let alpha = |byte| {
        label
            .attributes()
            .unwrap()
            .iterator()
            .into_iter()
            .flatten()
            .filter(|attr| attr.type_() == gtk::pango::AttrType::ForegroundAlpha)
            .find(|attr| attr.start_index() <= byte && byte < attr.end_index())
            .map_or(u16::MAX, |attr| {
                attr.downcast_ref::<gtk::pango::AttrInt>().unwrap().value() as u16
            })
    };
    motion.update(&label, 0.0, 4.0);
    let dim = alpha(2);
    // No model/UI dispatch between these samples: only the widget frame clock.
    position.set(0.92);
    pump(80);
    let first = alpha(2);
    position.set(0.96);
    pump(45);
    let second = alpha(2);
    position.set(1.0);
    pump(45);
    assert!(dim < first && first < second && second < alpha(2));
    playing.set(false);
    motion.update(&label, 0.0, 4.0);
    let paused_samples = samples.get();
    pump(100);
    assert_eq!(
        samples.get(),
        paused_samples,
        "pause removes the frame callback"
    );
    position.set(0.90);
    motion.update(&label, 0.0, 4.0);
    assert!(
        alpha(2) < first,
        "paused seeking rewinds the fade immediately"
    );
    playing.set(true);
    motion.update(&label, 0.0, 4.0);
    window.hide();
    assert!(label.attributes().is_none());
    let hidden_samples = samples.get();
    pump(100);
    assert_eq!(
        samples.get(),
        hidden_samples,
        "hidden lyrics must not run a clock"
    );
    window.show_all();
    pump(50);
    motion.update(&label, 0.0, 4.0);
    position.set(4.0);
    pump(60);
    assert_eq!(
        boundaries.get(),
        1,
        "line boundaries notify the view without waiting for IPC"
    );
    let ended_samples = samples.get();
    pump(100);
    assert_eq!(samples.get(), ended_samples);
    position.set(0.5);
    motion.update(&label, 0.0, 4.0);
    let resting_samples = samples.get();
    pump(100);
    assert_eq!(
        samples.get(),
        resting_samples,
        "between fades the renderer should wait, not run a frame clock"
    );
    gtk::Settings::default()
        .unwrap()
        .set_property("gtk-enable-animations", false);
    position.set(0.9);
    motion.update(&label, 0.0, 4.0);
    assert!(label.attributes().is_none());
    let reduced_samples = samples.get();
    pump(100);
    assert_eq!(samples.get(), reduced_samples);
    gtk::Settings::default()
        .unwrap()
        .set_property("gtk-enable-animations", true);
    label.set_text("First second");
    playing.set(false);
    position.set(0.5);
    motion.update_timed(&label, "<00:00>First <00:01>second<00:02>", 0.0, 2.0, 6.5);
    assert_eq!(alpha(6), lyric_timing::DIM);
    let paused_samples = samples.get();
    pump(80);
    assert_eq!(
        samples.get(),
        paused_samples,
        "paused exact lyrics have no frame callback"
    );
    position.set(1.0);
    motion.update_timed(&label, "<00:00>First <00:01>second<00:02>", 0.0, 2.0, 6.5);
    assert_eq!(
        alpha(6),
        u16::MAX,
        "seek to an exact cue updates while paused"
    );
    position.set(0.5);
    motion.update_timed(&label, "<00:00>First <00:01>second<00:02>", 0.0, 2.0, 6.5);
    assert_eq!(
        alpha(6),
        lyric_timing::DIM,
        "backward seek clears the future word"
    );
    window.remove(&label);
    let next = gtk::Label::new(Some("Next track"));
    window.add(&next);
    window.show_all();
    pump(40);
    motion.update_timed(&next, "<00:00>Next <00:01>track<00:02>", 0.0, 2.0, 6.5);
    assert!(
        label.attributes().is_none(),
        "changing the target clears the old track's attributes"
    );
    window.close();
}

/// Measure the actual GTK attributes against known word onsets. This is not a
/// vocal aligner: it diagnoses rendering delay independently of source quality.
#[test]
#[ignore = "Requires an isolated X11 display; scripts/check-lyrics.sh"]
fn gtk_lyric_timing_probe() {
    gtk::init().unwrap();
    set_decorative_motion(true);
    gtk::Settings::default()
        .unwrap()
        .set_property("gtk-enable-animations", true);
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    let label = gtk::Label::new(Some("One two three"));
    window.add(&label);
    window.show_all();
    pump(60);
    let mut reports = Vec::new();
    for speed in [0.5, 1.0, 1.5, 2.0] {
        let started = Instant::now();
        let motion = motion::LyricHighlight::new(
            move || Some((started.elapsed().as_secs_f64() * speed, true, speed)),
            || {},
        );
        motion.update_timed(
            &label,
            "<00:00.100>One <00:00.400>two <00:00.700>three<00:01.000>",
            0.1,
            1.1,
            6.5,
        );
        let mut observed = [None; 3];
        while started.elapsed().as_secs_f64() * speed < 0.9 {
            let context = glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
            for (index, byte) in [0, 4, 8].into_iter().enumerate() {
                let alpha = label.attributes().map_or(u16::MAX, |attrs| {
                    attrs
                        .iterator()
                        .into_iter()
                        .flatten()
                        .filter(|attr| attr.type_() == gtk::pango::AttrType::ForegroundAlpha)
                        .find(|attr| attr.start_index() <= byte && byte < attr.end_index())
                        .map_or(u16::MAX, |attr| {
                            attr.downcast_ref::<gtk::pango::AttrInt>().unwrap().value() as u16
                        })
                });
                if alpha == u16::MAX && observed[index].is_none() {
                    observed[index] = Some(started.elapsed().as_secs_f64());
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let errors: Vec<_> = observed
            .into_iter()
            .zip([0.1, 0.4, 0.7])
            .map(|(actual, expected)| (actual.unwrap() - expected / speed) * 1000.0)
            .collect();
        assert!(
            errors.iter().all(|error| (-6.0..50.0).contains(error)),
            "GTK onset error at speed {speed}: {errors:?}"
        );
        reports.push(json!({"speed":speed,"wall_error_ms":errors}));
        motion.clear();
    }
    eprintln!(
        "LYRIC_TIMING_PROBE {}",
        json!({"reference":"controlled Enhanced LRC, GTK attributes","results":reports})
    );
    window.close();
}

#[test]
#[ignore = "Requires the isolated curl fixture: scripts/check-ui.sh gtk_lyrics_provider_cache"]
fn gtk_lyrics_provider_cache() {
    let session = session::load().unwrap();
    let fixture = session.paths[0].parent().unwrap();
    let counter = || {
        std::fs::read_to_string(std::env::var_os("OWN_PLAYER_CURL_LOG").unwrap())
            .unwrap_or_default()
            .lines()
            .count()
    };
    let make_track = |name: &str| {
        let path = fixture.join(format!("{name}.wav"));
        std::fs::write(&path, "fixture").unwrap();
        Track {
            path,
            title: name.into(),
            artist: "Fixture Artist".into(),
            album: String::new(),
            duration: 20.0,
            cover_path: None,
            lyrics: Vec::new(),
        }
    };
    let track = make_track("word-timed");
    let before = counter();
    let words = online_lyrics(&track).unwrap();
    assert_eq!(
        lyric_timing::quality(&words),
        3,
        "automatic lookup should reach the word-level provider"
    );
    assert_eq!(
        counter() - before,
        2,
        "one LRCLIB search and one LyricsPlus request"
    );
    let after = counter();
    assert_eq!(online_lyrics(&track).unwrap(), words);
    assert_eq!(
        counter(),
        after,
        "a positive cache hit must not use the network"
    );
    let missing = make_track("cached-miss");
    assert!(online_lyrics(&missing).is_err());
    let after = counter();
    assert!(online_lyrics(&missing).is_err());
    assert_eq!(
        counter(),
        after,
        "negative results survive repeated lookups"
    );
    lyrics::clear_lookup(&missing);
    assert!(online_lyrics(&missing).is_err());
    assert!(
        counter() > after,
        "explicit retry invalidates a negative cache entry"
    );
}

#[test]
#[ignore = "Run under Xvfb at GDK_SCALE=1 and GDK_SCALE=2"]
fn gtk_artwork_density() {
    gtk::init().unwrap();
    let root = std::env::temp_dir().join(format!("veyra-density-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("cover.png");
    let pixels = Pixbuf::new(Colorspace::Rgb, true, 8, 768, 768).unwrap();
    pixels.fill(0x70549fff);
    pixels.savev(&path, "png", &[]).unwrap();
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_default_size(420, 420);
    window.show_all();
    pump(40);
    let expected = if window.scale_factor() > 1 {
        752.0
    } else {
        384.0
    };
    let updates = Rc::new(UiUpdates::default());
    let ready = RefCell::new(None);
    pump_until(|| {
        if let Some(Some(image)) = artwork::get(Some(&path), true, &updates) {
            ready.replace(Some(image));
            return true;
        }
        false
    });
    let image = ready.borrow().as_ref().unwrap().clone();
    assert_eq!(image.hero.as_ref().unwrap().edge, expected);
    assert_eq!(image.thumb.width(), 40);
    let queue = artwork::get(Some(&path), false, &updates).unwrap().unwrap();
    assert!(
        Rc::ptr_eq(&image, &queue),
        "queue must reuse the ready thumbnail without another full decode"
    );
    window.close();
    std::fs::remove_dir_all(root).unwrap();
}
