//! Native macOS launch requests. Launch Services reuses the registered app process,
//! while directory-open events create independent terminal windows in that process.

use std::cell::RefCell;
use std::ffi::{CStr, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::{ClassType, DeclaredClass, declare_class, msg_send, msg_send_id, mutability, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDidFinishLaunchingNotification,
    NSApplicationWillFinishLaunchingNotification,
};
use objc2_foundation::{
    MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSNotification,
    NSNotificationCenter, NSObject, NSObjectProtocol,
};
use winit::event_loop::EventLoopProxy;

use crate::term::UserEvent;

// Apple Event Manager constants from AERegistry.h / AppleEvents.h. The objc2
// version used by winit does not expose the corresponding Carbon typedefs.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const OPEN_DOCUMENTS_EVENT: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

struct LaunchRequests {
    fallback_directory: Option<PathBuf>,
}

impl LaunchRequests {
    fn new(directory: PathBuf) -> Self {
        Self { fallback_directory: Some(directory) }
    }

    fn open_directory(&mut self, directory: PathBuf) -> Option<PathBuf> {
        if !directory.is_dir() {
            eprintln!("Cannot open terminal directory: {}", directory.display());
            return None;
        }
        // AppKit delivers initial file-open requests before didFinishLaunching.
        // Each requested directory replaces the otherwise empty initial window.
        self.fallback_directory = None;
        Some(directory)
    }

    fn finish_launching(&mut self) -> Option<PathBuf> {
        self.fallback_directory.take()
    }
}

struct Ivars {
    proxy: EventLoopProxy<UserEvent>,
    requests: RefCell<LaunchRequests>,
}

declare_class!(
    struct LaunchObserver;

    // SAFETY: NSObject has no subclassing requirements. AppKit calls its
    // launch notifications and document-open handlers on the main thread.
    unsafe impl ClassType for LaunchObserver {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "O9TermLaunchObserver";
    }

    impl DeclaredClass for LaunchObserver {
        type Ivars = Ivars;
    }

    unsafe impl NSObjectProtocol for LaunchObserver {}

    unsafe impl LaunchObserver {
        #[method(willFinishLaunching:)]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            // Install after AppKit's default handlers and before launch events
            // are delivered. Winit's application delegate remains untouched.
            unsafe {
                let manager = NSAppleEventManager::sharedAppleEventManager();
                let _: () = msg_send![&*manager,
                    setEventHandler: self,
                    andSelector: sel!(openDocuments:withReplyEvent:),
                    forEventClass: CORE_EVENT_CLASS,
                    andEventID: OPEN_DOCUMENTS_EVENT
                ];
            }
        }

        #[method(openDocuments:withReplyEvent:)]
        fn open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            // SAFETY: paramDescriptorForKeyword: accepts an AEKeyword (UInt32)
            // and returns an autoreleased, nullable NSAppleEventDescriptor.
            let documents: Option<Retained<NSAppleEventDescriptor>> = unsafe {
                msg_send_id![event, paramDescriptorForKeyword: DIRECT_OBJECT]
            };
            let Some(documents) = documents else { return; };
            for index in 1..=unsafe { documents.numberOfItems() } {
                // SAFETY: Only local file URLs have a filesystem representation.
                // Copy its bytes before the URL or its autorelease pool is released.
                let directory = unsafe {
                    let Some(item) = documents.descriptorAtIndex(index) else { continue; };
                    let Some(url) = item.fileURLValue() else { continue; };
                    if !url.isFileURL() { continue; }
                    let path = CStr::from_ptr(url.fileSystemRepresentation().as_ptr());
                    PathBuf::from(OsStr::from_bytes(path.to_bytes()))
                };
                let directory = self.ivars().requests.borrow_mut().open_directory(directory);
                if let Some(directory) = directory {
                    let _ = self.ivars().proxy.send_event(UserEvent::NewWindow(directory));
                }
            }
        }

        #[method(didFinishLaunching:)]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let directory = self.ivars().requests.borrow_mut().finish_launching();
            if let Some(directory) = directory {
                let _ = self.ivars().proxy.send_event(UserEvent::NewWindow(directory));
            }
        }
    }
);

/// Keeps native request handlers alive for the complete event loop.
pub struct InstanceGuard {
    observer: Retained<LaunchObserver>,
}

/// Install after creating the winit event loop and before calling `run_app`.
/// Both native folder opens and the default launch become `NewWindow` events.
pub fn install(proxy: EventLoopProxy<UserEvent>, directory: PathBuf) -> InstanceGuard {
    let mtm = MainThreadMarker::new().expect("Launch observer requires the main thread");
    let this = mtm.alloc::<LaunchObserver>().set_ivars(Ivars {
        proxy,
        requests: RefCell::new(LaunchRequests::new(directory)),
    });
    // SAFETY: NSObject's init has no additional preconditions.
    let observer: Retained<LaunchObserver> = unsafe { msg_send_id![super(this), init] };
    // SAFETY: Both selectors are implemented above with NSNotification arguments.
    // The guard retains the observer until explicitly unregistering it in Drop.
    unsafe {
        let center = NSNotificationCenter::defaultCenter();
        let app = NSApplication::sharedApplication(mtm);
        center.addObserver_selector_name_object(
            &observer, sel!(willFinishLaunching:),
            Some(NSApplicationWillFinishLaunchingNotification), Some(&app),
        );
        center.addObserver_selector_name_object(
            &observer, sel!(didFinishLaunching:),
            Some(NSApplicationDidFinishLaunchingNotification), Some(&app),
        );
    }
    InstanceGuard { observer }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: These remove only the registrations installed above.
        unsafe {
            NSNotificationCenter::defaultCenter().removeObserver(&self.observer);
            let manager = NSAppleEventManager::sharedAppleEventManager();
            let _: () = msg_send![&*manager,
                removeEventHandlerForEventClass: CORE_EVENT_CLASS,
                andEventID: OPEN_DOCUMENTS_EVENT
            ];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LaunchRequests;
    use std::path::PathBuf;

    #[test]
    fn direct_launch_opens_exactly_one_default_window() {
        let directory = PathBuf::from("/direct-launch");
        let mut requests = LaunchRequests::new(directory.clone());
        assert_eq!(requests.finish_launching(), Some(directory));
        assert_eq!(requests.finish_launching(), None);
    }

    #[test]
    fn folder_launch_and_later_folder_opens_do_not_add_a_default_window() {
        let directory = std::env::temp_dir();
        let mut requests = LaunchRequests::new(PathBuf::from("/fallback"));
        assert_eq!(requests.open_directory(directory.clone()), Some(directory.clone()));
        assert_eq!(requests.finish_launching(), None);
        assert_eq!(requests.open_directory(directory.clone()), Some(directory));
        assert_eq!(requests.finish_launching(), None);
    }

    #[test]
    fn invalid_folder_request_does_not_suppress_default_window() {
        let directory = std::env::temp_dir();
        let mut requests = LaunchRequests::new(directory.clone());
        assert_eq!(requests.open_directory(PathBuf::from("/dev/null")), None);
        assert_eq!(requests.finish_launching(), Some(directory));
    }
}
