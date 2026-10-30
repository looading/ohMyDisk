//! Extend winit's application menu without replacing its standard commands.

use super::{Language, LanguageSender};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::AnyObject, sel,
};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

// winit's standard items use their default zero tags. These tags identify only
// the menu items owned here; existing menu positions and titles are not keys.
const LANGUAGE_MENU_TAG: isize = 0x4f4d_4401;
const ENGLISH_TAG: isize = 0x4f4d_4402;
const CHINESE_TAG: isize = 0x4f4d_4403;

struct HandlerState {
    sender: LanguageSender,
    choice: Language,
}

define_class!(
    // SAFETY: NSObject is subclassable. Every instance is main-thread-only,
    // initializes its Rust ivars before init, and adds no custom deallocation.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OhMyDiskLanguageMenuHandler"]
    #[ivars = HandlerState]
    struct LanguageMenuHandler;

    // SAFETY: NSObjectProtocol adds no invariants beyond the NSObject superclass.
    unsafe impl NSObjectProtocol for LanguageMenuHandler {}

    impl LanguageMenuHandler {
        // SAFETY: This matches AppKit's target/action ABI: void return and one
        // nullable Objective-C sender. AppKit invokes it on the main thread.
        // Sending a Copy value does not perform I/O or call back into App state.
        #[unsafe(method(chooseLanguage:))]
        fn choose_language(&self, _sender: Option<&AnyObject>) {
            self.ivars().sender.send(self.ivars().choice);
        }
    }
);

impl LanguageMenuHandler {
    fn new(sender: LanguageSender, choice: Language, mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(HandlerState { sender, choice });
        // SAFETY: The ivars are initialized. NSObject's init has this exact
        // signature; allocation and initialization remain on the main thread.
        unsafe { msg_send![super(allocated), init] }
    }
}

fn item(menu: &NSMenu, tag: isize, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    if let Some(item) = menu.itemWithTag(tag) {
        return item;
    }
    let item = NSMenuItem::new(mtm);
    item.setTag(tag);
    menu.addItem(&item);
    item
}

/// Called only from iced's window event-loop callback. Native objects remain
/// owned by AppKit; the caller receives no menu handles or retained pointers.
pub(super) fn sync(language: Language, sender: LanguageSender) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let application = NSApplication::sharedApplication(mtm);
    let Some(app_menu) = application
        .mainMenu()
        .and_then(|menu| menu.itemAtIndex(0))
        .and_then(|item| item.submenu())
    else {
        // winit creates its standard menu during application launch. A later
        // window focus/open event can retry if launch has not reached it yet.
        return false;
    };

    let language_item = if let Some(item) = app_menu.itemWithTag(LANGUAGE_MENU_TAG) {
        item
    } else {
        let item = NSMenuItem::new(mtm);
        item.setTag(LANGUAGE_MENU_TAG);
        // Keep About first and leave the standard separator/Services/Hide/Quit
        // items, selectors and key equivalents intact.
        app_menu.insertItem_atIndex(&item, app_menu.numberOfItems().min(1));
        item
    };
    let title = NSString::from_str(language.text("Language", "语言"));
    language_item.setTitle(&title);
    language_item.setEnabled(true);
    let submenu = language_item.submenu().unwrap_or_else(|| {
        let menu = NSMenu::new(mtm);
        language_item.setSubmenu(Some(&menu));
        menu
    });
    submenu.setTitle(&title);
    // Only these two app-owned choices bypass automatic responder validation.
    // The existing application menu keeps its original validation behavior.
    submenu.setAutoenablesItems(false);

    for (choice, tag) in [
        (Language::English, ENGLISH_TAG),
        (Language::Chinese, CHINESE_TAG),
    ] {
        let item = item(&submenu, tag, mtm);
        item.setTitle(&NSString::from_str(choice.label()));
        item.setEnabled(true);
        item.setState(if choice == language {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });

        // A fresh handler also replaces a previous App instance's sender when
        // the existing native items are reused. It owns no menu back-reference.
        let handler = LanguageMenuHandler::new(sender.clone(), choice, mtm);
        // SAFETY: representedObject strongly retains this NSObject subclass.
        // target is weak, so assign the strong owner before pointing target at
        // the same live object. chooseLanguage: is implemented above with the
        // exact target/action signature. No references escape to another thread.
        unsafe {
            item.setRepresentedObject(Some(&handler));
            item.setTarget(Some(&handler));
            item.setAction(Some(sel!(chooseLanguage:)));
        }
    }
    true
}
