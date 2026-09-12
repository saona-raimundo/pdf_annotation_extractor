//! A one-page browser front end for `pdf_annotation_extractor`.
//!
//! The document never leaves the machine: `extract` runs in WebAssembly on the
//! bytes the browser already has. That is the reason this is a playground and
//! not a service — people try these things on unpublished papers.

#![forbid(unsafe_code)]

mod job;

use std::sync::Arc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use pdf_annotation_extractor::{Descriptor, Numbering, Severity};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use job::{Format, Outcome, Params};

const SAMPLES: [(&str, &str); 3] = [
    ("Firefox", "samples/base-firefox-153.0.4.pdf"),
    ("Okular", "samples/base-okular-26.04.3.pdf"),
    ("Papers", "samples/base-papers-50.2.pdf"),
];

#[derive(Clone)]
struct Doc {
    name: String,
    /// Shared rather than cloned: every parameter change re-runs `extract`,
    /// which takes the bytes by value, and a fifty-page paper is not something
    /// to copy on every keystroke of the overlap field.
    ///
    /// `Arc` rather than `Rc` even though this only ever runs on one thread:
    /// `RwSignal` stores its value in `SyncStorage` and so requires
    /// `Send + Sync`. `RwSignal::new_local` would take an `Rc`, at the price
    /// of panicking if the signal is ever read from another thread — a worse
    /// trade than one atomic per document load.
    bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Report,
    Notes,
}

fn main() {
    // Without this a panic in the library surfaces as `unreachable executed`
    // with no location, which is the least useful bug report possible.
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <App /> })
}

#[component]
fn App() -> impl IntoView {
    let doc = RwSignal::new(None::<Doc>);
    let params = RwSignal::new(Params::default());
    let outcome = RwSignal::new(None::<Result<Outcome, String>>);
    let reading = RwSignal::new(false);
    let over = RwSignal::new(false);
    let tab = RwSignal::new(Tab::Report);

    // Re-extract whenever the document or any parameter changes. Synchronous,
    // so the toggle and its result land in the same frame; the only await in
    // the application is reading the file, which is where the one visible
    // pause is.
    Effect::new(move |_| {
        let Some(d) = doc.get() else {
            outcome.set(None);
            return;
        };
        let p = params.get();
        let result = job::run(d.bytes.as_ref().clone(), &d.name, &p);
        outcome.set(Some(result));
    });

    let load = move |name: String, bytes: Vec<u8>| {
        doc.set(Some(Doc {
            name,
            bytes: Arc::new(bytes),
        }));
        reading.set(false);
    };

    let take_file = move |file: web_sys::File| {
        reading.set(true);
        spawn_local(async move {
            let name = file.name();
            match JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                    load(name, bytes);
                }
                Err(_) => {
                    reading.set(false);
                    outcome.set(Some(Err(format!("could not read {name}"))));
                }
            }
        });
    };

    let take_sample = move |url: &'static str| {
        reading.set(true);
        spawn_local(async move {
            match fetch_bytes(url).await {
                Ok(bytes) => {
                    let name = url.rsplit('/').next().unwrap_or(url).to_string();
                    load(name, bytes);
                }
                Err(e) => {
                    reading.set(false);
                    outcome.set(Some(Err(e)));
                }
            }
        });
    };

    view! {
        <h1>"pdf_annotation_extractor"</h1>
        <p class="tagline">
            "Drop an annotated PDF to pull out its highlights and comments. "
            "Everything runs in your browser; the file is never uploaded."
        </p>

        <div
            class="drop"
            class:over=move || over.get()
            on:dragover=move |ev| {
                ev.prevent_default();
                over.set(true);
            }
            on:dragleave=move |_| over.set(false)
            on:drop=move |ev| {
                ev.prevent_default();
                over.set(false);
                if let Some(file) = ev.data_transfer().and_then(|dt| dt.files()).and_then(|fs| fs.get(0)) {
                    take_file(file);
                }
            }
        >
            <p><strong>"Drop a PDF here"</strong></p>
            <p>
                "or "
                <input
                    type="file"
                    accept="application/pdf"
                    on:change=move |ev| {
                        let input: web_sys::HtmlInputElement = ev.target().unwrap().unchecked_into();
                        if let Some(file) = input.files().and_then(|fs| fs.get(0)) {
                            take_file(file);
                        }
                    }
                />
            </p>
            <p class="samples">
                "No PDF to hand? The same document, annotated in three different readers: "
                {SAMPLES
                    .iter()
                    .map(|&(label, url)| {
                        view! {
                            <button on:click=move |_| take_sample(url)>{label}</button>
                            " "
                        }
                    })
                    .collect_view()}
            </p>
        </div>

        <Controls params=params/>

        {move || {
            if reading.get() {
                return view! { <p class="status">"Reading the file…"</p> }.into_any();
            }
            outcome
                .with(|o| match o {
                    None => view! {
                        <p class="status">"Nothing loaded yet."</p>
                    }.into_any(),
                    Some(Err(message)) => {
                        let message = message.clone();
                        view! {
                            <p class="status error">{message}</p>
                        }.into_any()
                    }
                    Some(Ok(result)) => {
                        let name = doc.with(|d| d.as_ref().map(|d| d.name.clone()).unwrap_or_default());
                        view! { <Output result=result.clone() source=name params=params tab=tab/> }
                            .into_any()
                    }
                })
        }}

        <h2>{"What this does"}</h2>
        <p>
            {"Text markup in a PDF records "}<em>{"where"}</em>
            {" a highlight is (a set of rectangle coordinates, quadrilaterals) not the text underneath it.
            The text has to be recovered by intersecting each quadrilateral with the
            glyph boxes on the page, which is what this crate does. Comments, authors,
            dates and colours come from the annotation dictionary itself."}
        </p>
        <p>
            {"The "}<b>{"Diagnostics"}</b>
            {" tells you if something unexpected happened: it lists what had to be assumed and
            what was given up on. An annotated PDF with no diagnostics was read cleanly; one
            with warnings should be checked against the document."}
        </p>

        <footer>
            <h3>{"This playground"}</h3>
            <p>
                {"Source code: "}
                <a href="https://github.com/saona-raimundo/pdf_annotation_extractor">
                    {"GitHub"}
                </a>
                {" · The crate: "}
                <a href="https://crates.io/crates/pdf_annotation_extractor">{"crates.io"}</a>
                {", "}
                <a href="https://docs.rs/pdf_annotation_extractor">{"docs.rs"}</a>
            </p>
            <p>
                {"Licence: "}
                <a rel="license" href="https://creativecommons.org/publicdomain/zero/1.0/">
                    <img
                        alt="Creative Commons Licence"
                        style="border-width:0"
                        src="https://i.creativecommons.org/l/by/4.0/80x15.png"
                    />
                </a> <a rel="license" href="https://creativecommons.org/publicdomain/zero/1.0/">
                    {"CC0 1.0 Universal"}
                </a>
            </p>
            <address>
                {"Author: 🧑🏼‍💻"}
                <a href="https://saona-raimundo.github.io/">{"Raimundo Saona"}</a>
            </address>
        </footer>
    }
}

#[component]
fn Controls(params: RwSignal<Params>) -> impl IntoView {
    let toggle_descriptor = move |d: Descriptor| {
        params.update(|p| match p.descriptors.iter().position(|x| *x == d) {
            Some(i) => {
                p.descriptors.remove(i);
            }
            None => p.descriptors.push(d),
        });
    };
    let has = move |d: Descriptor| params.with(|p| p.descriptors.contains(&d));

    view! {
        <div class="controls">
            <label>
                "Format "
                <select on:change=move |ev| {
                    let value = event_target_value(&ev);
                    params.update(|p| {
                        p.format = if value == "json" { Format::Json } else { Format::Markdown };
                    });
                }>
                    <option value="markdown">"Markdown"</option>
                    <option value="json">"JSON"</option>
                </select>
            </label>

            <Flag label="Keep ligatures" get=move || params.with(|p| p.keep_ligatures)
                set=move || params.update(|p| p.keep_ligatures = !p.keep_ligatures)/>
            <Flag label="Keep hyphens" get=move || params.with(|p| p.keep_hyphens)
                set=move || params.update(|p| p.keep_hyphens = !p.keep_hyphens)/>
            <Flag label="Split quads" get=move || params.with(|p| p.split_quads)
                set=move || params.update(|p| p.split_quads = !p.split_quads)/>
            <Flag label="Keep empty" get=move || params.with(|p| p.keep_empty)
                set=move || params.update(|p| p.keep_empty = !p.keep_empty)/>
        </div>

        <details class="advanced">
            <summary>"Markdown style and matching thresholds"</summary>
            <div class="controls">
                <label>
                    "Numbering "
                    <select on:change=move |ev| {
                        let value = event_target_value(&ev);
                        params.update(|p| {
                            p.numbering = match value.as_str() {
                                "global" => Numbering::Global,
                                "per-page" => Numbering::PerPage,
                                _ => Numbering::None,
                            };
                        });
                    }>
                        <option value="none">"None"</option>
                        <option value="global">"Global"</option>
                        <option value="per-page">"Per page"</option>
                    </select>
                </label>

                <span>"Show:"</span>
                <Flag label="Colour" get=move || has(Descriptor::Colour)
                    set=move || toggle_descriptor(Descriptor::Colour)/>
                <Flag label="Kind" get=move || has(Descriptor::Kind)
                    set=move || toggle_descriptor(Descriptor::Kind)/>
                <Flag label="Author" get=move || has(Descriptor::Author)
                    set=move || toggle_descriptor(Descriptor::Author)/>
                <Flag label="Date" get=move || has(Descriptor::Date)
                    set=move || toggle_descriptor(Descriptor::Date)/>
            </div>
            <div class="controls">
                <label>
                    "Minimum overlap "
                    <input
                        type="number" min="0" max="1" step="0.05"
                        prop:value=move || params.with(|p| p.min_overlap.to_string())
                        on:change=move |ev| {
                            if let Ok(v) = event_target_value(&ev).parse::<f32>() {
                                params.update(|p| p.min_overlap = v);
                            }
                        }
                    />
                </label>
                <label>
                    "Space gap "
                    <input
                        type="number" min="0" max="2" step="0.05"
                        prop:value=move || params.with(|p| p.space_gap.to_string())
                        on:change=move |ev| {
                            if let Ok(v) = event_target_value(&ev).parse::<f32>() {
                                params.update(|p| p.space_gap = v);
                            }
                        }
                    />
                </label>
                <Flag label="Per-page counts" get=move || params.with(|p| p.page_counts)
                    set=move || params.update(|p| p.page_counts = !p.page_counts)/>
            </div>
        </details>
    }
}

#[component]
fn Flag(
    label: &'static str,
    get: impl Fn() -> bool + Copy + Send + Sync + 'static,
    set: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    view! {
        <label>
            <input type="checkbox" prop:checked=get on:change=move |_| set()/>
            {label}
        </label>
    }
}

#[component]
fn Output(
    result: Outcome,
    source: String,
    params: RwSignal<Params>,
    tab: RwSignal<Tab>,
) -> impl IntoView {
    let format = params.with_untracked(|p| p.format);
    let file_name = result.file_name(&source, format);
    let text = result.text.clone();
    let notes = result.notes.clone();
    let annotations = result.annotations;
    let warnings = result.warnings;
    let note_count = notes.len();

    let for_download = text.clone();
    let for_clipboard = text.clone();

    view! {
        <p class="status">
            {format!("{annotations} annotation(s)")}
            {move || if warnings > 0 { format!(", {warnings} warning(s)") } else { String::new() }}
            " · "
            <button on:click=move |_| copy(&for_clipboard)>"Copy"</button>
            " "
            <button on:click={
                let name = file_name.clone();
                move |_| download(&name, mime(format), &for_download)
            }>"Download"</button>
        </p>

        <div class="tabs">
            <button
                class:active=move || tab.get() == Tab::Report
                on:click=move |_| tab.set(Tab::Report)
            >"Report"</button>
            <button
                class:active=move || tab.get() == Tab::Notes
                on:click=move |_| tab.set(Tab::Notes)
            >{format!("Diagnostics ({note_count})")}</button>
        </div>

        {move || match tab.get() {
            Tab::Report => view! { <pre>{text.clone()}</pre> }.into_any(),
            Tab::Notes if note_count == 0 => view! {
                <pre>"Nothing was assumed or skipped."</pre>
            }.into_any(),
            Tab::Notes => view! {
                <pre>
                    {notes
                        .iter()
                        .map(|n| {
                            let class = match n.severity {
                                Severity::Warning => "diag-warning",
                                Severity::Note => "diag-note",
                                Severity::Info => "diag-info",
                            };
                            view! { <div class=class>{n.text.clone()}</div> }
                        })
                        .collect_view()}
                </pre>
            }.into_any(),
        }}
    }
}

fn mime(format: Format) -> &'static str {
    match format {
        Format::Markdown => "text/markdown;charset=utf-8",
        Format::Json => "application/json;charset=utf-8",
    }
}

async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|_| format!("could not fetch {url}"))?
        .unchecked_into::<web_sys::Response>();
    if !response.ok() {
        return Err(format!("could not fetch {url}: {}", response.status()));
    }
    let buffer = JsFuture::from(response.array_buffer().map_err(|_| "no body")?)
        .await
        .map_err(|_| "could not read the response")?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

fn copy(text: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}

/// Hand the text to the browser as a file, without a round trip to a server.
fn download(file_name: &str, mime: &str, text: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };

    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(text));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);

    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };

    if let Ok(element) = document.create_element("a") {
        let anchor: web_sys::HtmlAnchorElement = element.unchecked_into();
        anchor.set_href(&url);
        anchor.set_download(file_name);
        anchor.click();
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}
