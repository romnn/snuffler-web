//! Files the browser reads for the user: picked waveform files and directories, the
//! watched-file reload, and the single files a metadata picker returns.
//!
//! `bridge.js` hands files over a page at a time.
//! A load is one ledger flow whose children are the per-file `LoadBytes` or
//! `LoadBundle` calls, so a cancel stops it between files.

use super::*;

/// How a loaded file relates to what the pile already holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceLoadMode {
    /// Add the file, as `File ▸ Open` does.
    Append,
    /// Replace the traces an earlier read of the same source installed.
    Replace,
}

/// What loading one browser file did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserFileOutcome {
    Installed,
    /// Pyrocko already held identical content, so nothing changed.
    Duplicate,
}

/// How loading one page of files ended.
enum FilePageOutcome {
    /// `bridge.js` has another page to read.
    More,
    Complete,
    Cancelled {
        /// Lower bound on the files not attempted.
        remaining: u32,
    },
    /// A child failed below the rpc layer (transport, protocol drift, stale
    /// generation), so the flow itself is broken and must not issue further
    /// children.
    /// After a runtime loss they would commit into the replacement interpreter's
    /// fresh session under an already-retired job.
    Aborted(CallError),
}

/// Running totals of one multi-page load.
#[derive(Default)]
struct FileLoadProgress {
    /// Files that changed the pile.
    committed: u32,
    /// Files attempted, whether or not they changed anything.
    processed: u32,
    failures: Vec<ReloadSourceError>,
}

/// Decodes the protobuf page a `bridge.js` file call resolved to.
pub(super) fn decode_browser_file_page(
    result: Result<JsValue, JsValue>,
    field: &'static str,
    action: &'static str,
) -> Result<BrowserFileLoadPage, BridgeError> {
    let value = result.map_err(|error| BridgeError::browser(action, js_value_to_string(error)))?;
    let bytes = uint8_array_bytes(value, field)?;
    proto::decode_message(&bytes, field).map_err(Into::into)
}

/// The files of a picker page, all or nothing.
pub(super) fn file_page_files(
    page: BrowserFileLoadPage,
) -> Result<Vec<pb::BrowserFile>, BridgeError> {
    if page.cancelled {
        return Ok(Vec::new());
    }
    // These aggregate imports are all-or-error: silently loading four of five
    // station files would leave the user with an incomplete table and no signal,
    // so any per-file failure (oversized, unreadable) rejects the operation.
    if !page.failures.is_empty() {
        return Err(BridgeError::browser(
            "reading selected files",
            page.failures
                .iter()
                .map(|failure| format!("{}: {}", failure.name, failure.detail))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    Ok(page.files)
}

/// The UTF-8 texts of the files a metadata picker returned.
pub(super) fn picked_file_texts(
    result: Result<JsValue, JsValue>,
    field: &'static str,
    action: &'static str,
) -> Result<Vec<String>, BridgeError> {
    let page = decode_browser_file_page(result, field, action)?;
    file_page_files(page)?
        .into_iter()
        .map(|file| {
            String::from_utf8(file.content).map_err(|error| {
                BridgeError::browser("decoding a selected text file", error.to_string())
            })
        })
        .collect()
}

/// Tells `bridge.js` which version of a file handle is now loaded, so the watched-file
/// poll only rereads it once it changes again.
fn acknowledge_source(file: &pb::BrowserFile) {
    if file.source_id.is_empty() {
        return;
    }
    js_acknowledge_sources(
        BrowserSourceAckList {
            sources: vec![BrowserSourceAck {
                source_id: file.source_id.clone(),
                last_modified: file.last_modified,
                bundle: file.bundle,
            }],
        }
        .encode_to_vec(),
    );
}

/// Loads one file as a bundle or as waveforms, depending on what `bridge.js` saw.
async fn load_browser_file(
    inner: &Rc<RefCell<Inner>>,
    file: &mut pb::BrowserFile,
    mode: SourceLoadMode,
) -> Result<BrowserFileOutcome, CallError> {
    let content = std::mem::take(&mut file.content);
    if file.bundle {
        retire_live_session(inner);
        return client()
            .load_bundle(pb::LoadBundleRequest {
                url: String::new(),
                sha256: String::new(),
                content,
                name: file.name.clone(),
                marker_budget: Some(standard_marker_budget()),
            })
            .await
            .map(|bundle| {
                inner.borrow_mut().install_bundle_result(bundle);
                BrowserFileOutcome::Installed
            });
    }
    client()
        .load_bytes(pb::LoadBytesRequest {
            name: file.name.clone(),
            format: "detect".to_owned(),
            append: true,
            content,
            source_id: file.source_id.clone(),
            replace_source: mode == SourceLoadMode::Replace && !file.source_id.is_empty(),
        })
        .await
        .map(|result| {
            let outcome = if result.loaded {
                BrowserFileOutcome::Installed
            } else {
                BrowserFileOutcome::Duplicate
            };
            inner.borrow_mut().install_waveform_result(result);
            outcome
        })
}

/// Loads every file of one page, stopping at a cancel or a broken flow.
async fn load_file_page(
    inner: &Rc<RefCell<Inner>>,
    flow: JobHandle,
    mut page: BrowserFileLoadPage,
    mode: SourceLoadMode,
    progress: &mut FileLoadProgress,
) -> FilePageOutcome {
    // Files the browser could not even read
    progress.failures.extend(
        std::mem::take(&mut page.failures)
            .into_iter()
            .map(|failure| ReloadSourceError {
                source_id: failure.source_id,
                name: failure.name,
                reason: ReloadFailureReason::BrowserRead {
                    detail: failure.detail,
                },
            }),
    );

    let page_count = u32::try_from(page.files.len()).unwrap_or(u32::MAX);
    let known_total = progress
        .processed
        .saturating_add(page_count)
        .saturating_add(u32::from(page.has_more));
    for (index, mut file) in page.files.into_iter().enumerate() {
        let remaining = page_count
            .saturating_sub(u32::try_from(index).unwrap_or(u32::MAX))
            .saturating_add(u32::from(page.has_more));
        if flow_must_stop(inner, flow) {
            return FilePageOutcome::Cancelled { remaining };
        }
        update_flow_progress(
            inner,
            flow,
            u64::from(progress.processed),
            u64::from(known_total),
            format!("Loading {}", file.name),
        );
        match load_browser_file(inner, &mut file, mode).await {
            Ok(BrowserFileOutcome::Installed) => {
                progress.committed = progress.committed.saturating_add(1);
                acknowledge_source(&file);
            }
            Ok(BrowserFileOutcome::Duplicate) => {}
            Err(CallError::Cancelled { .. }) => {
                return FilePageOutcome::Cancelled { remaining };
            }
            // Only a pyrocko rejection is a per-file verdict worth continuing past;
            // anything below the rpc layer is a broken flow.
            Err(CallError::Remote(remote)) => progress.failures.push(ReloadSourceError {
                source_id: file.source_id.clone(),
                name: file.name.clone(),
                reason: ReloadFailureReason::Remote(remote),
            }),
            // The abort still joins the failure list, so the diagnostic keeps the
            // per-file failures that accumulated before the flow broke.
            Err(error) => {
                progress.failures.push(ReloadSourceError {
                    source_id: file.source_id.clone(),
                    name: file.name.clone(),
                    reason: match &error {
                        CallError::Transport(transport) => ReloadFailureReason::Transport {
                            detail: transport.to_string(),
                        },
                        other => ReloadFailureReason::Protocol {
                            detail: other.to_string(),
                        },
                    },
                });
                return FilePageOutcome::Aborted(error);
            }
        }
        progress.processed = progress.processed.saturating_add(1);
    }
    if page.has_more {
        FilePageOutcome::More
    } else {
        FilePageOutcome::Complete
    }
}

/// Tells `bridge.js` to stop reading pages for this load.
fn cancel_browser_file_load(load_id: &str) {
    if !load_id.is_empty() {
        js_cancel_file_load(
            pb::BrowserFileLoadPageRequest {
                load_id: load_id.to_owned(),
            }
            .encode_to_vec(),
        );
    }
}

/// Failures that survive to a terminal outcome surface as one typed per-source
/// diagnostic, so a pyrocko rejection keeps its traceback.
fn set_reload_diagnostic(inner: &Rc<RefCell<Inner>>, failures: &mut Vec<ReloadSourceError>) {
    if !failures.is_empty() {
        let error = BridgeError::from(ReloadError {
            failures: std::mem::take(failures),
        });
        inner.borrow_mut().diagnostic = Some(error);
    }
}

/// Ends a cancelled load: `Stopped` when some files landed, `Cancelled` otherwise.
fn stop_file_load(
    inner: &Rc<RefCell<Inner>>,
    progress: &mut FileLoadProgress,
    skipped: u32,
    load_id: &str,
) -> Result<Outcome, BridgeError> {
    cancel_browser_file_load(load_id);
    set_reload_diagnostic(inner, &mut progress.failures);
    if progress.committed == 0 {
        Err(BridgeError::Call(CallError::Cancelled {
            method: "browser file load",
        }))
    } else {
        Ok(Outcome::Stopped {
            committed: progress.committed,
            skipped,
        })
    }
}

/// Loads every page of a picked or watched file set, as one ledger flow.
async fn run_file_load(
    inner: Rc<RefCell<Inner>>,
    flow: JobHandle,
    mut page: BrowserFileLoadPage,
    mode: SourceLoadMode,
) -> Result<Outcome, BridgeError> {
    let load_id = page.load_id.clone();
    if let Err(error) = ensure_flow_live(&inner, flow, "browser file load") {
        cancel_browser_file_load(&load_id);
        return Err(error);
    }
    if page.cancelled {
        return Ok(Outcome::Succeeded);
    }

    // Load page by page
    let mut progress = FileLoadProgress::default();
    loop {
        match load_file_page(&inner, flow, page, mode, &mut progress).await {
            FilePageOutcome::More => {}
            FilePageOutcome::Complete => break,
            FilePageOutcome::Cancelled { remaining } => {
                return stop_file_load(&inner, &mut progress, remaining, &load_id);
            }
            FilePageOutcome::Aborted(error) => {
                cancel_browser_file_load(&load_id);
                // The failure list already carries the abort plus everything
                // accumulated before it; the flow's terminal outcome carries
                // the systemic error itself.
                set_reload_diagnostic(&inner, &mut progress.failures);
                return Err(BridgeError::Call(error));
            }
        }
        if flow_must_stop(&inner, flow) {
            return stop_file_load(&inner, &mut progress, 1, &load_id);
        }
        let page_request = pb::BrowserFileLoadPageRequest {
            load_id: load_id.clone(),
        }
        .encode_to_vec();
        page = match decode_browser_file_page(
            js_read_file_load_page(page_request).await,
            "read_file_load_page",
            "reading the next file page",
        ) {
            Ok(next) => next,
            Err(error) => {
                cancel_browser_file_load(&load_id);
                return Err(error);
            }
        };
    }

    // Finish
    update_flow_progress(
        &inner,
        flow,
        u64::from(progress.processed),
        u64::from(progress.processed),
        "Finishing file load".to_owned(),
    );
    if flow_must_stop(&inner, flow) {
        // Cancellation arrived after the final commit; nothing was skipped,
        // but per-file failures accumulated on the way must still surface.
        set_reload_diagnostic(&inner, &mut progress.failures);
        return Ok(Outcome::TooLate);
    }
    if progress.failures.is_empty() {
        inner.borrow_mut().diagnostic = None;
        return Ok(Outcome::Succeeded);
    }
    let committed = progress.committed;
    let error = BridgeError::from(ReloadError {
        failures: progress.failures,
    });
    inner.borrow_mut().diagnostic = Some(error.clone());
    if committed == 0 {
        Err(error)
    } else {
        Ok(Outcome::Succeeded)
    }
}

impl Bridge {
    /// Open local waveform files via a browser picker and append them.
    pub fn open_files(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow_with_outcome(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::OpenSelectedFiles,
            Some("selected files".to_owned()),
            Vec::new(),
            move |flow| async move {
                let page =
                    decode_browser_file_page(js_open_files().await, "open_files", "opening files")?;
                run_file_load(inner, flow, page, SourceLoadMode::Append).await
            },
        )
    }

    /// Recursively load every waveform file under a picked directory
    /// (Chromium File System Access); appends them to the pile.
    pub fn open_directory(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow_with_outcome(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::OpenDirectory,
            Some("selected directory".to_owned()),
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_open_directory().await,
                    "open_directory",
                    "opening a directory",
                )?;
                run_file_load(inner, flow, page, SourceLoadMode::Append).await
            },
        )
    }

    /// Re-read any opened files whose on-disk mtime changed and replace their
    /// traces (snuffler's "r").
    ///
    /// Successful sources are installed even when another source in the same
    /// batch fails.
    /// Those failures remain visible through the recoverable diagnostic slot.
    pub fn reload_files(&self) -> Submission {
        self.inner.borrow_mut().diagnostic = None;
        let inner = self.inner.clone();
        submit_flow_with_outcome(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ReloadOpenFiles,
            Some("modified files".to_owned()),
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_reload_files().await,
                    "reload_files",
                    "reloading files",
                )?;
                run_file_load(inner, flow, page, SourceLoadMode::Replace).await
            },
        )
    }

    /// Polls watched files without showing a loading status.
    ///
    /// Successful changes install immediately; source failures still surface
    /// because otherwise their earlier batch peers would change invisibly.
    pub fn poll_watched_files(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow_with_outcome(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::PollWatchedFiles,
            None,
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_reload_files().await,
                    "poll_watched_files",
                    "watching files",
                )?;
                run_file_load(inner, flow, page, SourceLoadMode::Replace).await
            },
        )
    }
}
