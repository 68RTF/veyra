use super::*;

pub(crate) fn add_details(
    menu: &gtk::Menu,
    model: &Rc<RefCell<Model>>,
    target: &Rc<RefCell<PathBuf>>,
) {
    for (label, action) in [
        ("Открыть папку файла", TrackAction::Reveal),
        ("Информация о треке", TrackAction::Info),
        ("Скопировать название", TrackAction::CopyTitle),
        ("Скопировать исполнителя", TrackAction::CopyArtist),
    ] {
        let item = gtk::MenuItem::with_label(label);
        menu.append(&item);
        let item_on_show = item.downgrade();
        let model_on_show = model.clone();
        let target_on_show = target.clone();
        menu.connect_map(move |_| {
            if let Some(item) = item_on_show.upgrade() {
                let state = model_on_show.borrow();
                let target = target_on_show.borrow();
                item.set_visible(
                    find_track(&state, &target).is_some_and(|track| match action {
                        TrackAction::Reveal => {
                            track.path.is_absolute() && track.path.parent().is_some()
                        }
                        TrackAction::CopyTitle => !track.title.is_empty(),
                        TrackAction::CopyArtist => !track.artist.is_empty(),
                        TrackAction::Info => true,
                    }),
                );
            }
        });
        let model = model.clone();
        let target = target.clone();
        item.connect_activate(move |_| {
            let track = find_track(&model.borrow(), &target.borrow()).cloned();
            if let Some(track) = track {
                action.activate(&track);
            }
        });
    }
}

fn find_track<'a>(state: &'a Model, target: &Path) -> Option<&'a Track> {
    state.tracks.iter().find(|track| track.path == target)
}

#[derive(Clone, Copy)]
enum TrackAction {
    Reveal,
    Info,
    CopyTitle,
    CopyArtist,
}

impl TrackAction {
    fn activate(self, track: &Track) {
        match self {
            Self::Reveal => {
                if let Some(parent) = track.path.parent().filter(|path| path.is_absolute()) {
                    let uri = gtk::gio::File::for_path(parent).uri();
                    gtk::gio::AppInfo::launch_default_for_uri_async(
                        &uri,
                        None::<&gtk::gio::AppLaunchContext>,
                        gtk::gio::Cancellable::NONE,
                        |result| {
                            if let Err(error) = result {
                                show_info("Не удалось открыть папку", &error.to_string());
                            }
                        },
                    );
                }
            }
            Self::Info => {
                let unknown = |value: &str| {
                    if value.is_empty() {
                        "—".to_owned()
                    } else {
                        value.to_owned()
                    }
                };
                let duration = if track.duration > 0.0 {
                    format_time(track.duration)
                } else {
                    "—".into()
                };
                show_info(
                    &track.title,
                    &format!(
                        "Исполнитель: {}\nАльбом: {}\nДлительность: {}\n\n{}",
                        unknown(&track.artist),
                        unknown(&track.album),
                        duration,
                        track.path.display()
                    ),
                );
            }
            Self::CopyTitle => {
                gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&track.title)
            }
            Self::CopyArtist => {
                gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&track.artist)
            }
        }
    }
}

fn show_info(title: &str, detail: &str) {
    let parent = active_window();
    let dialog = gtk::MessageDialog::new(
        parent.as_ref(),
        gtk::DialogFlags::DESTROY_WITH_PARENT | gtk::DialogFlags::MODAL,
        gtk::MessageType::Info,
        gtk::ButtonsType::Close,
        title,
    );
    dialog.set_secondary_text(Some(detail));
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.show();
}
