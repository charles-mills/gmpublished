//! Native macOS document-open mechanisms.
//!
//! Two complementary handlers feed [`super::accept_paths`]:
//!
//! 1. **Delegate patch (primary)** — winit 0.30.13 (used by iced 0.14) does
//!    *not* implement `application:openURLs:` on its `NSApplicationDelegate`,
//!    so AppKit has nowhere to route document opens. We dynamically add the
//!    method to winit's delegate class via `class_addMethod`; AppKit routes
//!    opens to the delegate once the method exists. The delegate only exists
//!    after winit's event loop is
//!    created inside `run()`, so the patch is enqueued onto the main dispatch
//!    queue from `main()` and executes on the first main-run-loop turns.
//! 2. **Apple Event handler (fallback)** — a `kAEOpenDocuments` handler
//!    installed synchronously in `main()` before the delegate exists, so
//!    launch-time opens cannot be lost even if the patch loses the race with
//!    the initial `odoc` event.

#![allow(unsafe_code)]

use std::fmt;

use objc2::{
    ffi::class_addMethod,
    runtime::{AnyClass, AnyObject, ProtocolObject, Sel},
};
use objc2_app_kit::{NSApplication, NSApplicationDelegate};
use objc2_foundation::{MainThreadMarker, NSArray, NSURL};

/// Maximum main-queue turns to wait for winit's delegate to appear.
const DELEGATE_PATCH_ATTEMPTS: u32 = 50;

/// Must run on the main thread (in `main()`, before the Iced event loop
/// starts): the Apple Event handler is installed immediately, and the
/// delegate patch is scheduled onto the main dispatch queue so it runs once
/// the run loop (and therefore winit's `NSApplicationDelegate`) exists.
pub fn install() {
    match apple_event::install_document_open_handler(super::accept_paths) {
        Ok(registration) => {
            // Keeps the handler installed for the process lifetime.
            Box::leak(Box::new(registration));
            log::info!("installed macOS document-open Apple Event fallback handler");
        }
        Err(error) => {
            log::warn!("macOS document-open Apple Event fallback was not installed: {error}");
        }
    }

    schedule_delegate_patch(DELEGATE_PATCH_ATTEMPTS);
}

fn schedule_delegate_patch(attempts: u32) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        match install_delegate_open_urls_handler() {
            Ok(()) => {}
            Err(MacDocumentOpenError::MissingAppDelegate) if attempts > 0 => {
                schedule_delegate_patch(attempts - 1);
            }
            Err(error) => {
                log::warn!("macOS document-open delegate patch was not installed: {error}");
            }
        }
    });
}

fn install_delegate_open_urls_handler() -> Result<(), MacDocumentOpenError> {
    let mtm = MainThreadMarker::new().ok_or(MacDocumentOpenError::NotMainThread)?;
    let app = NSApplication::sharedApplication(mtm);
    let delegate = app
        .delegate()
        .ok_or(MacDocumentOpenError::MissingAppDelegate)?;
    let delegate_class = delegate_class(&delegate);
    let delegate_class_name = delegate_class.name().to_string_lossy().into_owned();
    let open_urls = Sel::register(c"application:openURLs:");

    if delegate_class.responds_to(open_urls) || delegate_class.instance_method(open_urls).is_some()
    {
        // A future winit may implement this itself; if so this bridge needs
        // rework to consume winit's own delivery instead of patching.
        return Err(MacDocumentOpenError::ExistingDelegateOpenUrls {
            delegate_class: delegate_class_name,
        });
    }

    // SAFETY: `delegate_class` is the live Class of the delegate object AppKit
    // handed us via `NSApplication::delegate()`, and we've just confirmed
    // above that it has no existing `application:openURLs:` method, so this
    // adds a new entry rather than racing a redefinition. The transmute is
    // sound because `application_open_urls`'s parameter list (self, cmd, two
    // object pointers) matches the "v@:@@" encoding registered here, which is
    // exactly the ABI objc2's `Imp` dispatch will call it with. `c"v@:@@"` is
    // a `'static` literal, so the pointer stays valid beyond this call.
    let added = unsafe {
        class_addMethod(
            (delegate_class as *const AnyClass).cast_mut(),
            open_urls,
            std::mem::transmute::<
                unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
                objc2::runtime::Imp,
            >(application_open_urls),
            c"v@:@@".as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err(MacDocumentOpenError::AddDelegateMethodFailed {
            delegate_class: delegate_class_name,
        });
    }

    log::info!(
        "installed macOS document-open application:openURLs: method on existing {delegate_class_name} NSApplicationDelegate"
    );
    Ok(())
}

fn delegate_class(delegate: &ProtocolObject<dyn NSApplicationDelegate>) -> &AnyClass {
    let object: &AnyObject = delegate.as_ref();
    object.class()
}

// SAFETY: this is only reachable as the Imp installed above for
// `application:openURLs:`, so AppKit calls it with `self`/`cmd` from a real
// message send and `urls` typed per the "v@:@@" encoding; `catch_unwind`
// stops a Rust panic from unwinding across the ObjC call boundary.
unsafe extern "C-unwind" fn application_open_urls(
    _delegate: *mut AnyObject,
    _cmd: Sel,
    _application: *mut AnyObject,
    urls: *mut AnyObject,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if urls.is_null() {
            log::warn!("macOS application:openURLs: callback received null URL array");
            return;
        }

        // SAFETY: `urls` was just checked non-null, and AppKit's
        // `application:openURLs:` contract guarantees this argument is a
        // valid `NSArray<NSURL> *` for the duration of the callback.
        let urls = unsafe { &*(urls.cast::<NSArray<NSURL>>()) };
        let mut paths = Vec::new();
        for index in 0..urls.count() {
            // SAFETY: `index` ranges over `0..urls.count()`, always in bounds
            // for `urls`, satisfying `objectAtIndex_unchecked`'s precondition.
            let url = unsafe { urls.objectAtIndex_unchecked(index) };
            match url.to_file_path() {
                Some(path) => paths.push(path),
                None => log::debug!("ignored macOS document-open URL without file path"),
            }
        }

        super::accept_paths(paths);
    }));

    if result.is_err() {
        log::error!("macOS application:openURLs: document-open handler panicked");
    }
}

#[derive(Debug)]
enum MacDocumentOpenError {
    NotMainThread,
    MissingAppDelegate,
    ExistingDelegateOpenUrls { delegate_class: String },
    AddDelegateMethodFailed { delegate_class: String },
}

impl fmt::Display for MacDocumentOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => {
                write!(
                    f,
                    "macOS document-open delegate patch must run on the main thread"
                )
            }
            Self::MissingAppDelegate => write!(
                f,
                "macOS document-open delegate patch found no NSApplicationDelegate"
            ),
            Self::ExistingDelegateOpenUrls { delegate_class } => write!(
                f,
                "macOS NSApplicationDelegate {delegate_class} already implements application:openURLs:"
            ),
            Self::AddDelegateMethodFailed { delegate_class } => write!(
                f,
                "failed to install application:openURLs: on macOS NSApplicationDelegate {delegate_class}"
            ),
        }
    }
}

impl std::error::Error for MacDocumentOpenError {}

mod apple_event {
    use std::{
        ffi::{c_char, c_int, c_long, c_short, c_uchar, c_uint, c_void},
        fmt, panic,
        path::PathBuf,
        ptr::{self, NonNull},
    };

    pub(super) struct MacDocumentOpenRegistration {
        state: NonNull<NativeHandlerState>,
    }

    struct NativeHandlerState {
        sender: Box<dyn Fn(Vec<PathBuf>)>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct MacDocumentOpenError {
        operation: &'static str,
        status: OSErr,
    }

    impl fmt::Display for MacDocumentOpenError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "{} failed with Apple Event status {}",
                self.operation, self.status
            )
        }
    }

    impl std::error::Error for MacDocumentOpenError {}

    pub(super) fn install_document_open_handler(
        sender: impl Fn(Vec<PathBuf>) + 'static,
    ) -> Result<MacDocumentOpenRegistration, MacDocumentOpenError> {
        let state = Box::new(NativeHandlerState {
            sender: Box::new(sender),
        });
        let state = NonNull::from(Box::leak(state));
        // SAFETY: `handle_open_documents` matches the `AEEventHandlerUPP`
        // signature Carbon expects, and `state.as_ptr()` is the pointer just
        // produced by `Box::leak` above, so it stays valid for as long as the
        // registration lives (freed on the error path below, or in Drop).
        let status = unsafe {
            AEInstallEventHandler(
                K_CORE_EVENT_CLASS,
                K_AE_OPEN_DOCUMENTS,
                Some(handle_open_documents),
                state.as_ptr().cast(),
                FALSE,
            )
        };
        if status != NO_ERR {
            // SAFETY: `state` was leaked just above and installation failed,
            // so Carbon never received this pointer and nothing else can
            // alias it; reconstructing and dropping the Box here reclaims it.
            unsafe {
                drop(Box::from_raw(state.as_ptr()));
            }
            return Err(MacDocumentOpenError {
                operation: "AEInstallEventHandler(aevt/odoc)",
                status,
            });
        }

        Ok(MacDocumentOpenRegistration { state })
    }

    impl Drop for MacDocumentOpenRegistration {
        fn drop(&mut self) {
            // SAFETY: this passes the exact class/id/handler/is_sys_handler
            // tuple used to install this handler in
            // `install_document_open_handler`, which Carbon requires to
            // identify the registration to remove; `self` owns the
            // registration, so this can't double-remove it.
            let status = unsafe {
                AERemoveEventHandler(
                    K_CORE_EVENT_CLASS,
                    K_AE_OPEN_DOCUMENTS,
                    Some(handle_open_documents),
                    FALSE,
                )
            };
            if status != NO_ERR {
                log::debug!("failed to remove macOS document-open handler: {status}");
                return;
            }
            // SAFETY: `self.state` is the same pointer produced by
            // `Box::leak` in `install_document_open_handler`, owned
            // exclusively by this registration; the handler was just
            // successfully removed above, so Carbon can no longer call it
            // with this pointer, and `Drop::drop` runs at most once.
            unsafe {
                drop(Box::from_raw(self.state.as_ptr()));
            }
        }
    }

    // SAFETY: this is only invoked by Carbon's Apple Event dispatcher as the
    // `AEEventHandlerUPP` registered above, which supplies `event`/`_reply`
    // as valid `AEDesc` pointers for the callback's duration and
    // `handler_refcon` as exactly the pointer passed to
    // `AEInstallEventHandler`; `catch_unwind` keeps a Rust panic from
    // unwinding across the C calling convention.
    unsafe extern "C" fn handle_open_documents(
        event: *const AppleEvent,
        _reply: *mut AppleEvent,
        handler_refcon: SRefCon,
    ) -> OSErr {
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            if event.is_null() || handler_refcon.is_null() {
                log::warn!("macOS document-open handler received a null Apple Event");
                return;
            }

            // SAFETY: `event` was just checked non-null and, per Carbon's
            // contract for an installed `AEEventHandlerUPP`, points to a live
            // AppleEvent for the duration of this callback.
            let paths = match unsafe { document_paths_from_event(event) } {
                Ok(paths) => paths,
                Err(status) => {
                    log::warn!("failed to parse macOS document-open Apple Event: {status}");
                    return;
                }
            };
            if paths.is_empty() {
                log::debug!("macOS document-open Apple Event contained no file URL paths");
                return;
            }

            // SAFETY: `handler_refcon` was just checked non-null and is
            // exactly the pointer produced by `Box::leak`ing a
            // `NativeHandlerState` in `install_document_open_handler`, kept
            // alive by the registration for as long as Carbon can call this
            // handler.
            let state = unsafe { &*(handler_refcon.cast::<NativeHandlerState>()) };
            (state.sender)(paths);
        }));

        if result.is_err() {
            log::error!("macOS document-open handler panicked");
        }

        NO_ERR
    }

    unsafe fn document_paths_from_event(event: *const AppleEvent) -> Result<Vec<PathBuf>, OSErr> {
        let mut list = AEDesc::default();
        // SAFETY: `event` is this function's own precondition, upheld by its
        // one caller (a live AppleEvent from the Carbon dispatcher); `list`
        // is a fresh stack-owned `AEDesc` we're writing into.
        let status = unsafe { AEGetParamDesc(event, KEY_DIRECT_OBJECT, TYPE_AE_LIST, &mut list) };
        if status != NO_ERR {
            return Err(status);
        }

        // SAFETY: `list` was just populated by the successful `AEGetParamDesc`
        // call above, so it's an initialized, live `AEDescList`.
        let result = unsafe { document_paths_from_list(&list) };
        // SAFETY: `list` was successfully filled above and not yet disposed;
        // this releases its owned resources exactly once.
        let dispose_status = unsafe { AEDisposeDesc(&mut list) };
        if dispose_status != NO_ERR {
            log::debug!("failed to dispose macOS document-open list descriptor: {dispose_status}");
        }
        result
    }

    unsafe fn document_paths_from_list(list: *const AEDescList) -> Result<Vec<PathBuf>, OSErr> {
        let mut count = 0;
        // SAFETY: `list` is this function's own precondition, upheld by its
        // caller (an `AEDescList` just filled by `AEGetParamDesc`); `count`
        // is a local we own, so writing through the pointer is sound.
        let status = unsafe { AECountItems(list, &mut count) };
        if status != NO_ERR {
            return Err(status);
        }

        let mut paths = Vec::new();
        for index in 1..=count {
            let mut item = AEDesc::default();
            // SAFETY: `index` ranges over `1..=count`, where `count` was just
            // obtained from `AECountItems` on this same `list`, satisfying
            // the 1-based in-range requirement; `item` is a fresh local we own.
            let status =
                unsafe { AEGetNthDesc(list, index, TYPE_FILE_URL, ptr::null_mut(), &mut item) };
            if status != NO_ERR {
                log::debug!("failed to read document-open item {index}: {status}");
                continue;
            }

            // SAFETY: `item` was just populated by the successful
            // `AEGetNthDesc` call above (errors `continue`d past this point).
            match unsafe { path_from_file_url_desc(&item) } {
                Some(path) => paths.push(path),
                None => log::debug!("ignored document-open item {index} with invalid file URL"),
            }
            // SAFETY: `item` was successfully filled above for this
            // iteration and is disposed exactly once before it's reused.
            let dispose_status = unsafe { AEDisposeDesc(&mut item) };
            if dispose_status != NO_ERR {
                log::debug!(
                    "failed to dispose macOS document-open item descriptor {index}: {dispose_status}"
                );
            }
        }

        Ok(paths)
    }

    unsafe fn path_from_file_url_desc(desc: *const AEDesc) -> Option<PathBuf> {
        // SAFETY: `desc` is this function's own precondition, upheld by its
        // only caller, which passes a descriptor just filled by `AEGetNthDesc`.
        let size = unsafe { AEGetDescDataSize(desc) };
        if size <= 0 {
            return None;
        }

        let mut bytes = vec![0; usize::try_from(size).ok()?];
        // SAFETY: `desc` is the same valid descriptor as above; `bytes` was
        // just allocated with exactly `size` bytes, matching the
        // `maximum_size` argument, so the write cannot overrun the buffer.
        let status = unsafe { AEGetDescData(desc, bytes.as_mut_ptr().cast(), size) };
        if status != NO_ERR {
            log::debug!("failed to read document-open file URL bytes: {status}");
            return None;
        }

        file_url_bytes_to_path(&bytes)
    }

    fn file_url_bytes_to_path(bytes: &[u8]) -> Option<PathBuf> {
        // SAFETY: `cf_file_url_bytes_to_path` only reads `bytes` through a
        // safe `&[u8]` slice to build a CFURL; it carries no additional
        // precondition beyond what the slice itself already guarantees.
        let path = unsafe { cf_file_url_bytes_to_path(bytes) };
        if path.is_some() {
            return path;
        }

        let text = std::str::from_utf8(bytes).ok()?;
        text.starts_with('/').then(|| PathBuf::from(text))
    }

    unsafe fn cf_file_url_bytes_to_path(bytes: &[u8]) -> Option<PathBuf> {
        // SAFETY: `bytes.as_ptr()` paired with the `CFIndex`-converted
        // `bytes.len()` is exactly the pointer+length CoreFoundation needs to
        // read `length` bytes from the slice; `ptr::null()` for the
        // allocator/base URL selects CF's documented defaults.
        let url = unsafe {
            CFURLCreateWithBytes(
                ptr::null(),
                bytes.as_ptr(),
                CFIndex::try_from(bytes.len()).ok()?,
                K_CF_STRING_ENCODING_UTF8,
                ptr::null(),
            )
        };
        let url = NonNull::new(url.cast_mut())?;

        // SAFETY: `url` was just produced by `CFURLCreateWithBytes` and
        // null-checked above, so it's a live `CFURLRef` we hold a "Create"
        // reference to.
        let path =
            unsafe { CFURLCopyFileSystemPath(url.as_ptr().cast(), K_CF_URL_POSIX_PATH_STYLE) };
        // SAFETY: `url` came from a "Create" function, so this call releases
        // the single reference it owns, exactly once, after its last use above.
        unsafe {
            CFRelease(url.as_ptr().cast());
        }
        let path = NonNull::new(path.cast_mut())?;

        // SAFETY: `path` was just produced by `CFURLCopyFileSystemPath` and
        // null-checked above, so it's a live `CFStringRef` valid until the
        // `CFRelease` below.
        let result = unsafe { cf_string_to_string(path.as_ptr().cast()) }.map(PathBuf::from);
        // SAFETY: `path` came from a "Copy" function (same ownership rule as
        // "Create"), so it owns exactly one reference; `cf_string_to_string`
        // has already finished reading it, so releasing here is not a
        // use-after-free.
        unsafe {
            CFRelease(path.as_ptr().cast());
        }
        result
    }

    unsafe fn cf_string_to_string(string: CFStringRef) -> Option<String> {
        // SAFETY: `string` is this function's own precondition, upheld by its
        // one caller, which passes a live `CFStringRef` it just got from
        // `CFURLCopyFileSystemPath`.
        let length = unsafe { CFStringGetLength(string) };
        if length < 0 {
            return None;
        }
        // SAFETY: this call takes only plain integer/enum arguments (no
        // pointers), so it's sound regardless of `string`'s validity.
        let max_size =
            unsafe { CFStringGetMaximumSizeForEncoding(length, K_CF_STRING_ENCODING_UTF8) };
        if max_size < 0 {
            return None;
        }

        let mut buffer = vec![0; usize::try_from(max_size).ok()?.checked_add(1)?];
        // SAFETY: `string` is the same live CFString validated by the
        // caller; `buffer` was just allocated with `max_size + 1` bytes and
        // its length is passed as `buffer_size`, so CoreFoundation cannot
        // write past its end.
        let ok = unsafe {
            CFStringGetCString(
                string,
                buffer.as_mut_ptr().cast(),
                CFIndex::try_from(buffer.len()).ok()?,
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        if ok == FALSE {
            return None;
        }

        let nul = buffer.iter().position(|byte| *byte == 0)?;
        String::from_utf8(buffer[..nul].to_vec()).ok()
    }

    type OSErr = c_short;
    type Boolean = c_uchar;
    type SRefCon = *mut c_void;
    type Size = c_long;
    type CFIndex = c_long;
    type CFStringEncoding = c_uint;
    type CFURLPathStyle = c_int;
    type CFAllocatorRef = *const c_void;
    type CFURLRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFTypeRef = *const c_void;
    type AEKeyword = c_uint;
    type DescType = c_uint;
    type AEEventClass = c_uint;
    type AEEventID = c_uint;
    type AEEventHandlerUPP =
        Option<unsafe extern "C" fn(*const AppleEvent, *mut AppleEvent, SRefCon) -> OSErr>;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct AEDesc {
        descriptor_type: DescType,
        data_handle: *mut c_void,
    }

    type AEDescList = AEDesc;
    type AppleEvent = AEDesc;

    const NO_ERR: OSErr = 0;
    const FALSE: Boolean = 0;
    const KEY_DIRECT_OBJECT: AEKeyword = fourcc(*b"----");
    const K_CORE_EVENT_CLASS: AEEventClass = fourcc(*b"aevt");
    const K_AE_OPEN_DOCUMENTS: AEEventID = fourcc(*b"odoc");
    const TYPE_AE_LIST: DescType = fourcc(*b"list");
    const TYPE_FILE_URL: DescType = fourcc(*b"furl");
    const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;
    const K_CF_URL_POSIX_PATH_STYLE: CFURLPathStyle = 0;

    const fn fourcc(bytes: [u8; 4]) -> u32 {
        u32::from_be_bytes(bytes)
    }

    // SAFETY: these signatures are transcribed from Carbon's
    // `AEDataModel.h`/`AEInteraction.h` headers (argument types, order, and
    // the C calling convention), so every call site above that passes
    // matching argument types upholds the ABI contract this block promises
    // the compiler.
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AEInstallEventHandler(
            the_ae_event_class: AEEventClass,
            the_ae_event_id: AEEventID,
            handler: AEEventHandlerUPP,
            handler_refcon: SRefCon,
            is_sys_handler: Boolean,
        ) -> OSErr;
        fn AERemoveEventHandler(
            the_ae_event_class: AEEventClass,
            the_ae_event_id: AEEventID,
            handler: AEEventHandlerUPP,
            is_sys_handler: Boolean,
        ) -> OSErr;
        fn AEGetParamDesc(
            the_apple_event: *const AppleEvent,
            the_ae_keyword: AEKeyword,
            desired_type: DescType,
            result: *mut AEDesc,
        ) -> OSErr;
        fn AECountItems(the_ae_desc_list: *const AEDescList, the_count: *mut c_long) -> OSErr;
        fn AEGetNthDesc(
            the_ae_desc_list: *const AEDescList,
            index: c_long,
            desired_type: DescType,
            the_ae_keyword: *mut AEKeyword,
            result: *mut AEDesc,
        ) -> OSErr;
        fn AEGetDescData(
            the_ae_desc: *const AEDesc,
            data_ptr: *mut c_void,
            maximum_size: Size,
        ) -> OSErr;
        fn AEGetDescDataSize(the_ae_desc: *const AEDesc) -> Size;
        fn AEDisposeDesc(the_ae_desc: *mut AEDesc) -> OSErr;
    }

    // SAFETY: these signatures are transcribed from CoreFoundation's
    // `CFURL.h`/`CFString.h`/`CFBase.h` headers, so every call site above
    // that passes matching argument types upholds the ABI contract this
    // block promises the compiler.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateWithBytes(
            allocator: CFAllocatorRef,
            url_bytes: *const u8,
            length: CFIndex,
            encoding: CFStringEncoding,
            base_url: CFURLRef,
        ) -> CFURLRef;
        fn CFURLCopyFileSystemPath(url: CFURLRef, path_style: CFURLPathStyle) -> CFStringRef;
        fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
        fn CFStringGetMaximumSizeForEncoding(
            length: CFIndex,
            encoding: CFStringEncoding,
        ) -> CFIndex;
        fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: CFIndex,
            encoding: CFStringEncoding,
        ) -> Boolean;
        fn CFRelease(cf: CFTypeRef);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn file_url_bytes_decode_percent_escaped_posix_paths() {
            let path = file_url_bytes_to_path(b"file:///tmp/gmpublished%20addon.gma")
                .expect("file URL should decode");

            assert_eq!(path, PathBuf::from("/tmp/gmpublished addon.gma"));
        }

        #[test]
        fn file_url_bytes_accept_absolute_path_fallback() {
            let path = file_url_bytes_to_path(b"/tmp/addon.gma")
                .expect("absolute path bytes should decode");

            assert_eq!(path, PathBuf::from("/tmp/addon.gma"));
        }
    }
}
