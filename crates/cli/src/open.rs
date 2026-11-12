//! `tickwave open`: the viewer, a recording and its sidecar, served on 127.0.0.1 until the process
//! is stopped, with the viewer opened in the default browser.
//!
//! The site is served at the root. The named files are served under a token drawn for this run,
//! each by its own name, and nothing else from their directories. The browser is started on a
//! launch address that works once and redirects to the viewer, so the token never shows in a
//! command line that another user on the machine could list. A request must name a loopback host
//! (against DNS rebinding), and no response carries CORS headers, so another site open in the
//! browser can't read the files.

use std::borrow::Cow;
use std::collections::HashMap;
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow, bail};
use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
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

/// What the viewer opens: a recording, and its sidecar's hand-written file with the files it
/// includes.
pub struct Files {
    pub recording: Named,
    pub sidecar: Option<(Named, Vec<Named>)>,
}

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

struct Served {
    site: Box<dyn Site>,
    /// `/<token>/`.
    files_at: String,
    files: HashMap<String, PathBuf>,
    /// `/<code>`: redirects to `page` once.
    launch: String,
    launched: AtomicBool,
    /// The viewer's path and query.
    page: String,
}

impl Served {
    fn new(site: Box<dyn Site>, token: &str, launch: String, files: Files) -> Result<Self> {
        let at = |name: &str| format!("{token}/{}", utf8_percent_encode(name, SEGMENT));
        let mut page = format!(
            "/open.html?src={}",
            utf8_percent_encode(&at(&files.recording.name), QUERY)
        );
        if let Some((hand, _)) = &files.sidecar {
            page += &format!(
                "&annotations={}",
                utf8_percent_encode(&at(&hand.name), QUERY)
            );
        }
        page += "&local";
        let mut by_name = HashMap::new();
        let (hand, included) = files
            .sidecar
            .map_or((None, Vec::new()), |(h, i)| (Some(h), i));
        for named in [files.recording].into_iter().chain(hand).chain(included) {
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
            files_at: format!("/{token}/"),
            files: by_name,
            launch,
            launched: AtomicBool::new(false),
            page,
        })
    }
}

/// Whether a Host header names this machine's loopback interface.
fn loopback(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "[::1]"
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

async fn route(served: &Served, req: Request) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    if !host.is_some_and(loopback) {
        return (
            StatusCode::FORBIDDEN,
            "tickwave open answers requests to 127.0.0.1 and localhost only\n",
        )
            .into_response();
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = req.uri().path().to_owned();
    if let Some(name) = path.strip_prefix(&served.files_at) {
        let Some(file) = percent_decode_str(name)
            .decode_utf8()
            .ok()
            .and_then(|name| served.files.get(&*name))
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
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

fn router(served: Served) -> Router {
    Router::new().fallback(answer).with_state(Arc::new(served))
}

/// Serves `files` and the viewer on 127.0.0.1 at `port` (0 lets the system choose) until the
/// process is stopped, and opens the viewer in the default browser if `browser`.
pub fn serve(site: Box<dyn Site>, files: Files, port: u16, browser: bool) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| anyhow!("listening on 127.0.0.1:{port}: {e}"))?;
    listener.set_nonblocking(true)?;
    let origin = format!("http://{}", listener.local_addr()?);
    let launch = format!("/{}", random()?);
    let served = Served::new(site, &random()?, launch.clone(), files)?;
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

    /// A recording of 0..=255 named `rec 1.tickwave`, a sidecar, an included file, and a file
    /// beside them that nothing names.
    fn served() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            Named {
                name: name.into(),
                path,
            }
        };
        let bytes: Vec<u8> = (0..=255).collect();
        let files = Files {
            recording: write("rec 1.tickwave", &bytes),
            sidecar: Some((
                write("rec 1.annotations.toml", b"hand"),
                vec![write("rec 1.swd.jsonl", b"generated")],
            )),
        };
        write("secret.txt", b"secret");
        let served = Served::new(Box::new(Fake), TOKEN, LAUNCH.into(), files).unwrap();
        (dir, router(served))
    }

    /// The status, the headers and the body of a GET for `path`, with `headers`.
    async fn get(
        router: &Router,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut req = Request::get(path).header("host", "127.0.0.1:4000");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let response = router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
        (parts.status, parts.headers, body)
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
    fn answers_loopback_hosts_only() {
        assert!(loopback("127.0.0.1:4000"));
        assert!(loopback("localhost:4000"));
        assert!(loopback("LocalHost"));
        assert!(loopback("[::1]:4000"));
        assert!(loopback("[::1]"));
        assert!(!loopback("evil.example:4000"));
        assert!(!loopback("127.0.0.1.evil.example"));
        assert!(!loopback("localhost.evil.example:4000"));
        let (_dir, router) = served();
        run(async {
            let req = Request::get("/open.html")
                .header("host", "evil.example:4000")
                .body(Body::empty())
                .unwrap();
            let response = router.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let req = Request::get("/open.html").body(Body::empty()).unwrap();
            let response = router.oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
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
            sidecar: Some((named("rec.toml"), vec![named("rec.tickwave")])),
        };
        assert!(Served::new(Box::new(Fake), TOKEN, LAUNCH.into(), files).is_err());
    }
}
