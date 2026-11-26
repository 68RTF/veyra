use gtk::prelude::*;
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    time::Duration,
};

type Listener = RefCell<Box<dyn FnMut()>>;

/// Coalesce model events and cap progress redraws at roughly 15 Hz.
/// The main context has no refresh source once the last update settles.
#[derive(Default)]
pub(super) struct UiUpdates {
    listeners: RefCell<Vec<Weak<Listener>>>,
    pending: RefCell<Option<(glib::SourceId, Duration)>>,
}

impl UiUpdates {
    pub fn request(self: &Rc<Self>) {
        self.schedule(Duration::from_millis(16));
    }

    pub fn request_layout(self: &Rc<Self>) {
        self.schedule(Duration::ZERO);
    }

    pub fn request_progress(self: &Rc<Self>) {
        self.schedule(Duration::from_millis(65));
    }

    fn schedule(self: &Rc<Self>, delay: Duration) {
        if self.listeners.borrow().is_empty() {
            return;
        }
        let mut pending = self.pending.borrow_mut();
        if pending
            .as_ref()
            .is_some_and(|(_, scheduled)| delay >= *scheduled)
        {
            return;
        }
        if let Some((source, _)) = pending.take() {
            source.remove();
        }
        let updates = Rc::downgrade(self);
        let dispatch = move || {
            let Some(updates) = updates.upgrade() else {
                return;
            };
            updates.pending.take();
            let listeners = {
                let mut listeners = updates.listeners.borrow_mut();
                listeners.retain(|listener| listener.strong_count() > 0);
                listeners.clone()
            };
            for listener in listeners
                .into_iter()
                .filter_map(|listener| listener.upgrade())
            {
                listener.borrow_mut()();
            }
        };
        let source = if delay.is_zero() {
            // Rebind the scroll window before GTK allocates and paints the next frame.
            glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                dispatch();
                glib::ControlFlow::Break
            })
        } else {
            glib::timeout_add_local_once(delay, dispatch)
        };
        *pending = Some((source, delay));
    }

    pub fn listen(
        self: &Rc<Self>,
        owner: &impl IsA<gtk::Widget>,
        callback: impl FnMut() + 'static,
    ) {
        let callback: Rc<Listener> = Rc::new(RefCell::new(Box::new(callback)));
        self.listeners.borrow_mut().push(Rc::downgrade(&callback));
        let callback = RefCell::new(Some(callback));
        owner.connect_destroy(move |_| {
            callback.take();
        });
        let updates = Rc::downgrade(self);
        owner.connect_map(move |_| {
            if let Some(updates) = updates.upgrade() {
                updates.schedule(Duration::ZERO);
            }
        });
        self.request();
    }
}
