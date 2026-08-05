//! The annotation editor window and the commands behind it.
//!
//! The editor is a third window (M2 §1) that edits exactly one capture at a
//! time. Rust owns the window lifecycle, reads the source PNG off disk and takes
//! the finished image back as base64; everything between those two points —
//! shapes, undo, crop, rendering — lives in the webview.

use std::fs;
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use image::RgbaImage;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::capture;
use crate::store::{self, AppState, CaptureRecord};
use crate::{lock, run_blocking};

pub const EDITOR_LABEL: &str = "editor";

/// Sent to a live editor window when a second edit request arrives, payload the
/// new capture id. Re-targeting beats building a duplicate window, and the
/// editor is the only thing that knows whether its current document is dirty.
const LOAD_EVENT: &str = "editor://load";

/// Emitted when `save_edit` rewrites an existing capture instead of adding one.
/// The history store replaces the record in place rather than prepending it.
const UPDATED_EVENT: &str = "capture://updated";

// -------------------------------------------------------------------------
// commands
// -------------------------------------------------------------------------

#[tauri::command]
pub async fn open_editor(app: AppHandle, id: String) -> Result<(), String> {
    run_blocking(move || open_editor_blocking(&app, &id)).await
}

#[tauri::command]
pub fn get_capture(app: AppHandle, id: String) -> Result<CaptureRecord, String> {
    store::capture_by_id(&app, &id)
}

/// Absolute path to the full-resolution PNG. The frontend runs it through
/// `convertFileSrc` before handing it to an `<img>`.
#[tauri::command]
pub fn get_capture_image(app: AppHandle, id: String) -> Result<String, String> {
    let record = store::capture_by_id(&app, &id)?;

    // A capture taken with "save to disk" off only ever existed in the
    // clipboard and as a 480px thumbnail — there is nothing to edit.
    if record.path.is_empty() {
        return Err(format!(
            "\"{}\" was never written to disk, so there is no image to edit",
            record.name
        ));
    }
    if !Path::new(&record.path).exists() {
        return Err(format!("{} is no longer on disk", record.name));
    }
    Ok(record.path)
}

#[tauri::command]
pub async fn save_edit(
    app: AppHandle,
    id: String,
    png: String,
    replace: bool,
) -> Result<CaptureRecord, String> {
    run_blocking(move || {
        let bytes = decode_base64(&png)?;
        let img = decode_png(&bytes)?;

        let saved = if replace {
            replace_capture(&app, &id, &img, &bytes)
        } else {
            // A new record goes through M1's pipeline unchanged, so "save as
            // new" honours the filename pattern, the clipboard setting and
            // open_folder_after exactly like a fresh capture does.
            crate::finalize(&app, img, "edit")
        }?;

        // M6 §2: a workflow's Annotate step is blocked on this. `new_id` is the
        // record the rest of the chain must carry — on "save as new" the
        // original still holds the un-annotated image, and uploading that is
        // the privacy failure the whole wait exists to prevent.
        crate::workflow::editor_finished(
            &app,
            &id,
            true,
            (!replace).then(|| saved.id.clone()),
        );
        Ok(saved)
    })
    .await
}

#[tauri::command]
pub async fn copy_edit(app: AppHandle, png: String) -> Result<(), String> {
    run_blocking(move || {
        let img = decode_png(&decode_base64(&png)?)?;
        crate::copy_rgba_to_clipboard(&app, &img)
    })
    .await
}

#[tauri::command]
pub async fn close_editor(app: AppHandle) -> Result<(), String> {
    run_blocking(move || {
        // Closing without saving is a cancel, and a workflow waiting on this
        // editor has to hear it as one — it ends the chain rather than carrying
        // an un-annotated image on to the destination. Sent before the window
        // goes, so the `Destroyed` handler's grace timer never has to fire.
        crate::workflow::editor_cancelled(&app);

        if let Some(window) = app.get_webview_window(EDITOR_LABEL) {
            // `destroy`, not `close`: the caller has already decided (and, if the
            // document was dirty, already asked). `close` would come back round
            // as a CloseRequested the editor would have to answer again.
            window
                .destroy()
                .map_err(|e| format!("could not close the editor: {e}"))?;
        }
        Ok(())
    })
    .await
}

// -------------------------------------------------------------------------
// internals
// -------------------------------------------------------------------------

/// The blocking body of `open_editor`, also called from `finalize` when
/// `open_editor_after` is set.
///
/// Must never run on the main thread: `WebviewWindowBuilder::build` blocks
/// waiting on the event loop and would deadlock, exactly as in `overlay.rs`.
pub(crate) fn open_editor_blocking(app: &AppHandle, id: &str) -> Result<(), String> {
    let record = store::capture_by_id(app, id)?;

    // M4 §5: the editor refuses a recording *with a clear message* rather than
    // opening on a broken canvas. Every UI that offers Edit already asks this
    // question, so reaching here means a path that did not — and the failure
    // without this check is not an error dialog, it is an editor window sitting
    // open on nothing while `image::open` chokes on an MP4 behind it.
    if record.kind == crate::record::RECORD_KIND {
        return Err(format!(
            "The editor works on still images. \"{}\" is a screen recording.",
            record.name
        ));
    }

    let title = format!("Edit — {}", record.name);

    // One editor at a time (M2 §1): a second request re-points the live window
    // instead of stacking another on the same label.
    if let Some(window) = app.get_webview_window(EDITOR_LABEL) {
        let _ = window.set_title(&title);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return app
            .emit_to(EDITOR_LABEL, LOAD_EVENT, record.id)
            .map_err(|e| format!("could not hand the editor a new capture: {e}"));
    }

    // Ids are "{millis}-{n}" (store::next_id), so they carry nothing that would
    // need percent-encoding in the query.
    let url = format!("?w=editor&id={}", record.id);

    WebviewWindowBuilder::new(
        app,
        EDITOR_LABEL,
        // The bare query, *not* "index.html?w=editor": under `tauri dev` the app
        // base URL is the SvelteKit dev server, which has no /index.html route
        // and answers it with a 404 page. See the same note in overlay.rs.
        WebviewUrl::App(url.into()),
    )
    .title(title)
    .inner_size(1280.0, 820.0)
    .min_inner_size(900.0, 600.0)
    .decorations(true)
    .resizable(true)
    .center()
    .build()
    .map_err(|e| format!("could not create the editor window: {e}"))?;

    Ok(())
}

/// Overwrites the original file in place and updates its record. The id, name
/// and path all survive — from the gallery's point of view this is the same
/// capture with new pixels.
fn replace_capture(
    app: &AppHandle,
    id: &str,
    img: &RgbaImage,
    encoded: &[u8],
) -> Result<CaptureRecord, String> {
    let mut record = store::capture_by_id(app, id)?;
    if record.path.is_empty() {
        return Err(format!(
            "\"{}\" has no file to overwrite — save it as a new capture instead",
            record.name
        ));
    }

    // The canvas already handed us a PNG; re-encoding it would only cost time.
    fs::write(&record.path, encoded)
        .map_err(|e| format!("could not overwrite {}: {e}", record.path))?;

    let (width, height) = img.dimensions();
    record.width = width;
    record.height = height;
    record.size_bytes = encoded.len() as u64;
    record.saved = true;

    // Same path as before, so the gallery's <img> src is unchanged and the
    // frontend has to bust its own cache off `created_at`/`size_bytes`.
    let thumb_path = store::thumb_path(app, &record.id);
    let thumb_bytes = capture::encode_png_fast(&capture::thumbnail(img))?;
    fs::write(&thumb_path, &thumb_bytes)
        .map_err(|e| format!("could not write the thumbnail: {e}"))?;
    record.thumb = thumb_path.to_string_lossy().into_owned();

    {
        let state = app.state::<AppState>();
        let mut history = lock(&state.history);
        let slot = history
            .iter_mut()
            .find(|r| r.id == record.id)
            .ok_or_else(|| format!("no capture with id {}", record.id))?;
        *slot = record.clone();
        store::persist_history(app, &mut history)?;
    }

    let _ = app.emit(UPDATED_EVENT, record.clone());
    Ok(record)
}

/// The frontend strips the `data:image/png;base64,` prefix off `toDataURL`, but
/// tolerate one arriving anyway rather than rejecting a perfectly good image.
fn decode_base64(png: &str) -> Result<Vec<u8>, String> {
    let payload = png
        .split_once("base64,")
        .map(|(_, rest)| rest)
        .unwrap_or(png);

    STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("the edited image is not valid base64: {e}"))
}

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, String> {
    Ok(
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map_err(|e| format!("the edited image is not a readable PNG: {e}"))?
            .to_rgba8(),
    )
}
