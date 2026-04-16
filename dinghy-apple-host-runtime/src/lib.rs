#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::OnceLock;

use objc2::{ClassType, MainThreadOnly, define_class};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol};

#[cfg(not(target_os = "macos"))]
use objc2_foundation::{
    NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains,
};

#[cfg(target_os = "macos")]
use objc2::{msg_send, rc::Retained, runtime::ProtocolObject};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSApplicationDelegate as DelegateProtocol};
#[cfg(not(target_os = "macos"))]
use objc2_ui_kit::{UIApplication, UIApplicationDelegate as DelegateProtocol};

static RUNNER: OnceLock<fn() -> i32> = OnceLock::new();

#[cfg(not(target_os = "macos"))]
mod ansi {
    use std::sync::Mutex;

    pub struct State {
        foreground: u8,
        bold: bool,
    }

    impl State {
        const fn new() -> Self {
            Self {
                foreground: 0,
                bold: false,
            }
        }

        fn rgb(&self) -> (f64, f64, f64) {
            let code = if self.bold && (30..=37).contains(&self.foreground) {
                self.foreground + 60
            } else {
                self.foreground
            };
            match code {
                0 if self.bold => (0.33, 1.0, 0.33),
                0 => (0.0, 1.0, 0.0),
                30 => (0.25, 0.25, 0.25),
                31 => (0.8, 0.0, 0.0),
                32 => (0.0, 0.8, 0.0),
                33 => (0.8, 0.8, 0.0),
                34 => (0.4, 0.4, 1.0),
                35 => (0.8, 0.0, 0.8),
                36 => (0.0, 0.8, 0.8),
                37 => (0.75, 0.75, 0.75),
                90 => (0.5, 0.5, 0.5),
                91 => (1.0, 0.33, 0.33),
                92 => (0.33, 1.0, 0.33),
                93 => (1.0, 1.0, 0.33),
                94 => (0.33, 0.33, 1.0),
                95 => (1.0, 0.33, 1.0),
                96 => (0.33, 1.0, 1.0),
                97 => (1.0, 1.0, 1.0),
                _ => (0.0, 1.0, 0.0),
            }
        }
    }

    pub static STATE: Mutex<State> = Mutex::new(State::new());

    pub struct Segment {
        pub text: String,
        pub rgb: (f64, f64, f64),
    }

    pub fn parse(input: &str) -> Vec<Segment> {
        let mut state = STATE.lock().unwrap();
        let mut segments = Vec::new();
        let mut current_text = String::new();
        let bytes = input.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                if !current_text.is_empty() {
                    segments.push(Segment {
                        text: std::mem::take(&mut current_text),
                        rgb: state.rgb(),
                    });
                }
                i += 2;
                let start = i;
                while i < bytes.len() && !(bytes[i] as char).is_ascii_alphabetic() {
                    i += 1;
                }
                if i < bytes.len() {
                    if bytes[i] == b'm' {
                        let params = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
                        if params.is_empty() {
                            state.foreground = 0;
                            state.bold = false;
                        } else {
                            for param in params.split(';') {
                                match param.parse::<u8>() {
                                    Ok(0) => {
                                        state.foreground = 0;
                                        state.bold = false;
                                    }
                                    Ok(1) => state.bold = true,
                                    Ok(22) => state.bold = false,
                                    Ok(n @ 30..=37) => state.foreground = n,
                                    Ok(39) => state.foreground = 0,
                                    Ok(n @ 90..=97) => state.foreground = n,
                                    _ => {}
                                }
                            }
                        }
                    }
                    i += 1;
                }
            } else if let Some(ch) = std::str::from_utf8(&bytes[i..])
                .ok()
                .and_then(|s| s.chars().next())
            {
                current_text.push(ch);
                i += ch.len_utf8();
            } else {
                i += 1;
            }
        }

        if !current_text.is_empty() {
            segments.push(Segment {
                text: current_text,
                rgb: state.rgb(),
            });
        }

        segments
    }
}

#[cfg(not(target_os = "macos"))]
mod ios_ui {
    use std::sync::OnceLock;

    use objc2::rc::Retained;
    use objc2::MainThreadOnly;
    use objc2_foundation::{MainThreadMarker, NSObject, NSRange, NSString};
    use objc2_ui_kit::{UIColor, UIFont, UIScreen, UITextView, UIViewController, UIWindow};

    struct UiHandles {
        text_view: usize,
        font: usize,
    }

    static HANDLES: OnceLock<UiHandles> = OnceLock::new();

    #[allow(deprecated)]
    pub fn setup_window_and_text_view(mtm: MainThreadMarker) {
        let screen_bounds = UIScreen::mainScreen(mtm).bounds();

        let window = UIWindow::initWithFrame(UIWindow::alloc(mtm), screen_bounds);

        let view_controller = UIViewController::new(mtm);
        window.setRootViewController(Some(&view_controller));

        let vc_view: Retained<NSObject> =
            unsafe { objc2::msg_send![&view_controller, view] };
        let black = UIColor::blackColor();
        let _: () = unsafe { objc2::msg_send![&*vc_view, setBackgroundColor: &*black] };

        let zero = objc2_core_foundation::CGRect::ZERO;
        let text_view = UITextView::initWithFrame(UITextView::alloc(mtm), zero);
        text_view.setEditable(false);
        text_view.setBackgroundColor(Some(&UIColor::blackColor()));
        let font = UIFont::monospacedSystemFontOfSize_weight(
            12.0,
            unsafe { objc2_ui_kit::UIFontWeightRegular },
        );
        text_view.setFont(Some(&font));

        let _: () = unsafe {
            objc2::msg_send![&text_view, setTranslatesAutoresizingMaskIntoConstraints: false]
        };
        let _: () = unsafe { objc2::msg_send![&*vc_view, addSubview: &*text_view] };

        let guide: Retained<NSObject> =
            unsafe { objc2::msg_send![&*vc_view, safeAreaLayoutGuide] };

        let guide_top: Retained<NSObject> =
            unsafe { objc2::msg_send![&*guide, topAnchor] };
        let guide_bottom: Retained<NSObject> =
            unsafe { objc2::msg_send![&*guide, bottomAnchor] };
        let guide_leading: Retained<NSObject> =
            unsafe { objc2::msg_send![&*guide, leadingAnchor] };
        let guide_trailing: Retained<NSObject> =
            unsafe { objc2::msg_send![&*guide, trailingAnchor] };

        let tv_top: Retained<NSObject> =
            unsafe { objc2::msg_send![&text_view, topAnchor] };
        let tv_bottom: Retained<NSObject> =
            unsafe { objc2::msg_send![&text_view, bottomAnchor] };
        let tv_leading: Retained<NSObject> =
            unsafe { objc2::msg_send![&text_view, leadingAnchor] };
        let tv_trailing: Retained<NSObject> =
            unsafe { objc2::msg_send![&text_view, trailingAnchor] };

        let c_top: Retained<NSObject> =
            unsafe { objc2::msg_send![&*tv_top, constraintEqualToAnchor: &*guide_top] };
        let c_bottom: Retained<NSObject> =
            unsafe { objc2::msg_send![&*tv_bottom, constraintEqualToAnchor: &*guide_bottom] };
        let c_leading: Retained<NSObject> =
            unsafe { objc2::msg_send![&*tv_leading, constraintEqualToAnchor: &*guide_leading] };
        let c_trailing: Retained<NSObject> = unsafe {
            objc2::msg_send![&*tv_trailing, constraintEqualToAnchor: &*guide_trailing]
        };

        let _: () = unsafe { objc2::msg_send![&*c_top, setActive: true] };
        let _: () = unsafe { objc2::msg_send![&*c_bottom, setActive: true] };
        let _: () = unsafe { objc2::msg_send![&*c_leading, setActive: true] };
        let _: () = unsafe { objc2::msg_send![&*c_trailing, setActive: true] };

        window.makeKeyAndVisible();

        let tv_ptr = Retained::as_ptr(&text_view) as usize;
        let font_ptr = Retained::as_ptr(&font) as usize;
        let _ = HANDLES.set(UiHandles {
            text_view: tv_ptr,
            font: font_ptr,
        });

        std::mem::forget(window);
        std::mem::forget(view_controller);
        std::mem::forget(text_view);
        std::mem::forget(font);
    }

    pub fn append_text(text: &str) {
        let handles = match HANDLES.get() {
            Some(h) if h.text_view != 0 => h,
            _ => return,
        };

        let text_view = unsafe { &*(handles.text_view as *const UITextView) };
        let font = unsafe { &*(handles.font as *const UIFont) };

        let segments = super::ansi::parse(text);
        if segments.is_empty() {
            return;
        }

        let font_key = NSString::from_str("NSFont");
        let color_key = NSString::from_str("NSColor");

        let storage: Retained<NSObject> =
            unsafe { objc2::msg_send![text_view, textStorage] };
        let _: () = unsafe { objc2::msg_send![&*storage, beginEditing] };

        for segment in &segments {
            let old_len: usize = unsafe { objc2::msg_send![&*storage, length] };

            let ns_text = NSString::from_str(&segment.text);
            let insert_range = NSRange::new(old_len, 0);
            let _: () = unsafe {
                objc2::msg_send![
                    &*storage,
                    replaceCharactersInRange: insert_range,
                    withString: &*ns_text
                ]
            };

            let new_len: usize = unsafe { objc2::msg_send![&*storage, length] };
            let inserted_len = new_len - old_len;
            if inserted_len == 0 {
                continue;
            }
            let attr_range = NSRange::new(old_len, inserted_len);

            let (r, g, b) = segment.rgb;
            let color: Retained<NSObject> = unsafe {
                objc2::msg_send![
                    objc2::class!(UIColor),
                    colorWithRed: r,
                    green: g,
                    blue: b,
                    alpha: 1.0_f64
                ]
            };

            let _: () = unsafe {
                objc2::msg_send![
                    &*storage,
                    addAttribute: &*color_key,
                    value: &*color,
                    range: attr_range
                ]
            };
            let _: () = unsafe {
                objc2::msg_send![
                    &*storage,
                    addAttribute: &*font_key,
                    value: font,
                    range: attr_range
                ]
            };
        }

        let _: () = unsafe { objc2::msg_send![&*storage, endEditing] };

        let total_len: usize = unsafe { objc2::msg_send![&*storage, length] };
        if total_len > 0 {
            let range = NSRange::new(total_len.saturating_sub(1), 1);
            let _: () =
                unsafe { objc2::msg_send![text_view, scrollRangeToVisible: range] };
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn setup_stdout_mirror() {
    use std::os::unix::io::RawFd;

    let original_stdout: RawFd = unsafe { libc::dup(1) };
    if original_stdout < 0 {
        return;
    }

    let mut pipe_fds: [libc::c_int; 2] = [0; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        unsafe { libc::close(original_stdout) };
        return;
    }

    let read_end = pipe_fds[0];
    let write_end = pipe_fds[1];

    unsafe {
        libc::dup2(write_end, 1);
        libc::dup2(write_end, 2);
        libc::close(write_end);
    }

    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            let bytes_read = unsafe {
                libc::read(
                    read_end,
                    buffer.as_mut_ptr() as *mut libc::c_void,
                    buffer.len(),
                )
            };
            if bytes_read <= 0 {
                break;
            }
            let chunk = &buffer[..bytes_read as usize];

            unsafe {
                libc::write(
                    original_stdout,
                    chunk.as_ptr() as *const libc::c_void,
                    chunk.len(),
                );
            }

            if let Ok(text) = std::str::from_utf8(chunk) {
                let text = text.to_string();
                dispatch_to_main(move || {
                    ios_ui::append_text(&text);
                });
            }
        }
        unsafe { libc::close(read_end) };
        unsafe { libc::close(original_stdout) };
    });
}

#[cfg(not(target_os = "macos"))]
fn dispatch_to_main<F: FnOnce() + Send + 'static>(f: F) {
    dispatch2::DispatchQueue::main().exec_async(f);
}

fn run_and_exit() {
    #[cfg(not(target_os = "macos"))]
    {
        let paths = NSSearchPathForDirectoriesInDomains(
            NSSearchPathDirectory(9),
            NSSearchPathDomainMask(1),
            true,
        );
        if let Some(documents_dir) = paths.firstObject() {
            let _ = std::env::set_current_dir(documents_dir.to_string());
        }
    }

    let run = *RUNNER
        .get()
        .expect("runner must be initialized before app launch");
    let exit_code = run();
    std::process::exit(exit_code);
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl DelegateProtocol for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            #[cfg(not(target_os = "macos"))]
            {
                let mtm = objc2_foundation::MainThreadMarker::new()
                    .expect("did_finish_launching must be on main thread");
                ios_ui::setup_window_and_text_view(mtm);
                setup_stdout_mirror();
            }
            std::thread::spawn(run_and_exit);
        }
    }
);

#[cfg(target_os = "macos")]
pub fn run_application(run: fn() -> i32) -> ! {
    RUNNER
        .set(run)
        .expect("run_application must only be called once");

    let mtm = MainThreadMarker::new().expect("host app must start on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    let delegate: Retained<AppDelegate> = unsafe { msg_send![AppDelegate::class(), new] };
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    application.run();
    unreachable!("NSApplication::run should not return");
}

#[cfg(not(target_os = "macos"))]
pub fn run_application(run: fn() -> i32) -> ! {
    RUNNER
        .set(run)
        .expect("run_application must only be called once");

    let mtm = MainThreadMarker::new().expect("host app must start on the main thread");
    let delegate_class = objc2_foundation::NSString::from_class(AppDelegate::class());
    UIApplication::main(None, Some(&delegate_class), mtm);
}
