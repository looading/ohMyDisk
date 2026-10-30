//! Native application-menu events share the GUI's existing language command.
use super::locale::Language;
use iced::{Subscription, Task};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(any(target_os = "macos", test))]
use std::{
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, MutexGuard},
};
#[cfg(any(target_os = "macos", test))]
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Command {
    pub language: Language,
    revision: u64,
}

#[cfg(any(target_os = "macos", test))]
struct Selection {
    language: Language,
    revision: u64,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone)]
pub(super) struct LanguageSender {
    sender: watch::Sender<Option<Command>>,
    selection: Arc<Mutex<Selection>>,
}

#[cfg(any(target_os = "macos", test))]
impl LanguageSender {
    pub fn send(&self, language: Language) -> Command {
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        selection.revision = selection.revision.wrapping_add(1);
        // Assign and publish together so concurrent native callbacks cannot
        // replace a newer command with an older command in the watch channel.
        let command = Command {
            language,
            revision: selection.revision,
        };
        self.sender.send_replace(Some(command));
        command
    }
}

#[derive(Clone)]
pub(super) struct Bridge {
    #[cfg(any(target_os = "macos", test))]
    shared: Arc<Shared>,
}

#[cfg(any(target_os = "macos", test))]
struct Shared {
    selection: Arc<Mutex<Selection>>,
    sender: LanguageSender,
    receiver: watch::Receiver<Option<Command>>,
}

impl Bridge {
    pub fn new(language: Language) -> Self {
        #[cfg(any(target_os = "macos", test))]
        {
            let (sender, receiver) = watch::channel(None);
            let selection = Arc::new(Mutex::new(Selection {
                language,
                revision: 0,
            }));
            Self {
                shared: Arc::new(Shared {
                    selection: selection.clone(),
                    sender: LanguageSender { sender, selection },
                    receiver,
                }),
            }
        }
        #[cfg(not(any(target_os = "macos", test)))]
        {
            let _ = language;
            Self {}
        }
    }

    pub fn select(&self, language: Language) {
        #[cfg(any(target_os = "macos", test))]
        {
            let mut selection = self.selection();
            selection.revision = selection.revision.wrapping_add(1);
            selection.language = language;
        }
        #[cfg(not(any(target_os = "macos", test)))]
        let _ = language;
    }

    #[cfg(any(target_os = "macos", test))]
    fn selection(&self) -> MutexGuard<'_, Selection> {
        self.shared
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn language(&self) -> Language {
        self.selection().language
    }

    pub fn accept(&self, command: Command) -> bool {
        #[cfg(any(target_os = "macos", test))]
        {
            let mut selection = self.selection();
            if selection.revision != command.revision {
                return false;
            }
            selection.language = command.language;
            true
        }
        #[cfg(not(any(target_os = "macos", test)))]
        {
            let _ = command;
            false
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub(super) fn sender(&self) -> LanguageSender {
        self.shared.sender.clone()
    }

    pub fn subscription(&self) -> Subscription<Command> {
        #[cfg(target_os = "macos")]
        {
            Subscription::run_with(self.clone(), events)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Subscription::none()
        }
    }

    pub fn synchronize(&self) -> Task<()> {
        #[cfg(target_os = "macos")]
        {
            let bridge = self.clone();
            iced::window::oldest().then(move |window| {
                let Some(id) = window else {
                    return Task::none();
                };
                let bridge = bridge.clone();
                iced::window::run(id, move |_| {
                    // Read current GUI state when executing on the main thread,
                    // so an earlier queued callback cannot restore old checks.
                    macos::sync(bridge.language(), bridge.sender())
                })
                .discard()
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Task::none()
        }
    }
}

#[cfg(any(target_os = "macos", test))]
impl Hash for Bridge {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Selection changes do not restart the event subscription.
        std::ptr::hash(Arc::as_ptr(&self.shared), state);
    }
}

#[cfg(any(target_os = "macos", test))]
fn events(bridge: &Bridge) -> iced::futures::stream::BoxStream<'static, Command> {
    use iced::futures::StreamExt;
    iced::futures::stream::unfold(bridge.shared.receiver.clone(), |mut receiver| async move {
        loop {
            receiver.changed().await.ok()?;
            let selected = *receiver.borrow_and_update();
            if let Some(command) = selected {
                return Some((command, receiver));
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::{FutureExt, StreamExt};

    #[tokio::test]
    async fn native_selection_wakes_the_stream_without_changing_gui_state() {
        let bridge = Bridge::new(Language::Chinese);
        let mut events = events(&bridge);
        assert!(events.next().now_or_never().is_none());
        bridge.sender().send(Language::English);
        let command = events.next().await.unwrap();
        assert_eq!(command.language, Language::English);
        assert_eq!(bridge.language(), Language::Chinese);
        assert!(bridge.accept(command));
        assert_eq!(bridge.language(), Language::English);
        assert!(events.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn delayed_native_commands_cannot_override_newer_top_bar_choices() {
        let bridge = Bridge::new(Language::Chinese);
        let mut events = events(&bridge);
        bridge.sender().send(Language::English);
        bridge.select(Language::Chinese);
        let stale = events.next().await.unwrap();
        assert!(!bridge.accept(stale));
        assert_eq!(bridge.language(), Language::Chinese);

        bridge.sender().send(Language::English);
        let queued = events.next().await.unwrap();
        bridge.select(Language::Chinese);
        assert!(!bridge.accept(queued));
        assert_eq!(bridge.language(), Language::Chinese);
    }

    #[test]
    fn subscription_identity_is_stable_across_language_changes() {
        let bridge = Bridge::new(Language::English);
        let hash = |value: &Bridge| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            value.hash(&mut hasher);
            hasher.finish()
        };
        let initial = hash(&bridge);
        bridge.select(Language::Chinese);
        assert_eq!(hash(&bridge.clone()), initial);
        assert_ne!(hash(&Bridge::new(Language::English)), initial);
    }
}
