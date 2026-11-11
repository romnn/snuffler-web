//! Operations that run pyrocko over the pile: figures, snufflings, RMS traces,
//! ':' commands, earth models and Green's-function stores.

use super::*;

impl Bridge {
    /// Render a real pyrocko/matplotlib figure (amplitude spectrum, cake, …)
    /// in Pyodide and store the resulting PNG for the matching figure window.
    /// Keyed by `request.kind`, so several figure windows can be open at once.
    pub fn render_figure(&self, request: FigureRequest) -> Submission {
        // No slot-level short circuit here: the ledger's DEDUP policy on the
        // encoded request already joins identical work, and a same-kind
        // request with different params must queue, not silently join.
        // Joining it would leave the window showing the old figure forever.
        let kind = request.kind.clone();
        let inner = self.inner.clone();
        let dedup_key = request.encode_to_vec();
        let slot_kind = kind.clone();
        // Filled in once the ledger has minted the handle, before the work can run.
        let own_job = Rc::new(Cell::new(None));
        let work_own_job = own_job.clone();
        let submission = submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::RenderFigure,
            Some(kind.clone()),
            dedup_key,
            async move {
                let rendered = client().render_figure(request).await;
                let mut state = inner.borrow_mut();
                let slot = state.figures.entry(slot_kind).or_default();
                // A queued successor may already own the slot's spinner.
                if slot.job == work_own_job.get() {
                    slot.job = None;
                }
                let figure = rendered?;
                slot.data = FigureData {
                    png: figure.png,
                    error: figure.error,
                };
                slot.generation = slot.generation.wrapping_add(1);
                Ok(())
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            own_job.set(Some(handle));
            self.inner.borrow_mut().figures.entry(kind).or_default().job = Some(handle);
        }
        submission
    }

    /// Run a built-in snuffling (`MinMax`, `STA/LTA`, …) and store its result.
    pub fn run_snuffling(&self, name: String, params: SnufflingRequestParams) -> Submission {
        let request = pb::NamedSnufflingRequest {
            name,
            params: Some(params),
        };
        let inner = self.inner.clone();
        let dedup_key = request.encode_to_vec();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::RunSnuffling,
            Some(request.name.clone()),
            dedup_key,
            async move {
                let mut result = client().run_snuffling(request).await?;
                let mut state = inner.borrow_mut();
                if let Some(scene) = result.scene.take() {
                    state.replace_scene(scene);
                }
                state.install_snuffling_result(result);
                Ok(())
            },
        )
    }

    /// Computes blockwise RMS traces, replacing the previous RMS run.
    pub fn rms_traces(&self, params: SnufflingRequestParams) -> Submission {
        let inner = self.inner.clone();
        let dedup_key = params.encode_to_vec();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::RmsTraces,
            None,
            dedup_key,
            async move {
                let mut result = client().rms_traces(params).await?;
                let mut state = inner.borrow_mut();
                if let Some(scene) = result.scene.take() {
                    state.replace_scene(scene);
                }
                state.install_snuffling_result(result);
                Ok(())
            },
        )
    }

    /// Execute a ':' command line (goto / hide / unhide / reduce / scaling).
    ///
    /// A goto stores a pending time the session jumps to; a visibility change swaps the
    /// scene in place (no refit) and forces the view to re-dispatch.
    /// Either way the outcome lands in `command_outcome` for the command line to act on.
    pub fn run_command(&self, request: CommandRequest) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::RunCommand,
            Some(request.line.clone()),
            Vec::new(),
            async move {
                let result = client().run_command(request).await?;
                let mut state = inner.borrow_mut();
                match result.action.as_str() {
                    "goto" => {
                        state.pending_goto = result.time.map(|time| GotoTarget {
                            time,
                            length_s: result.window_length_s,
                        });
                    }
                    "scene" => {
                        if let Some(scene) = result.scene {
                            state.scene = Some(scene);
                            // Re-dispatch without refitting the time window.
                            state.invalidate_view_request();
                        }
                    }
                    _ => {}
                }
                state.command_outcome = Some(match (result.action.as_str(), result.message) {
                    ("error", Some(message)) => CommandOutcome::Failed(message),
                    ("error", None) => CommandOutcome::Failed("command failed".to_owned()),
                    _ => CommandOutcome::Succeeded,
                });
                Ok(())
            },
        )
    }

    /// Reload the Python snuffling code (the `web/snuffler_bridge` package) in the worker,
    /// preserving the loaded pile (snuffler's Shift+R).
    ///
    /// Forces a view re-fetch since the reload resets Python's view cache; the scene
    /// (track list) is unchanged.
    pub fn reload_snufflings(&self) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ReloadSnufflings,
            None,
            Vec::new(),
            async move {
                let job = lane_owner(Operation::ReloadSnufflings, "reloadSnufflings")
                    .map_err(CallError::from)?;
                js_reload_snufflings(job.id().get())
                    .await
                    .map_err(|error| {
                        BridgeError::browser("reloading snufflings", js_value_to_string(error))
                    })?;
                inner.borrow_mut().invalidate_view_request();
                Ok(())
            },
        )
    }

    /// Let the user pick a custom `.nd` model file ("Add Model"); pyrocko
    /// stores it and the returned name is added to the Cake Phase dropdown.
    pub fn add_cake_model(&self) -> Submission {
        let inner = self.inner.clone();
        submit_flow(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::AddCakeModel,
            None,
            Vec::new(),
            move |flow| async move {
                let page = decode_browser_file_page(
                    js_pick_cake_model().await,
                    "cake_model",
                    "adding an earth model",
                )?;
                ensure_flow_live(&inner, flow, "adding an earth model")?;
                let Some(file) = file_page_files(page)?.into_iter().next() else {
                    return Ok(());
                };
                let label = client()
                    .add_cake_model(pb::AddCakeModelRequest {
                        name: file.name,
                        content: file.content,
                    })
                    .await?;
                if !label.value.is_empty() {
                    inner.borrow_mut().pending_cake_model = Some(label.value);
                }
                Ok(())
            },
        )
    }

    /// Mount a Green's-functions store by fetching its files from an HTTP base URL into
    /// the Pyodide filesystem (no directory picker) and register it as `GF Store: <id>`.
    /// Errors surface in the snuffling text window.
    pub fn load_gf_store_url(&self, url: String) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadGfStoreUrl,
            Some(url.clone()),
            Vec::new(),
            async move {
                let label = client()
                    .load_gf_store_url(pb::StringRequest { value: url })
                    .await?;
                if !label.value.is_empty() {
                    inner.borrow_mut().pending_cake_model = Some(label.value);
                }
                Ok(())
            },
        )
    }

    /// Mount a Green's-functions store directory (`NativeFS`, via the File System Access
    /// API) and add its embedded 1D earth model to the Cake Phase dropdown as
    /// `GF Store: <id>`.
    /// Errors (unsupported browser, not a store) surface in the snuffling text window.
    #[cfg(feature = "gf")]
    pub fn add_gf_store(&self) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::MountGfStore,
            None,
            Vec::new(),
            async move {
                let job =
                    lane_owner(Operation::MountGfStore, "addGfStore").map_err(CallError::from)?;
                let value = js_add_gf_store(job.id().get()).await.map_err(|error| {
                    BridgeError::browser("adding a GF store", js_value_to_string(error))
                })?;
                if let Some(name) = value.as_string()
                    && !name.is_empty()
                {
                    inner.borrow_mut().pending_cake_model = Some(name);
                }
                Ok(())
            },
        )
    }
}
