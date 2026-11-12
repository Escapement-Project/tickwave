//! `tickwave open`: the viewer, a recording and its sidecar, served on 127.0.0.1 until the process
//! is stopped, with the viewer opened in the default browser.
//!
//! The site is served at the root. The named files are served under a token drawn for this run,
//! each by its own name, and nothing else from their directories. The browser is started on a
//! launch address that works once and redirects to the viewer, so the token never shows in a
//! command line that another user on the machine could list. A request must name a loopback host
//! and the port this server bound (against DNS rebinding), and no response carries CORS headers,
//! so another site open in the browser can't read the files.
//!
//! The page may save the sidecar's hand-written file in place, by PUT to its name under the token.
//! A save must come from the server's own origin, as `application/toml`, which a cross-site form
//! can't send. It names the file it replaces by the ETag its GET gave, or `If-None-Match: *` to
//! create it, so an edit made meanwhile in a text editor isn't overwritten. The text must check
//! as `annotations check` checks it, and include the files served and no others.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Result, anyhow, bail};
use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use tower_http::services::ServeFile;
use tracing::{info, warn};

/// The viewer's site: each file's contents and media type, by its path from the site's root.
pub trait Site: Send + Sync {
    fn get(&self, path: &str) -> Option<(Cow<'static, [u8]>, String)>;
}

#[cfg(feature = "viewer")]
#[derive(rust_embed::Embed)]
#[folder = "../../web/dist/site"]
struct Embedded;

#[cfg(feature = "viewer")]
impl Site for Embedded {
    fn get(&self, path: &str) -> Option<(Cow<'static, [u8]>, String)> {
        let file = <Embedded as rust_embed::Embed>::get(path)?;
        let mime = file.metadata.mimetype().to_owned();
        Some((file.data, mime))
    }
}

/// The site this binary carries.
#[cfg(feature = "viewer")]
pub fn viewer() -> Result<Box<dyn Site>> {
    Ok(Box::new(Embedded))
}

/// The site this binary carries: none.
#[cfg(not(feature = "viewer"))]
pub fn viewer() -> Result<Box<dyn Site>> {
    bail!(
        "this tickwave was built without the viewer, so it can't open a recording: run \
         crates/wasm/build.sh and crates/convert-wasm/build.sh, then `npm run build` in web/, then \
         build the CLI with `--features viewer`"
    )
}

/// A file served under the token by `name`, read from `path`.
pub struct Named {
    pub name: String,
    pub path: PathBuf,
}

/// Checks a hand-written file's text the page saves: the problems, if there are any.
pub type Check = Box<dyn Fn(&str) -> Result<(), Vec<String>> + Send + Sync>;

/// A sidecar's hand-written file and the files it includes.
pub struct Sidecar {
    pub hand: Named,
    pub included: Vec<Named>,
    /// False when there is no file yet: the page starts one, and its first save creates it.
    pub exists: bool,
    /// None when the page may not save, as when the file has problems a save would overwrite.
    pub check: Option<Check>,
}

/// What the viewer opens: a recording, and its sidecar.
pub struct Files {
    pub recording: Named,
    pub sidecar: Option<Sidecar>,
}

/// The most the page may save, in bytes.
const MAX_SAVE: usize = 1 << 20;

/// A path segment's characters, with everything else escaped.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
/// A query value made of path segments.
const QUERY: &AsciiSet = &SEGMENT.remove(b'/');

/// 128 bits from the system's generator, in hex.
fn random() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("drawing a token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// A file's ETag: its bytes' BLAKE3, quoted.
fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", blake3::hash(bytes).to_hex())
}

/// The sidecar's hand-written file, as the page saves it.
struct Save {
    name: String,
    path: PathBuf,
    check: Check,
    /// Held from reading the file for the precondition until the new one is in place.
    lock: Mutex<()>,
}

struct Served {
    site: Box<dyn Site>,
    /// The port bound, which a request's Host must name.
    port: u16,
    /// `/<token>/`.
    files_at: String,
    files: HashMap<String, PathBuf>,
    /// The hand-written file's name, whose GET carries its ETag.
    hand: Option<String>,
    save: Option<Arc<Save>>,
    /// `/<code>`: redirects to `page` once.
    launch: String,
    launched: AtomicBool,
    /// The viewer's path and query.
    page: String,
}

impl Served {
    fn new(
        site: Box<dyn Site>,
        token: &str,
        launch: String,
        files: Files,
        port: u16,
    ) -> Result<Self> {
        let at = |name: &str| {
            let at = format!("{token}/{}", utf8_percent_encode(name, SEGMENT));
            utf8_percent_encode(&at, QUERY).to_string()
        };
        let mut page = format!("/open.html?src={}", at(&files.recording.name));
        if let Some(sidecar) = &files.sidecar {
            if sidecar.exists {
                page += &format!("&annotations={}", at(&sidecar.hand.name));
            }
            if sidecar.check.is_some() {
                page += &format!("&save={}", at(&sidecar.hand.name));
            }
        }
        page += "&local";
        let (hand, save, named) = match files.sidecar {
            Some(sidecar) => {
                let save = sidecar.check.map(|check| {
                    Arc::new(Save {
                        name: sidecar.hand.name.clone(),
                        path: sidecar.hand.path.clone(),
                        check,
                        lock: Mutex::new(()),
                    })
                });
                let hand = sidecar.hand.name.clone();
                let named = [sidecar.hand].into_iter().chain(sidecar.included);
                (Some(hand), save, named.collect())
            }
            None => (None, None, Vec::new()),
        };
        let mut by_name = HashMap::new();
        for named in [files.recording].into_iter().chain(named) {
            if by_name.contains_key(&named.name) {
                bail!(
                    "two files named {} to serve: the recording, its sidecar and the files it \
                     includes need names of their own",
                    named.name
                );
            }
            by_name.insert(named.name, named.path);
        }
        Ok(Self {
            site,
            port,
            files_at: format!("/{token}/"),
            files: by_name,
            hand,
            save,
            launch,
            launched: AtomicBool::new(false),
            page,
        })
    }
}

/// A Host or Origin authority's host and port. The port is 80 when none is given.
fn authority(text: &str) -> Option<(&str, u16)> {
    match text.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            Some((name, port.parse().ok()?))
        }
        _ => Some((text, 80)),
    }
}

fn loopback(name: &str) -> bool {
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "[::1]"
}

/// Whether a Host header names this machine's loopback interface and `port`.
fn on_port(host: &str, port: u16) -> bool {
    authority(host).is_some_and(|(name, p)| loopback(name) && p == port)
}

/// Whether an Origin header names the page this server serves to a request for `host`.
fn same_origin(origin: &str, host: &str, port: u16) -> bool {
    let Some(at) = origin.strip_prefix("http://") else {
        return false;
    };
    on_port(at, port)
        && on_port(host, port)
        && authority(at)
            .zip(authority(host))
            .is_some_and(|((a, _), (h, _))| a.eq_ignore_ascii_case(h))
}

/// Whether a Content-Type is `application/toml`, with at most a UTF-8 charset.
fn toml_type(value: &str) -> bool {
    let mut parts = value.split(';');
    let essence = parts.next().unwrap_or_default().trim();
    essence.eq_ignore_ascii_case("application/toml")
        && parts.all(|p| {
            p.split_once('=').is_some_and(|(k, v)| {
                k.trim().eq_ignore_ascii_case("charset")
                    && v.trim().trim_matches('"').eq_ignore_ascii_case("utf-8")
            })
        })
}

async fn answer(State(served): State<Arc<Served>>, req: Request) -> Response {
    let mut response = route(&served, req).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn text(status: StatusCode, why: &str) -> Response {
    (status, format!("{why}\n")).into_response()
}

async fn route(served: &Served, req: Request) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    if !host.is_some_and(|h| on_port(h, served.port)) {
        return text(
            StatusCode::FORBIDDEN,
            "tickwave open answers requests to 127.0.0.1 and localhost on its own port only",
        );
    }
    match *req.method() {
        Method::GET | Method::HEAD => {}
        Method::PUT => return put(served, req).await,
        _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
    let path = req.uri().path().to_owned();
    if let Some(name) = path.strip_prefix(&served.files_at) {
        let Some((name, file)) = percent_decode_str(name)
            .decode_utf8()
            .ok()
            .and_then(|name| served.files.get_key_value(&*name))
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if served.hand.as_ref() == Some(name) {
            return serve_hand(file.clone()).await;
        }
        return match ServeFile::new(file).try_call(req).await {
            Ok(response) => response.map(Body::new),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}\n")).into_response(),
        };
    }
    if path == served.launch {
        return if served.launched.swap(true, Ordering::Relaxed) {
            (
                StatusCode::GONE,
                "This launch address works once. The terminal that ran tickwave shows the \
                 viewer's address.\n",
            )
                .into_response()
        } else {
            (
                StatusCode::SEE_OTHER,
                [(header::LOCATION, served.page.clone())],
            )
                .into_response()
        };
    }
    let Ok(file) = percent_decode_str(&path[1..]).decode_utf8() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file = if file.is_empty() || file.ends_with('/') {
        format!("{file}index.html")
    } else {
        file.into_owned()
    };
    match served.site.get(&file) {
        Some((data, mime)) => ([(header::CONTENT_TYPE, mime)], data).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The hand-written file, whole, with the ETag a save names it by.
async fn serve_hand(path: PathBuf) -> Response {
    match tokio::task::spawn_blocking(move || fs::read(path)).await {
        Ok(Ok(bytes)) => (
            [
                (header::CONTENT_TYPE, "application/toml".to_owned()),
                (header::ETAG, etag(&bytes)),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn header_text(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// A save of the hand-written file.
async fn put(served: &Served, req: Request) -> Response {
    let headers = req.headers();
    let host = header_text(headers, header::HOST).unwrap_or_default();
    if !header_text(headers, header::ORIGIN).is_some_and(|o| same_origin(o, host, served.port)) {
        return text(
            StatusCode::FORBIDDEN,
            "a save must come from the page this server serves",
        );
    }
    let Some(name) = req.uri().path().strip_prefix(&served.files_at) else {
        return text(
            StatusCode::FORBIDDEN,
            "a save names a file under this run's token",
        );
    };
    let save = match (&served.save, percent_decode_str(name).decode_utf8()) {
        (Some(save), Ok(name)) if name == save.name => Arc::clone(save),
        _ => {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "GET, HEAD")],
                "only the sidecar's hand-written file can be saved, and only when it checks\n",
            )
                .into_response();
        }
    };
    if !header_text(headers, header::CONTENT_TYPE).is_some_and(toml_type) {
        return text(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "a save is sent as application/toml",
        );
    }
    let if_match = header_text(headers, header::IF_MATCH).map(str::to_owned);
    let if_none_match = header_text(headers, header::IF_NONE_MATCH).map(str::to_owned);
    if if_match.is_none() && if_none_match.is_none() {
        return text(
            StatusCode::PRECONDITION_REQUIRED,
            "a save names the file it replaces with If-Match and its ETag, or sends \
             If-None-Match: * to create it",
        );
    }
    let too_big = || text(StatusCode::PAYLOAD_TOO_LARGE, "a sidecar is at most 1 MiB");
    let length = header_text(headers, header::CONTENT_LENGTH).and_then(|n| n.parse::<u64>().ok());
    if length.is_some_and(|n| n > MAX_SAVE as u64) {
        return too_big();
    }
    let body = match Limited::new(req.into_body(), MAX_SAVE).collect().await {
        Ok(body) => body.to_bytes(),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => return too_big(),
        Err(e) => return text(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let Ok(new) = String::from_utf8(body.to_vec()) else {
        return text(StatusCode::BAD_REQUEST, "a sidecar's text is UTF-8");
    };
    let saved = tokio::task::spawn_blocking(move || {
        save.put(&new, if_match.as_deref(), if_none_match.as_deref())
    })
    .await;
    saved.unwrap_or_else(|e| text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))
}

impl Save {
    fn put(&self, new: &str, if_match: Option<&str>, if_none_match: Option<&str>) -> Response {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let current = match fs::read(&self.path) {
            Ok(bytes) => Some(etag(&bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        };
        // Strong comparison: no `*` and no weak tag matches, so a save always names the bytes it
        // replaces.
        let matched = if_match.is_none_or(|tags| {
            current
                .as_deref()
                .is_some_and(|now| tags.split(',').any(|t| t.trim() == now))
        });
        let absent = if_none_match.is_none_or(|v| v.trim() == "*" && current.is_none());
        if !(matched && absent) {
            return text(
                StatusCode::PRECONDITION_FAILED,
                "the sidecar on disk is not the one the page read",
            );
        }
        if let Err(problems) = (self.check)(new) {
            return (StatusCode::BAD_REQUEST, problems.join("\n") + "\n").into_response();
        }
        if let Err(e) = replace(&self.path, new) {
            warn!("saving {}: {e}", self.path.display());
            return text(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("saving the sidecar: {e}"),
            );
        }
        info!("saved {}", self.path.display());
        let status = match current {
            Some(_) => StatusCode::OK,
            None => StatusCode::CREATED,
        };
        (status, [(header::ETAG, etag(new.as_bytes()))]).into_response()
    }
}

/// The file `path` names, following symlinks, whether or not it exists.
fn link_target(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_owned();
    for _ in 0..40 {
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                let to = fs::read_link(&path)?;
                path = path.parent().unwrap_or(Path::new("")).join(to);
            }
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => return Ok(path),
        }
    }
    Err(io::Error::other(format!(
        "{}: too many symlinks",
        path.display()
    )))
}

/// Puts `text` in place of the file at `path`, whole or not at all: written and synced beside the
/// file, so a symlink at `path` stays one, with the old file's permissions, then renamed over it.
fn replace(path: &Path, text: &str) -> io::Result<()> {
    let target = link_target(path)?;
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} names no file", target.display())))?;
    let old = match fs::metadata(&target) {
        Ok(m) => Some(m.permissions()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        random().map_err(io::Error::other)?
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    // Readable by no one else until it takes the old file's permissions.
    #[cfg(unix)]
    if old.is_some() {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    }
    let mut file = options.open(&tmp)?;
    let written = (|| {
        if let Some(permissions) = old {
            file.set_permissions(permissions)?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &target)?;
        sync_dir(dir)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Makes a rename in `dir` durable.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_: &Path) -> io::Result<()> {
    Ok(())
}

fn router(served: Served) -> Router {
    Router::new().fallback(answer).with_state(Arc::new(served))
}

/// Serves `files` and the viewer on 127.0.0.1 at `port` (0 lets the system choose) until the
/// process is stopped, and opens the viewer in the default browser if `browser`.
pub fn serve(site: Box<dyn Site>, files: Files, port: u16, browser: bool) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| anyhow!("listening on 127.0.0.1:{port}: {e}"))?;
    listener.set_nonblocking(true)?;
    let bound = listener.local_addr()?;
    let origin = format!("http://{bound}");
    let launch = format!("/{}", random()?);
    let served = Served::new(site, &random()?, launch.clone(), files, bound.port())?;
    println!("{origin}{}", served.page);
    info!("serving the viewer until Ctrl-C");
    if browser {
        let url = format!("{origin}{launch}");
        std::thread::spawn(move || {
            if let Err(e) = webbrowser::open(&url) {
                warn!("couldn't start a browser ({e}): open the address above");
            }
        });
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener)?;
            axum::serve(listener, router(served)).await?;
            Ok(())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    struct Fake;

    impl Site for Fake {
        fn get(&self, path: &str) -> Option<(Cow<'static, [u8]>, String)> {
            match path {
                "index.html" => Some((Cow::Borrowed(b"index"), "text/html".into())),
                "open.html" => Some((Cow::Borrowed(b"open"), "text/html".into())),
                "docs/index.html" => Some((Cow::Borrowed(b"docs"), "text/html".into())),
                _ => None,
            }
        }
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const LAUNCH: &str = "/fedcba9876543210fedcba9876543210";
    const PORT: u16 = 4000;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> Named {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        Named {
            name: name.into(),
            path,
        }
    }

    /// A recording of 0..=255 named `rec 1.tickwave`, a sidecar the page may not save, an
    /// included file, and a file beside them that nothing names.
    fn served() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..=255).collect();
        let files = Files {
            recording: write(dir.path(), "rec 1.tickwave", &bytes),
            sidecar: Some(Sidecar {
                hand: write(dir.path(), "rec 1.annotations.toml", b"hand"),
                included: vec![write(dir.path(), "rec 1.swd.jsonl", b"generated")],
                exists: true,
                check: None,
            }),
        };
        write(dir.path(), "secret.txt", b"secret");
        let served = Served::new(Box::new(Fake), TOKEN, LAUNCH.into(), files, PORT).unwrap();
        (dir, router(served))
    }

    const ID: &str = "blake3:00ff000000000000000000000000000000000000000000000000000000000000";

    /// A sidecar that checks against `crate::tests::header()`.
    fn hand(include: &str, annotation: &str) -> String {
        format!(
            "format = \"tickwave-annotations/1\"\nrecording = \"rec 1.tickwave\"\nrecording_id = \
             \"{ID}\"\ninclude = [{include}]\n\n[[annotation]]\n{annotation}\n"
        )
    }

    fn swd() -> String {
        hand(
            r#""rec 1.swd.jsonl""#,
            "id = \"first\"\nstart = 10\nend = 20",
        )
    }

    fn generated(source: &str) -> String {
        format!(
            "{{\"format\": \"tickwave-annotations/1\", \"source\": \"{source}\", \"recording_id\": \
             \"{ID}\"}}\n{{\"id\": \"read/100\", \"start\": 100, \"end\": 200}}\n"
        )
    }

    /// A sidecar the page may save, with the real check: `rec 1.annotations.toml` holding
    /// `swd()` if `exists`, the generator file it includes, and another generator file beside
    /// them that nothing serves. The sidecar is written through `link`, a symlink, if given.
    fn writable(exists: bool) -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let recording = write(path, "rec 1.tickwave", b"not read");
        let swd_file = write(path, "rec 1.swd.jsonl", generated("swd").as_bytes());
        write(path, "rec 1.other.jsonl", generated("other").as_bytes());
        let name = "rec 1.annotations.toml";
        if exists {
            fs::write(path.join(name), swd()).unwrap();
        }
        let included = if exists { vec![swd_file] } else { Vec::new() };
        let check = crate::save_check(
            crate::tests::header(),
            recording.path.clone(),
            path.join(name),
            included.iter().map(|n| n.name.clone()).collect(),
        );
        let files = Files {
            recording,
            sidecar: Some(Sidecar {
                hand: Named {
                    name: name.into(),
                    path: path.join(name),
                },
                included,
                exists,
                check: Some(check),
            }),
        };
        let served = Served::new(Box::new(Fake), TOKEN, LAUNCH.into(), files, PORT).unwrap();
        (dir, router(served))
    }

    /// The status, the headers and the body of a request with exactly `headers`.
    async fn send(
        router: &Router,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: impl Into<Body>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let response = router
            .clone()
            .oneshot(req.body(body.into()).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
        (parts.status, parts.headers, body)
    }

    /// The status, the headers and the body of a GET for `path`, with `headers`.
    async fn get(
        router: &Router,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut all = vec![("host", "127.0.0.1:4000")];
        all.extend_from_slice(headers);
        send(router, Method::GET, path, &all, Body::empty()).await
    }

    const HAND_AT: &str = "/0123456789abcdef0123456789abcdef/rec%201.annotations.toml";

    /// What the page sends with a save, and `more`.
    fn page_headers<'a>(more: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut all = vec![
            ("host", "127.0.0.1:4000"),
            ("origin", "http://127.0.0.1:4000"),
            ("content-type", "application/toml"),
        ];
        all.extend_from_slice(more);
        all
    }

    /// The status and the body of a save of `body` to `path` with `headers`.
    async fn put(
        router: &Router,
        path: &str,
        headers: &[(&str, &str)],
        body: impl Into<Body>,
    ) -> (StatusCode, HeaderMap, String) {
        let (status, headers, body) = send(router, Method::PUT, path, headers, body).await;
        (status, headers, String::from_utf8(body).unwrap())
    }

    fn quoted_hash(text: &str) -> String {
        format!("\"{}\"", blake3::hash(text.as_bytes()).to_hex())
    }

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn the_page_names_the_files_under_the_token() {
        let (_dir, router) = served();
        run(async {
            let (status, headers, _) = get(&router, LAUNCH, &[]).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            let page = headers[header::LOCATION].to_str().unwrap();
            assert_eq!(
                page,
                format!(
                    "/open.html?src={TOKEN}/rec%25201.tickwave\
                     &annotations={TOKEN}/rec%25201.annotations.toml&local"
                )
            );
        });
    }

    #[test]
    fn the_page_may_save_a_sidecar_that_checks_or_start_one() {
        for (exists, wanted) in [
            (
                true,
                format!(
                    "/open.html?src={TOKEN}/rec%25201.tickwave\
                     &annotations={TOKEN}/rec%25201.annotations.toml\
                     &save={TOKEN}/rec%25201.annotations.toml&local"
                ),
            ),
            (
                false,
                format!(
                    "/open.html?src={TOKEN}/rec%25201.tickwave\
                     &save={TOKEN}/rec%25201.annotations.toml&local"
                ),
            ),
        ] {
            let (_dir, router) = writable(exists);
            run(async {
                let (_, headers, _) = get(&router, LAUNCH, &[]).await;
                assert_eq!(headers[header::LOCATION].to_str().unwrap(), wanted);
            });
        }
    }

    #[test]
    fn launches_once() {
        let (_dir, router) = served();
        run(async {
            assert_eq!(get(&router, LAUNCH, &[]).await.0, StatusCode::SEE_OTHER);
            assert_eq!(get(&router, LAUNCH, &[]).await.0, StatusCode::GONE);
        });
    }

    #[test]
    fn serves_ranges_of_the_named_files_only() {
        let (_dir, router) = served();
        run(async {
            let at = |name: &str| format!("/{TOKEN}/{name}");
            let (status, headers, body) =
                get(&router, &at("rec%201.tickwave"), &[("range", "bytes=4-7")]).await;
            assert_eq!(status, StatusCode::PARTIAL_CONTENT);
            assert_eq!(body, [4, 5, 6, 7]);
            assert_eq!(headers[header::CACHE_CONTROL], "no-store");
            assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
            let (status, _, body) =
                get(&router, &at("rec%201.tickwave"), &[("range", "bytes=-4")]).await;
            assert_eq!(status, StatusCode::PARTIAL_CONTENT);
            assert_eq!(body, [252, 253, 254, 255]);
            // Past the file's start: refused with its size, as GitHub Pages does, and the
            // reader then asks for the whole file.
            let (status, headers, _) = get(
                &router,
                &at("rec%201.tickwave"),
                &[("range", "bytes=-1000")],
            )
            .await;
            assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(headers[header::CONTENT_RANGE], "bytes */256");
            assert_eq!(
                get(&router, &at("rec%201.swd.jsonl"), &[]).await.2,
                b"generated"
            );
            assert_eq!(
                get(&router, &at("secret.txt"), &[]).await.0,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                get(&router, &at("..%2Fsecret.txt"), &[]).await.0,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                get(&router, "/0123/rec%201.tickwave", &[]).await.0,
                StatusCode::NOT_FOUND
            );
        });
    }

    #[test]
    fn the_sidecars_get_carries_its_etag() {
        let (_dir, router) = served();
        run(async {
            let (status, headers, body) = get(&router, HAND_AT, &[]).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, b"hand");
            // From Python's blake3 package, not this crate's.
            assert_eq!(
                headers[header::ETAG],
                "\"a31d0563e11e02681c5f0b67b4d23af73538448b7afc460cb4f92aaedac32d1e\""
            );
            assert_eq!(headers[header::ETAG], quoted_hash("hand").as_str());
            assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        });
    }

    #[test]
    fn serves_the_site_at_the_root() {
        let (_dir, router) = served();
        run(async {
            let (status, headers, body) = get(&router, "/open.html", &[]).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers[header::CONTENT_TYPE], "text/html");
            assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
            assert_eq!(body, b"open");
            assert_eq!(get(&router, "/", &[]).await.2, b"index");
            assert_eq!(get(&router, "/docs/", &[]).await.2, b"docs");
            assert_eq!(
                get(&router, "/secret.txt", &[]).await.0,
                StatusCode::NOT_FOUND
            );
        });
    }

    #[test]
    fn answers_loopback_hosts_on_its_port_only() {
        assert!(on_port("127.0.0.1:4000", PORT));
        assert!(on_port("localhost:4000", PORT));
        assert!(on_port("LocalHost:4000", PORT));
        assert!(on_port("[::1]:4000", PORT));
        assert!(on_port("[::1]", 80));
        assert!(on_port("localhost", 80));
        assert!(!on_port("[::1]", PORT));
        assert!(!on_port("localhost", PORT));
        assert!(!on_port("127.0.0.1:4001", PORT));
        assert!(!on_port("127.0.0.1:99999", PORT));
        assert!(!on_port("evil.example:4000", PORT));
        assert!(!on_port("127.0.0.1.evil.example:4000", PORT));
        assert!(!on_port("localhost.evil.example:4000", PORT));
        let (_dir, router) = writable(true);
        run(async {
            for host in [
                &[("host", "evil.example:4000")][..],
                &[("host", "127.0.0.1:4001")],
                &[],
            ] {
                let (status, ..) =
                    send(&router, Method::GET, "/open.html", host, Body::empty()).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{host:?}");
                let (status, ..) = send(&router, Method::GET, HAND_AT, host, Body::empty()).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{host:?}");
            }
            let mut wrong = page_headers(&[("if-match", "\"x\"")]);
            wrong[0] = ("host", "127.0.0.1:4001");
            wrong[1] = ("origin", "http://127.0.0.1:4001");
            assert_eq!(
                put(&router, HAND_AT, &wrong, swd()).await.0,
                StatusCode::FORBIDDEN
            );
        });
    }

    #[test]
    fn refuses_two_files_of_one_name() {
        let named = |name: &str| Named {
            name: name.into(),
            path: PathBuf::from(name),
        };
        let files = Files {
            recording: named("rec.tickwave"),
            sidecar: Some(Sidecar {
                hand: named("rec.toml"),
                included: vec![named("rec.tickwave")],
                exists: true,
                check: None,
            }),
        };
        assert!(Served::new(Box::new(Fake), TOKEN, LAUNCH.into(), files, PORT).is_err());
    }

    /// The sidecar's text, and its ETag from a GET.
    async fn current(router: &Router) -> (String, String) {
        let (status, headers, body) = get(router, HAND_AT, &[]).await;
        assert_eq!(status, StatusCode::OK);
        let etag = headers[header::ETAG].to_str().unwrap().to_owned();
        (String::from_utf8(body).unwrap(), etag)
    }

    #[test]
    fn a_save_replaces_the_sidecar_and_answers_its_new_etag() {
        let (dir, router) = writable(true);
        run(async {
            let (text, etag) = current(&router).await;
            assert_eq!(
                (text.as_str(), etag.as_str()),
                (swd().as_str(), quoted_hash(&swd()).as_str())
            );
            let new = hand(r#""rec 1.swd.jsonl""#, "id = \"renamed\"\nstart = 11");
            let (status, headers, _) = put(
                &router,
                HAND_AT,
                &page_headers(&[("if-match", &etag)]),
                new.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let saved = headers[header::ETAG].to_str().unwrap().to_owned();
            assert_ne!(saved, etag);
            assert_eq!(current(&router).await, (new.clone(), saved.clone()));
            assert_eq!(
                fs::read_to_string(dir.path().join("rec 1.annotations.toml")).unwrap(),
                new
            );
            // The page's copy is now stale; the new ETag saves, with a charset.
            let (status, ..) = put(
                &router,
                HAND_AT,
                &page_headers(&[("if-match", &etag)]),
                swd(),
            )
            .await;
            assert_eq!(status, StatusCode::PRECONDITION_FAILED);
            let with_charset = [
                ("host", "localhost:4000"),
                ("origin", "http://localhost:4000"),
                ("content-type", "application/toml; charset=UTF-8"),
                ("if-match", &saved),
            ];
            assert_eq!(
                put(&router, HAND_AT, &with_charset, swd()).await.0,
                StatusCode::OK
            );
            assert_eq!(current(&router).await.0, swd());
            // No temporary file is left beside it.
            let mut names: Vec<String> = fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            assert_eq!(
                names,
                [
                    "rec 1.annotations.toml",
                    "rec 1.other.jsonl",
                    "rec 1.swd.jsonl",
                    "rec 1.tickwave"
                ]
            );
        });
    }

    #[test]
    fn a_save_creates_a_sidecar_that_is_not_there() {
        let (dir, router) = writable(false);
        run(async {
            assert_eq!(get(&router, HAND_AT, &[]).await.0, StatusCode::NOT_FOUND);
            let new = hand("", "id = \"first\"\nstart = 10");
            let (status, ..) = put(
                &router,
                HAND_AT,
                &page_headers(&[("if-match", "\"x\"")]),
                new.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::PRECONDITION_FAILED);
            let (status, headers, _) = put(
                &router,
                HAND_AT,
                &page_headers(&[("if-none-match", "*")]),
                new.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
            let etag = headers[header::ETAG].to_str().unwrap().to_owned();
            assert_eq!(current(&router).await, (new.clone(), etag));
            assert_eq!(
                fs::read_to_string(dir.path().join("rec 1.annotations.toml")).unwrap(),
                new
            );
            let (status, ..) = put(
                &router,
                HAND_AT,
                &page_headers(&[("if-none-match", "*")]),
                new,
            )
            .await;
            assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        });
    }

    /// Asserts a save of `body` with `headers` to `path` is refused with `status`, and that the
    /// sidecar is as it was.
    async fn refused(
        router: &Router,
        path: &str,
        headers: &[(&str, &str)],
        body: impl Into<Body>,
        status: StatusCode,
    ) -> String {
        let before = current(router).await;
        let (got, _, why) = put(router, path, headers, body).await;
        assert_eq!(got, status, "{headers:?}: {why}");
        assert_eq!(current(router).await, before);
        why
    }

    #[test]
    fn a_save_from_another_origin_is_refused() {
        let (_dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            for origin in [
                None,
                Some("null"),
                Some("http://evil.example:4000"),
                Some("http://127.0.0.1:4001"),
                Some("https://127.0.0.1:4000"),
                Some("http://127.0.0.1:4000/"),
                // Loopback and the right port, but not the host the request names.
                Some("http://localhost:4000"),
            ] {
                let mut headers = page_headers(&[("if-match", &etag)]);
                headers.remove(1);
                if let Some(origin) = origin {
                    headers.push(("origin", origin));
                }
                refused(&router, HAND_AT, &headers, swd(), StatusCode::FORBIDDEN).await;
            }
        });
    }

    #[test]
    fn a_save_outside_the_token_is_refused() {
        let (_dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            let headers = page_headers(&[("if-match", &etag)]);
            for path in [
                "/00000000000000000000000000000000/rec%201.annotations.toml",
                "/rec%201.annotations.toml",
                "/open.html",
            ] {
                refused(&router, path, &headers, swd(), StatusCode::FORBIDDEN).await;
            }
        });
    }

    #[test]
    fn a_save_to_any_file_but_the_sidecar_is_refused() {
        let (dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            let headers = page_headers(&[("if-match", &etag)]);
            for name in [
                "rec%201.tickwave",
                "rec%201.swd.jsonl",
                "rec%201.other.jsonl",
                "new.toml",
            ] {
                let path = format!("/{TOKEN}/{name}");
                refused(
                    &router,
                    &path,
                    &headers,
                    swd(),
                    StatusCode::METHOD_NOT_ALLOWED,
                )
                .await;
            }
            assert_eq!(
                fs::read(dir.path().join("rec 1.tickwave")).unwrap(),
                b"not read"
            );
            assert!(!dir.path().join("new.toml").exists());
        });
        // A sidecar with problems: the page can't save over it.
        let (dir, router) = served();
        run(async {
            let etag = quoted_hash("hand");
            let headers = page_headers(&[("if-match", &etag)]);
            let (status, ..) = put(&router, HAND_AT, &headers, swd()).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(
                fs::read(dir.path().join("rec 1.annotations.toml")).unwrap(),
                b"hand"
            );
        });
    }

    #[test]
    fn a_save_is_toml_by_its_content_type() {
        let (_dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            for kind in [
                None,
                Some("text/plain"),
                Some("application/x-www-form-urlencoded"),
                Some("multipart/form-data; boundary=x"),
                Some("application/toml; charset=latin1"),
                Some("application/toml; boundary=x"),
                Some("application/tomlx"),
            ] {
                let mut headers = page_headers(&[("if-match", &etag)]);
                headers.remove(2);
                if let Some(kind) = kind {
                    headers.push(("content-type", kind));
                }
                refused(
                    &router,
                    HAND_AT,
                    &headers,
                    swd(),
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                )
                .await;
            }
        });
    }

    #[test]
    fn a_save_is_at_most_a_mebibyte() {
        let (_dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            let headers = page_headers(&[("if-match", &etag)]);
            let over = vec![b'#'; MAX_SAVE + 1];
            refused(
                &router,
                HAND_AT,
                &headers,
                over,
                StatusCode::PAYLOAD_TOO_LARGE,
            )
            .await;
            let declared = [headers.as_slice(), &[("content-length", "1048577")]].concat();
            refused(
                &router,
                HAND_AT,
                &declared,
                Body::empty(),
                StatusCode::PAYLOAD_TOO_LARGE,
            )
            .await;
            // 1 MiB exactly passes the cap and reaches the check.
            let at = vec![b'x'; 1 << 20];
            let why = refused(&router, HAND_AT, &headers, at, StatusCode::BAD_REQUEST).await;
            assert!(why.starts_with("the TOML file: "), "{why}");
            let why = refused(
                &router,
                HAND_AT,
                &headers,
                vec![0xff, 0xfe],
                StatusCode::BAD_REQUEST,
            )
            .await;
            assert_eq!(why, "a sidecar's text is UTF-8\n");
        });
    }

    #[test]
    fn a_save_needs_a_precondition_that_holds() {
        let (_dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            refused(
                &router,
                HAND_AT,
                &page_headers(&[]),
                swd(),
                StatusCode::PRECONDITION_REQUIRED,
            )
            .await;
            let weak = format!("W/{etag}");
            for precondition in [
                [("if-match", "\"0000\"")],
                [("if-match", "*")],
                [("if-match", &weak)],
                [("if-none-match", "*")],
                [("if-none-match", "\"0000\"")],
            ] {
                let headers = page_headers(&precondition);
                refused(
                    &router,
                    HAND_AT,
                    &headers,
                    swd(),
                    StatusCode::PRECONDITION_FAILED,
                )
                .await;
            }
            // Both: the file exists, so If-None-Match fails.
            let both = page_headers(&[("if-match", &etag), ("if-none-match", "*")]);
            refused(
                &router,
                HAND_AT,
                &both,
                swd(),
                StatusCode::PRECONDITION_FAILED,
            )
            .await;
            // One of a list.
            let list = format!("\"0000\", {etag}");
            let (status, ..) = put(
                &router,
                HAND_AT,
                &page_headers(&[("if-match", &list)]),
                swd(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        });
    }

    #[test]
    fn a_save_keeps_the_include_list_it_was_served_with() {
        let (_dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            let headers = page_headers(&[("if-match", &etag)]);
            let annotation = "id = \"first\"\nstart = 10";
            for include in [
                "",
                r#""rec 1.swd.jsonl", "rec 1.other.jsonl""#,
                r#""rec 1.other.jsonl""#,
            ] {
                let why = refused(
                    &router,
                    HAND_AT,
                    &headers,
                    hand(include, annotation),
                    StatusCode::BAD_REQUEST,
                )
                .await;
                assert!(
                    why.contains("but tickwave open serves [\"rec 1.swd.jsonl\"]"),
                    "{why}"
                );
            }
        });
    }

    #[test]
    fn a_save_that_does_not_check_is_refused_with_its_problems() {
        let (_dir, router) = writable(true);
        run(async {
            let etag = current(&router).await.1;
            let headers = page_headers(&[("if-match", &etag)]);
            let bad = hand(
                r#""rec 1.swd.jsonl""#,
                "id = \"first\"\nstart = 2000000\nchannels = [\"NOPE\"]",
            );
            let why = refused(&router, HAND_AT, &headers, bad, StatusCode::BAD_REQUEST).await;
            assert_eq!(
                why,
                "annotation 1: ticks 2000000 to 2000000 fall outside the capture, 0..1000000\n\
                 annotation 1: no channel \"NOPE\"\n"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_save_through_a_symlink_keeps_the_link_and_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, router) = writable(true);
        let path = dir.path();
        let real = path.join("kept");
        fs::create_dir(&real).unwrap();
        fs::rename(path.join("rec 1.annotations.toml"), real.join("real.toml")).unwrap();
        fs::set_permissions(real.join("real.toml"), fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink("kept/real.toml", path.join("rec 1.annotations.toml")).unwrap();
        run(async {
            let etag = current(&router).await.1;
            let new = hand(r#""rec 1.swd.jsonl""#, "id = \"through\"\nstart = 12");
            let (status, ..) = put(
                &router,
                HAND_AT,
                &page_headers(&[("if-match", &etag)]),
                new.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let link = path.join("rec 1.annotations.toml");
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read_link(&link).unwrap(), Path::new("kept/real.toml"));
            assert_eq!(fs::read_to_string(real.join("real.toml")).unwrap(), new);
            let mode = fs::metadata(real.join("real.toml"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o7777, 0o640);
            let left: Vec<_> = fs::read_dir(&real)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(left, ["real.toml"]);
        });
    }
}
