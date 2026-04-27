//! Embedded SvelteKit SPA bundle. The `spa/build/` directory is produced
//! by `npm run build` (see `crates/pitboss-web/spa/`). When the SPA hasn't
//! been built yet, the embed is empty and the fallback handler returns a
//! placeholder so the binary still runs for backend smoke tests.

use axum::{
    body::Body,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/spa/build/"]
struct SpaAssets;

pub async fn handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(resp) = serve_file(path) {
        return resp;
    }
    // SPA fallback: serve index.html so client-side routing works.
    if let Some(resp) = serve_file("index.html") {
        return resp;
    }
    placeholder_html()
}

fn serve_file(path: &str) -> Option<Response> {
    let asset = SpaAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .body(Body::from(asset.data.into_owned()))
            .expect("static response"),
    )
}

fn placeholder_html() -> Response {
    let body = r#"<!doctype html>
<html><head><title>pitboss-web</title>
<style>body{font:14px system-ui;margin:3em;max-width:42em;color:#222}
code{background:#eee;padding:2px 4px;border-radius:3px}</style>
</head><body>
<h1>pitboss-web</h1>
<p>The SvelteKit SPA bundle hasn't been built yet. Run:</p>
<pre><code>cd crates/pitboss-web/spa && npm install && npm run build</code></pre>
<p>Then restart this server. Backend API endpoints are live at
<code>/api/runs</code>, <code>/api/runs/:id</code>, etc.</p>
</body></html>
"#;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(body))
        .expect("placeholder response")
        .into_response()
}
