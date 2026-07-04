// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Mutex,
};
use tauri::{
    menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    webview::{DownloadEvent, PageLoadEvent, Webview, WebviewBuilder},
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, Window, WindowEvent,
};

/// Target height of the toolbar (tab strip + nav row) in CSS pixels.
const TOOLBAR_HEIGHT: f64 = 88.0;
const HOME_URL: &str = "https://www.google.com";

/// The toolbar's height in PHYSICAL pixels, discovered by self-calibration
/// (CSS px -> physical px is not a clean `dpr` multiple on scaled displays).
/// 0 until the first calibration completes.
static TOOLBAR_PHYS_H: AtomicU32 = AtomicU32::new(0);

/// Sequence for unique popup-window labels (window.open targets).
static POPUP_SEQ: AtomicU32 = AtomicU32::new(0);

/// Present as desktop Safari. Sites that gate on the user-agent — notably
/// Google sign-in — otherwise reject the embedded WebKit view as an insecure
/// browser, so the sign-in popup never proceeds.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4.1 Safari/605.1.15";

/// Pause and detach every audio/video element so playback stops immediately
/// when a tab is closed (before the webview is torn down).
const STOP_MEDIA_JS: &str = "document.querySelectorAll('audio,video')\
.forEach(function(m){try{m.pause();m.muted=true;m.removeAttribute('src');m.load();}catch(e){}});";

/// Title a popup sets (via the shim below) to ask the app to close it.
const POPUP_CLOSE_SENTINEL: &str = "__specter_close__";
/// Injected into popups: window.close() can't reach the OS window through wry,
/// so route it through a document.title sentinel the app watches for.
const POPUP_CLOSE_SHIM: &str = "(function(){var c=window.close.bind(window);\
window.close=function(){try{document.title='__specter_close__';}catch(e){}\
try{c();}catch(e){}};})();";

/// Custom URL scheme the context-menu script uses to hand a download URL to the
/// app (WebKit's native context-menu downloads never reach wry's handler).
const DOWNLOAD_SCHEME: &str = "x-specter-dl";

/// Injected into pages: a lightweight right-click menu for images and links
/// (only — the native menu still appears elsewhere) that can open in a new tab,
/// copy the address, and download. "Download" navigates to DOWNLOAD_SCHEME,
/// which Rust intercepts and fetches; data:/blob: use a normal <a download>.
const CONTEXT_MENU_JS: &str = r#"(function(){
  if (window.top !== window.self) return;
  function fileName(u){try{var p=new URL(u,location.href).pathname.split('/').filter(Boolean).pop();return decodeURIComponent(p||'')||'download';}catch(e){return 'download';}}
  function download(url,name){
    if(/^(data:|blob:)/i.test(url)){var a=document.createElement('a');a.href=url;a.download=name||'';document.body.appendChild(a);a.click();a.remove();return;}
    var abs;try{abs=new URL(url,location.href).href;}catch(e){abs=url;}
    location.href='x-specter-dl:?u='+encodeURIComponent(abs)+'&n='+encodeURIComponent(name||'')+'&r='+encodeURIComponent(location.href);
  }
  function copy(t){if(navigator.clipboard&&navigator.clipboard.writeText){navigator.clipboard.writeText(t).catch(function(){fb(t);});}else fb(t);}
  function fb(t){var x=document.createElement('textarea');x.value=t;x.style.cssText='position:fixed;opacity:0';document.body.appendChild(x);x.select();try{document.execCommand('copy');}catch(e){}x.remove();}
  var menu=null;
  function close(){if(menu){menu.remove();menu=null;document.removeEventListener('mousedown',od,true);window.removeEventListener('scroll',close,true);window.removeEventListener('blur',close);document.removeEventListener('keydown',ok,true);}}
  function od(e){if(menu&&!menu.contains(e.target))close();}
  function ok(e){if(e.key==='Escape')close();}
  function item(label,fn){var i=document.createElement('div');i.textContent=label;i.style.cssText='padding:7px 14px;font:13px -apple-system,system-ui,sans-serif;color:#e8eaed;cursor:default;white-space:nowrap;border-radius:5px';i.onmouseenter=function(){i.style.background='#3a6fd8';};i.onmouseleave=function(){i.style.background='';};i.addEventListener('mousedown',function(e){e.preventDefault();});i.addEventListener('click',function(e){e.preventDefault();close();fn();});menu.appendChild(i);}
  document.addEventListener('contextmenu',function(e){
    var img=e.target.closest&&e.target.closest('img');
    var link=e.target.closest&&e.target.closest('a[href]');
    if(!img&&!link)return;
    e.preventDefault();close();
    menu=document.createElement('div');
    menu.style.cssText='position:fixed;z-index:2147483647;min-width:180px;padding:5px;background:#26282d;border:1px solid #3a3d44;border-radius:9px;box-shadow:0 8px 28px rgba(0,0,0,.5)';
    if(img){var s=img.currentSrc||img.src;item('Open image in new tab',function(){window.open(s,'_blank');});item('Copy image address',function(){copy(s);});item('Download image',function(){download(s,fileName(s));});}
    if(link){if(img){var d=document.createElement('div');d.style.cssText='height:1px;margin:4px 6px;background:#3a3d44';menu.appendChild(d);}var h=link.href;item('Open link in new tab',function(){window.open(h,'_blank');});item('Copy link address',function(){copy(h);});item('Download linked file',function(){download(h,fileName(h));});}
    document.body.appendChild(menu);
    var r=menu.getBoundingClientRect(),x=Math.min(e.clientX,innerWidth-r.width-6),y=Math.min(e.clientY,innerHeight-r.height-6);
    menu.style.left=Math.max(6,x)+'px';menu.style.top=Math.max(6,y)+'px';
    document.addEventListener('mousedown',od,true);window.addEventListener('scroll',close,true);window.addEventListener('blur',close);document.addEventListener('keydown',ok,true);
  },true);
})();"#;

// Menu item ids for keyboard accelerators.
const FOCUS_ADDRESS_ID: &str = "focus_address";
const NEW_TAB_ID: &str = "new_tab";
const CLOSE_TAB_ID: &str = "close_tab";
const CLOSE_WINDOW_ID: &str = "close_window";

/// A single open tab and the main-frame URL it last loaded.
struct TabInfo {
    id: String,
    url: String,
    /// The page's document title (empty until the page reports one).
    title: String,
    /// True once the webview has committed a navigation. Until then its native
    /// `URL` is nil and reading it would panic in wry, so we don't poll it.
    loaded: bool,
    /// True while a navigation is in flight (drives the tab's loading spinner).
    loading: bool,
}

/// All open tabs and which one is active. Managed as Tauri state.
#[derive(Default)]
struct TabState {
    tabs: Vec<TabInfo>,
    active: Option<String>,
    next: u32,
}

/// Scheme for our internal "view a local file" protocol (file:// and top-level
/// data: are both restricted by WebKit, so we serve dropped files ourselves).
const LOCAL_FILE_SCHEME: &str = "specterfile";

/// MIME type for a file the webview can display, or None if it can't (in which
/// case a dropped file of this type is ignored).
fn viewable_mime(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "txt" | "text" | "log" | "md" | "markdown" => "text/plain; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        _ => return None,
    })
}

/// Build a `specterfile://localhost/?p=<path>` URL that serves the given file.
fn local_file_url(path: &std::path::Path) -> String {
    let mut url = tauri::Url::parse(&format!("{LOCAL_FILE_SCHEME}://localhost/"))
        .expect("valid base url");
    url.query_pairs_mut()
        .append_pair("p", &path.to_string_lossy());
    url.to_string()
}

/// The user's Downloads folder (falling back to home, then the temp dir).
fn downloads_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let downloads = PathBuf::from(&home).join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        return PathBuf::from(home);
    }
    std::env::temp_dir()
}

/// Avoid clobbering an existing file: `name.ext` -> `name (1).ext`, etc.
fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let dir = path.parent().map(PathBuf::from).unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into());
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1..1000 {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path
}

/// Download `url` to ~/Downloads via curl, reporting start/finish to the
/// toolbar. Used for context-menu downloads, which never reach the WKWebView
/// download delegate. `referer` helps with hotlink-protected resources.
fn start_download(app: &AppHandle, url: String, name: String, referer: String) {
    let filename = if name.trim().is_empty() {
        "download".to_string()
    } else {
        name
    };
    let path = unique_path(downloads_dir().join(filename));
    let display = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into());
    let _ = app.emit_to("toolbar", "download-started", display);

    let app = app.clone();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("curl");
        cmd.args(["-fL", "-sS", "--max-time", "600", "-A", USER_AGENT]);
        if !referer.is_empty() {
            cmd.arg("-e").arg(&referer);
        }
        let status = cmd.arg("-o").arg(&path).arg(&url).status();
        let ok = matches!(status, Ok(s) if s.success());
        if !ok {
            let _ = std::fs::remove_file(&path);
        }
        let _ = app.emit_to("toolbar", "download-finished", ok);
    });
}

/// Turn whatever the user typed in the address bar into a navigable URL.
/// - Looks like a URL  -> use as-is (adding https:// if no scheme).
/// - Looks like a host -> prepend https://.
/// - Anything else      -> treat as a search query.
fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return HOME_URL.to_string();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }
    let looks_like_host = trimmed.contains('.') && !trimmed.contains(' ');
    if looks_like_host {
        format!("https://{trimmed}")
    } else {
        // Minimal query encoding for a "basic" browser search.
        let query = trimmed.replace('%', "%25").replace(' ', "+").replace('&', "%26");
        format!("https://www.google.com/search?q={query}")
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

fn current_toolbar_h(win: &Window) -> u32 {
    let stored = TOOLBAR_PHYS_H.load(Ordering::Relaxed);
    if stored > 0 {
        stored
    } else {
        (TOOLBAR_HEIGHT * win.scale_factor().unwrap_or(1.0)).round() as u32
    }
}

/// Pin the toolbar across the top, give the active tab the rest of the window,
/// and park inactive tabs off-screen (they keep running in the background).
/// All sizes are physical pixels.
fn relayout(win: &Window) {
    let inner = match win.inner_size() {
        Ok(s) => s,
        Err(_) => return,
    };
    let toolbar_h = current_toolbar_h(win);

    if let Some(toolbar) = win.get_webview("toolbar") {
        let _ = toolbar.set_position(PhysicalPosition::new(0, 0));
        let _ = toolbar.set_size(PhysicalSize::new(inner.width, toolbar_h));
    }

    let content_h = inner.height.saturating_sub(toolbar_h).max(1);
    let state = win.state::<Mutex<TabState>>();
    let st = state.lock().unwrap();
    for tab in &st.tabs {
        if let Some(wv) = win.get_webview(&tab.id) {
            if st.active.as_deref() == Some(tab.id.as_str()) {
                let _ = wv.set_position(PhysicalPosition::new(0, toolbar_h as i32));
                let _ = wv.set_size(PhysicalSize::new(inner.width, content_h));
            } else {
                // Park just past the right edge of the window -> clipped, hidden.
                let _ = wv.set_position(PhysicalPosition::new(inner.width as i32, toolbar_h as i32));
                let _ = wv.set_size(PhysicalSize::new(inner.width.max(1), content_h));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Toolbar <-> backend events
// ---------------------------------------------------------------------------

/// Push the current tab list to the toolbar so it can render the tab strip.
fn emit_tabs(app: &AppHandle) {
    let state = app.state::<Mutex<TabState>>();
    let st = state.lock().unwrap();
    let list: Vec<serde_json::Value> = st
        .tabs
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "url": t.url,
                "title": t.title,
                "loading": t.loading,
                "active": st.active.as_deref() == Some(t.id.as_str()),
            })
        })
        .collect();
    drop(st);
    let _ = app.emit_to("toolbar", "tabs-changed", list);
}

/// Update the address bar to reflect the active tab's URL.
fn emit_active_url(app: &AppHandle) {
    let state = app.state::<Mutex<TabState>>();
    let st = state.lock().unwrap();
    let url = st
        .active
        .as_ref()
        .and_then(|active| st.tabs.iter().find(|t| &t.id == active))
        .map(|t| t.url.clone());
    drop(st);
    if let Some(url) = url {
        let _ = app.emit_to("toolbar", "url-changed", url);
    }
}

// ---------------------------------------------------------------------------
// Tab operations
// ---------------------------------------------------------------------------

/// Enable Safari-style trackpad pinch zoom on a tab. WKWebView's
/// `allowsMagnification` controls this but neither Tauri nor wry expose it, so
/// flip it natively on the underlying WKWebView.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2's msg_send! macro emits a stray cfg check
fn enable_magnification(webview: &Webview) {
    use objc::runtime::{Object, YES};
    use objc::{msg_send, sel, sel_impl};
    let _ = webview.with_webview(|platform| {
        let wk = platform.inner() as *mut Object;
        if !wk.is_null() {
            unsafe {
                let _: () = msg_send![wk, setAllowsMagnification: YES];
            }
        }
    });
}

/// What to show in the address bar / tab label for a loaded URL. A blank tab
/// (about:blank) shows nothing so the address-bar placeholder is visible.
fn display_url(url: &str) -> String {
    if url == "about:blank" {
        return String::new();
    }
    // For a local-file tab, show the file's path instead of the internal URL.
    if url.starts_with(&format!("{LOCAL_FILE_SCHEME}://")) {
        if let Ok(parsed) = tauri::Url::parse(url) {
            if let Some((_, p)) = parsed.query_pairs().find(|(k, _)| k == "p") {
                return p.into_owned();
            }
        }
        return String::new();
    }
    url.to_string()
}

/// Focus the toolbar webview and tell it to select the address bar.
fn focus_address_bar(app: &AppHandle) {
    if let Some(toolbar) = app.get_webview("toolbar") {
        let _ = toolbar.set_focus();
    }
    let _ = app.emit_to("toolbar", "focus-address", ());
}

/// Open a fresh blank tab in the foreground and focus the address bar.
fn open_blank_tab(app: &AppHandle) -> Result<(), String> {
    open_tab(app, "about:blank".to_string(), true)?;
    focus_address_bar(app);
    Ok(())
}

/// Create a new content webview and lay everything out. When `activate` is
/// true the new tab is brought to the foreground; otherwise it opens in the
/// background and the current tab stays active (e.g. cmd-click).
fn open_tab(app: &AppHandle, url: String, activate: bool) -> Result<(), String> {
    let win = app.get_window("main").ok_or("no main window")?;
    let target = if url.trim().is_empty() {
        HOME_URL.to_string()
    } else {
        url
    };
    let parsed: tauri::Url = target.parse().map_err(|_| format!("invalid url: {target}"))?;

    let id = {
        let state = app.state::<Mutex<TabState>>();
        let mut st = state.lock().unwrap();
        st.next += 1;
        let id = format!("tab-{}", st.next);
        st.tabs.push(TabInfo {
            id: id.clone(),
            url: display_url(&target),
            title: String::new(),
            loaded: false,
            // Real URLs start loading immediately; a blank tab does not.
            loading: target != "about:blank",
        });
        if activate {
            st.active = Some(id.clone());
        }
        id
    };

    let toolbar_h = current_toolbar_h(&win);
    let inner = win.inner_size().map_err(|e| e.to_string())?;

    let app_h = app.clone();
    let app_title = app.clone();
    let app_popup = app.clone();
    let app_dl = app.clone();
    let app_nav = app.clone();
    let label = id.clone();
    let label_title = id.clone();
    let builder = WebviewBuilder::new(&id, WebviewUrl::External(parsed))
        .user_agent(USER_AGENT)
        // Custom right-click menu for images/links (download / open / copy).
        .initialization_script(CONTEXT_MENU_JS)
        // Intercept the download scheme that the context-menu script navigates
        // to, and fetch the file ourselves (cancelling the navigation).
        .on_navigation(move |url| {
            if url.scheme() == DOWNLOAD_SCHEME {
                let (mut u, mut n, mut r) = (String::new(), String::new(), String::new());
                for (k, v) in url.query_pairs() {
                    match &*k {
                        "u" => u = v.into_owned(),
                        "n" => n = v.into_owned(),
                        "r" => r = v.into_owned(),
                        _ => {}
                    }
                }
                if !u.is_empty() {
                    start_download(&app_nav, u, n, r);
                }
                return false; // don't actually navigate
            }
            // A file:// navigation means a file was dropped onto the page (with
            // no picker to catch it) and WebKit is opening it. Open supported
            // types in a NEW tab via our protocol; ignore the current tab and
            // anything we can't display.
            if url.scheme() == "file" {
                if let Ok(path) = url.to_file_path() {
                    if viewable_mime(&path).is_some() {
                        let app_file = app_nav.clone();
                        let tab_url = local_file_url(&path);
                        let _ = app_nav.run_on_main_thread(move || {
                            let _ = open_tab(&app_file, tab_url, true);
                        });
                    }
                }
                return false; // never open file:// in the current tab
            }
            true
        })
        // Let the page's own HTML5 drag-and-drop handle dropped files/images
        // (e.g. upload zones) instead of Tauri intercepting the OS file drop.
        .disable_drag_drop_handler()
        // Save downloads to ~/Downloads (WKWebView won't download at all without
        // a handler). De-duplicate the filename so nothing is overwritten. Emit
        // start/finish events so the toolbar can show a downloads indicator.
        .on_download(move |_wv, event| {
            match event {
                DownloadEvent::Requested { url, destination } => {
                    let name = destination
                        .file_name()
                        .map(OsString::from)
                        .filter(|n| !n.is_empty())
                        .or_else(|| {
                            url.path_segments()
                                .and_then(|segs| segs.filter(|p| !p.is_empty()).last())
                                .map(OsString::from)
                        })
                        .unwrap_or_else(|| OsString::from("download"));
                    let path = unique_path(downloads_dir().join(name));
                    let display = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "download".into());
                    *destination = path;
                    let _ = app_dl.emit_to("toolbar", "download-started", display);
                }
                DownloadEvent::Finished { success, .. } => {
                    let _ = app_dl.emit_to("toolbar", "download-finished", success);
                }
                _ => {}
            }
            // Allow the download to proceed.
            true
        })
        .on_page_load(move |_wv, payload| {
            // on_page_load fires for the MAIN frame only (not iframes), so this
            // never picks up embedded widgets like accounts.google.com.
            let loaded = display_url(&payload.url().to_string());
            let started = matches!(payload.event(), PageLoadEvent::Started);
            {
                let state = app_h.state::<Mutex<TabState>>();
                let mut st = state.lock().unwrap();
                if let Some(t) = st.tabs.iter_mut().find(|t| t.id == label) {
                    t.url = loaded.clone();
                    t.loaded = true;
                    t.loading = started;
                    // A fresh navigation supersedes the old page's title.
                    if started {
                        t.title = String::new();
                    }
                }
            }
            emit_tabs(&app_h);
            let is_active = {
                let state = app_h.state::<Mutex<TabState>>();
                let st = state.lock().unwrap();
                st.active.as_deref() == Some(label.as_str())
            };
            if is_active {
                let _ = app_h.emit_to("toolbar", "url-changed", loaded);
            }
        })
        // Show the page's real title in its tab (fires on load and on SPA
        // document.title changes).
        .on_document_title_changed(move |_wv, title| {
            {
                let state = app_title.state::<Mutex<TabState>>();
                let mut st = state.lock().unwrap();
                if let Some(t) = st.tabs.iter_mut().find(|t| t.id == label_title) {
                    t.title = title;
                }
            }
            emit_tabs(&app_title);
        })
        // Handle window.open / target="_blank" / cmd-click.
        .on_new_window(move |url, features| {
            // No requested geometry => a plain "open in new context" (cmd-click,
            // target="_blank"). Browsers open these as a new TAB, so we do too.
            if features.size().is_none() {
                let app_tab = app_popup.clone();
                let target = url.to_string();
                // Defer to the next loop tick to avoid re-entering the event
                // loop from inside this callback. Open in the background so the
                // current tab stays focused (cmd-click behavior).
                let _ = app_popup.run_on_main_thread(move || {
                    let _ = open_tab(&app_tab, target, false);
                });
                return tauri::webview::NewWindowResponse::Deny;
            }

            // A sized window.open (e.g. "Sign in with Google" OAuth popup): open
            // a real window that SHARES the opener's webview config so
            // window.opener.postMessage — how OAuth returns its result — works.
            let seq = POPUP_SEQ.fetch_add(1, Ordering::Relaxed);
            let popup = tauri::WebviewWindowBuilder::new(
                &app_popup,
                format!("popup-{seq}"),
                WebviewUrl::External("about:blank".parse().unwrap()),
            )
            // Default size; window_features() overrides it if the opener
            // requested specific dimensions.
            .inner_size(480.0, 640.0)
            .window_features(features)
            .user_agent(USER_AGENT)
            .title(url.as_str())
            // wry doesn't bridge JS window.close() to closing the OS window, so
            // OAuth popups would linger blank after finishing. Flag the close via
            // a title sentinel (observed below) and actually close the window.
            .initialization_script(POPUP_CLOSE_SHIM)
            .on_document_title_changed(|w, title| {
                if title == POPUP_CLOSE_SENTINEL {
                    let _ = w.close();
                } else {
                    let _ = w.set_title(&title);
                }
            });
            match popup.build() {
                Ok(window) => {
                    // Keep the OAuth window capture-protected too.
                    let _ = window.set_content_protected(true);
                    tauri::webview::NewWindowResponse::Create { window }
                }
                Err(_) => tauri::webview::NewWindowResponse::Deny,
            }
        });

    // Place foreground tabs in the content area; background tabs start parked
    // off-screen so they don't flash over the current tab (relayout confirms it).
    let position = if activate {
        PhysicalPosition::new(0, toolbar_h as i32)
    } else {
        PhysicalPosition::new(inner.width as i32, toolbar_h as i32)
    };
    let webview = win
        .add_child(
            builder,
            position,
            PhysicalSize::new(inner.width, inner.height.saturating_sub(toolbar_h).max(1)),
        )
        .map_err(|e| e.to_string())?;

    // Safari-style trackpad pinch zoom.
    #[cfg(target_os = "macos")]
    enable_magnification(&webview);
    #[cfg(not(target_os = "macos"))]
    let _ = &webview;

    relayout(&win);
    emit_tabs(app);
    if activate {
        emit_active_url(app);
    }
    Ok(())
}

/// Switch to the tab at `index` in the tab strip (0-based). No-op if absent.
fn select_tab_by_index(app: &AppHandle, index: usize) {
    let id = {
        let state = app.state::<Mutex<TabState>>();
        let st = state.lock().unwrap();
        st.tabs.get(index).map(|t| t.id.clone())
    };
    if let Some(id) = id {
        let _ = switch_tab(app, &id);
    }
}

fn switch_tab(app: &AppHandle, id: &str) -> Result<(), String> {
    let win = app.get_window("main").ok_or("no main window")?;
    {
        let state = app.state::<Mutex<TabState>>();
        let mut st = state.lock().unwrap();
        if !st.tabs.iter().any(|t| t.id == id) {
            return Ok(());
        }
        st.active = Some(id.to_string());
    }
    relayout(&win);
    if let Some(wv) = win.get_webview(id) {
        let _ = wv.set_focus();
    }
    emit_tabs(app);
    emit_active_url(app);
    Ok(())
}

fn close_tab(app: &AppHandle, id: &str) -> Result<(), String> {
    let win = app.get_window("main").ok_or("no main window")?;
    if let Some(wv) = win.get_webview(id) {
        // Closing a child webview doesn't reliably deallocate the WKWebView, so
        // media would keep playing. Pause/clear media now, and navigate to
        // about:blank to tear down the page (covering Web Audio too) first.
        let _ = wv.eval(STOP_MEDIA_JS);
        if let Ok(blank) = "about:blank".parse() {
            let _ = wv.navigate(blank);
        }
        let _ = wv.close();
    }
    let became_empty = {
        let state = app.state::<Mutex<TabState>>();
        let mut st = state.lock().unwrap();
        if let Some(i) = st.tabs.iter().position(|t| t.id == id) {
            st.tabs.remove(i);
            if st.active.as_deref() == Some(id) {
                // Activate the previous tab (or the first remaining one).
                let ni = i.saturating_sub(1);
                st.active = st.tabs.get(ni).map(|t| t.id.clone());
            }
        }
        st.tabs.is_empty()
    };

    if became_empty {
        // Never leave the browser with zero tabs; open a fresh blank one.
        return open_blank_tab(app);
    }

    relayout(&win);
    let active = app.state::<Mutex<TabState>>().lock().unwrap().active.clone();
    if let Some(a) = active {
        if let Some(wv) = win.get_webview(&a) {
            let _ = wv.set_focus();
        }
    }
    emit_tabs(app);
    emit_active_url(app);
    Ok(())
}

/// The webview backing the currently active tab.
fn active_webview(app: &AppHandle) -> Result<Webview, String> {
    let win = app.get_window("main").ok_or("no main window")?;
    let id = {
        let state = app.state::<Mutex<TabState>>();
        let st = state.lock().unwrap();
        st.active.clone().ok_or("no active tab")?
    };
    win.get_webview(&id)
        .ok_or_else(|| "active webview missing".to_string())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn navigate(app: AppHandle, url: String) -> Result<(), String> {
    let target = normalize_url(&url);
    let parsed = target.parse().map_err(|_| format!("invalid url: {target}"))?;
    active_webview(&app)?.navigate(parsed).map_err(|e| e.to_string())
}

#[tauri::command]
fn go_back(app: AppHandle) -> Result<(), String> {
    active_webview(&app)?
        .eval("window.history.back()")
        .map_err(|e| e.to_string())
}

/// Open the ~/Downloads folder in Finder.
#[tauri::command]
fn open_downloads() -> Result<(), String> {
    std::process::Command::new("open")
        .arg(downloads_dir())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn go_forward(app: AppHandle) -> Result<(), String> {
    active_webview(&app)?
        .eval("window.history.forward()")
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reload(app: AppHandle) -> Result<(), String> {
    active_webview(&app)?
        .eval("window.location.reload()")
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn go_home(app: AppHandle) -> Result<(), String> {
    let parsed = HOME_URL.parse().map_err(|_| "invalid home url".to_string())?;
    active_webview(&app)?.navigate(parsed).map_err(|e| e.to_string())
}

#[tauri::command]
fn new_tab(app: AppHandle, url: Option<String>) -> Result<(), String> {
    match url {
        Some(u) if !u.trim().is_empty() => open_tab(&app, u, true),
        _ => open_blank_tab(&app),
    }
}

#[tauri::command]
fn select_tab(app: AppHandle, id: String) -> Result<(), String> {
    switch_tab(&app, &id)
}

#[tauri::command]
fn remove_tab(app: AppHandle, id: String) -> Result<(), String> {
    close_tab(&app, &id)
}

/// The toolbar calls this on load to fetch the current tab list / active URL,
/// since events emitted before it finished loading would otherwise be missed.
#[tauri::command]
fn request_tabs(app: AppHandle) {
    emit_tabs(&app);
    emit_active_url(&app);
}

/// The toolbar reports its real rendered CSS height; we measure the actual
/// physical-px-per-CSS-px ratio and resize it to hit the target exactly —
/// robust to scaled/HiDPI displays where `dpr` lies.
#[tauri::command]
fn calibrate_toolbar(app: AppHandle, measured_css_height: f64) -> Result<(), String> {
    if measured_css_height <= 1.0 {
        return Ok(());
    }
    let win = app.get_window("main").ok_or("no main window")?;
    let toolbar = win.get_webview("toolbar").ok_or("no toolbar webview")?;
    let cur = toolbar.size().map_err(|e| e.to_string())?;
    let phys_per_css = cur.height as f64 / measured_css_height;
    let target = (TOOLBAR_HEIGHT * phys_per_css).round().max(1.0) as u32;
    TOOLBAR_PHYS_H.store(target, Ordering::Relaxed);
    relayout(&win);
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Serve local files dropped onto the browser (file:// and top-level
        // data: are blocked by WebKit, so we stream the bytes ourselves).
        .register_uri_scheme_protocol(LOCAL_FILE_SCHEME, |_ctx, request| {
            use tauri::http::Response;
            let path = tauri::Url::parse(&request.uri().to_string())
                .ok()
                .and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "p")
                        .map(|(_, v)| PathBuf::from(v.into_owned()))
                });
            match path {
                Some(p) => {
                    let mime = viewable_mime(&p).unwrap_or("application/octet-stream");
                    match std::fs::read(&p) {
                        Ok(bytes) => Response::builder()
                            .status(200)
                            .header("Content-Type", mime)
                            .header("Access-Control-Allow-Origin", "*")
                            .body(bytes)
                            .unwrap(),
                        Err(_) => Response::builder().status(404).body(Vec::new()).unwrap(),
                    }
                }
                None => Response::builder().status(400).body(Vec::new()).unwrap(),
            }
        })
        .invoke_handler(tauri::generate_handler![
            navigate,
            go_back,
            go_forward,
            reload,
            go_home,
            new_tab,
            select_tab,
            remove_tab,
            request_tabs,
            open_downloads,
            calibrate_toolbar
        ])
        // App-wide keyboard accelerators (work even while a content webview has
        // keyboard focus).
        .on_menu_event(|app, event| {
            let id = event.id().0.as_str();
            if id == FOCUS_ADDRESS_ID {
                if let Some(toolbar) = app.get_webview("toolbar") {
                    let _ = toolbar.set_focus();
                }
                let _ = app.emit_to("toolbar", "focus-address", ());
            } else if id == NEW_TAB_ID {
                let _ = open_blank_tab(app);
            } else if id == CLOSE_TAB_ID {
                let active = app.state::<Mutex<TabState>>().lock().unwrap().active.clone();
                if let Some(a) = active {
                    let _ = close_tab(app, &a);
                }
            } else if id == CLOSE_WINDOW_ID {
                if let Some(win) = app.get_window("main") {
                    let _ = win.close();
                }
            } else if let Some(n) = id.strip_prefix("select_tab_") {
                // Cmd+1..Cmd+9 -> switch to the Nth tab.
                if let Ok(n) = n.parse::<usize>() {
                    select_tab_by_index(app, n - 1);
                }
            }
        })
        .setup(|app| {
            // Build the menu manually instead of Menu::default() so that nothing
            // else binds Cmd+W: our "Close Tab" owns Cmd+W, and "Close Window"
            // moves to Cmd+Shift+W. We still include the standard App/Edit/Window
            // items so Quit, copy/paste, minimize, etc. keep working.
            let app_menu = Submenu::with_items(
                app,
                "Specter",
                true,
                &[
                    &PredefinedMenuItem::about(app, None, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::show_all(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?;
            let edit_menu = Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?;
            let new_tab_item =
                MenuItem::with_id(app, NEW_TAB_ID, "New Tab", true, Some("CmdOrCtrl+T"))?;
            let close_tab_item =
                MenuItem::with_id(app, CLOSE_TAB_ID, "Close Tab", true, Some("CmdOrCtrl+W"))?;
            let focus_item = MenuItem::with_id(
                app,
                FOCUS_ADDRESS_ID,
                "Focus Address Bar",
                true,
                Some("CmdOrCtrl+L"),
            )?;
            let close_window_item = MenuItem::with_id(
                app,
                CLOSE_WINDOW_ID,
                "Close Window",
                true,
                Some("CmdOrCtrl+Shift+W"),
            )?;
            let tabs_menu = Submenu::with_items(
                app,
                "Tabs",
                true,
                &[
                    &new_tab_item,
                    &close_tab_item,
                    &focus_item,
                    &PredefinedMenuItem::separator(app)?,
                    &close_window_item,
                ],
            )?;
            // Cmd+1..Cmd+9 -> jump to the Nth tab.
            let tab_number_items: Vec<MenuItem<_>> = (1..=9)
                .map(|n| {
                    MenuItem::with_id(
                        app,
                        format!("select_tab_{n}"),
                        format!("Tab {n}"),
                        true,
                        Some(format!("CmdOrCtrl+{n}")),
                    )
                })
                .collect::<Result<_, _>>()?;
            let tab_number_refs: Vec<&dyn IsMenuItem<_>> = tab_number_items
                .iter()
                .map(|i| i as &dyn IsMenuItem<_>)
                .collect();
            let go_to_tab_menu = Submenu::with_items(app, "Go to Tab", true, &tab_number_refs)?;

            let window_menu = Submenu::with_items(
                app,
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(app, None)?,
                    &PredefinedMenuItem::fullscreen(app, None)?,
                ],
            )?;
            let menu = Menu::with_items(
                app,
                &[
                    &app_menu,
                    &edit_menu,
                    &tabs_menu,
                    &go_to_tab_menu,
                    &window_menu,
                ],
            )?;
            app.set_menu(menu)?;

            app.manage(Mutex::new(TabState::default()));

            let width = 1200.0;
            let height = 800.0;

            let window = tauri::window::WindowBuilder::new(app, "main")
                .title("Specter")
                .inner_size(width, height)
                .min_inner_size(480.0, 360.0)
                .build()?;

            // The whole point: stop other apps (and the OS) from capturing this
            // window in screenshots or screen recordings.
            //   macOS   -> NSWindowSharingType::None
            //   Windows -> WDA_EXCLUDEFROMCAPTURE
            window.set_content_protected(true)?;

            // Toolbar webview (tab strip + nav row). Child-webview bounds are in
            // PHYSICAL pixels; start with a scale_factor guess, then the toolbar
            // self-calibrates via `calibrate_toolbar`.
            let scale = window.scale_factor().unwrap_or(1.0);
            let inner = window.inner_size()?;
            let toolbar_h = (TOOLBAR_HEIGHT * scale).round() as u32;
            window.add_child(
                WebviewBuilder::new("toolbar", WebviewUrl::App("index.html".into())),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(inner.width, toolbar_h),
            )?;

            // Open the first tab(s). URLs passed on the command line each open a
            // tab (first one focused); with no args, a blank tab.
            let cli_urls: Vec<String> = std::env::args()
                .skip(1)
                .filter(|a| !a.starts_with('-')) // skip flags / macOS -psn_ arg
                .collect();
            if cli_urls.is_empty() {
                open_blank_tab(app.handle())?;
            } else {
                for (i, arg) in cli_urls.iter().enumerate() {
                    open_tab(app.handle(), normalize_url(arg), i == 0)?;
                }
            }

            // Re-lay out on resize (active tab fills, inactive parked).
            let win = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Resized(_) = event {
                    relayout(&win);
                }
            });

            // Watch the active tab's real top-level URL on a light interval.
            // on_page_load only fires for full document loads, so this is what
            // catches in-app (SPA) navigations via the History API, which update
            // the webview's URL without a page load.
            let poll = app.handle().clone();
            std::thread::spawn(move || {
                let mut last = String::new();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    // Only the active tab, and only once it has committed a
                    // navigation — reading url() before that panics inside wry.
                    let active_loaded = {
                        let state = poll.state::<Mutex<TabState>>();
                        let st = state.lock().unwrap();
                        st.active.as_ref().and_then(|a| {
                            st.tabs
                                .iter()
                                .find(|t| &t.id == a && t.loaded)
                                .map(|t| t.id.clone())
                        })
                    };
                    let current = active_loaded
                        .and_then(|id| poll.get_window("main").and_then(|w| w.get_webview(&id)))
                        .and_then(|wv| wv.url().ok())
                        .map(|u| display_url(&u.to_string()));

                    let Some(url) = current else { continue };
                    if url == last {
                        continue;
                    }
                    last = url.clone();

                    // Update the active tab's stored URL and notify the toolbar.
                    let mut changed = false;
                    {
                        let state = poll.state::<Mutex<TabState>>();
                        let mut st = state.lock().unwrap();
                        if let Some(active) = st.active.clone() {
                            if let Some(t) = st.tabs.iter_mut().find(|t| t.id == active) {
                                if t.url != url {
                                    t.url = url.clone();
                                    changed = true;
                                }
                            }
                        }
                    }
                    if changed {
                        emit_tabs(&poll);
                        let _ = poll.emit_to("toolbar", "url-changed", url);
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
