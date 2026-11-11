//! Operations that replace or extend the pile: examples, bundles, dropped files,
//! URLs and FDSN downloads.

use super::*;

impl Bridge {
    /// Replaces the current pile with the bundled synthetic example.
    pub fn load_example(&self) -> Submission {
        retire_live_session(&self.inner);
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::MakeExample,
            None,
            Vec::new(),
            async move {
                let scene = client().make_example().await?;
                let mut state = inner.borrow_mut();
                state.clear_event_state();
                state.replace_scene(scene);
                Ok(())
            },
        )
    }

    /// Empties the pile and the event context (`File ▸ Close all`), leaving
    /// the blank viewer Qt starts with when given no arguments.
    pub fn empty_scene(&self) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::EmptyScene,
            None,
            Vec::new(),
            async move {
                let scene = client().empty_scene().await?;
                retire_live_session(&inner);
                js_clear_open_sources();
                let mut state = inner.borrow_mut();
                state.diagnostic = None;
                state.snuffling = None;
                state.clear_event_state();
                state.replace_scene(scene);
                Ok(())
            },
        )
    }

    /// Appends waveform bytes supplied by the frontend to the pile.
    ///
    /// Snuffler's `open_waveforms` appends too (`pile_viewer.py:1651`); the boot
    /// example is dropped first by the bridge's example-pile rule.
    pub fn load_file(&self, name: String, data: Vec<u8>, format: String) -> Submission {
        // A dropped file may be a whole dataset.
        // Deciding on the bytes rather than the extension means a bundle behaves the same
        // however it arrives: dropped, picked, or fetched from the Examples menu.
        if data.starts_with(ZIP_MAGIC) {
            return self.load_bundle_bytes(name, data);
        }
        let inner = self.inner.clone();
        let detail = Some(name.clone());
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadBytes,
            detail,
            Vec::new(),
            async move {
                let result = client()
                    .load_bytes(pb::LoadBytesRequest {
                        name,
                        format,
                        append: true,
                        content: data,
                        source_id: String::new(),
                        replace_source: false,
                    })
                    .await?;
                inner.borrow_mut().install_waveform_result(result);
                Ok(())
            },
        )
    }

    /// Replaces the current pile with a waveform fetched from a URL.
    pub fn load_url(&self, url: String, format: String) -> Submission {
        retire_live_session(&self.inner);
        let inner = self.inner.clone();
        let detail = Some(url.clone());
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadUrl,
            detail,
            Vec::new(),
            async move {
                let scene = client()
                    .load_url(pb::LoadUrlRequest { url, format })
                    .await?;
                js_clear_open_sources();
                let mut state = inner.borrow_mut();
                state.clear_event_state();
                state.replace_scene(scene);
                Ok(())
            },
        )
    }

    /// Downloads waveforms from an FDSN dataselect service and appends them to the pile
    /// (browser fetch; pyrocko parses).
    pub fn download_waveforms(&self, params: WaveformDownloadRequest) -> Submission {
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::DownloadWaveforms,
            None,
            Vec::new(),
            async move {
                let scene = client().download_waveforms(params).await?;
                inner.borrow_mut().replace_scene(scene);
                Ok(())
            },
        )
    }

    /// Fetch the example index, once.
    ///
    /// The menu calls this on every frame it is open, so "once" has to mean once: a
    /// *failed* fetch is remembered too, or a datacenter that is down would be retried at
    /// the frame rate.
    /// `force` is the user asking again, from the retry the menu offers in its place.
    #[must_use]
    pub fn list_examples(&self, force: bool) -> Option<Submission> {
        {
            let state = self.inner.borrow();
            if let Some(job) = state.examples_job {
                return Some(Submission::Joined(job));
            }
            if state.examples.is_some() && !force {
                return None;
            }
        }
        let inner = self.inner.clone();
        let submission = submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ListExamples,
            None,
            Vec::new(),
            async move {
                let listed = client().list_examples().await;
                let mut state = inner.borrow_mut();
                state.examples_job = None;
                match listed {
                    Ok(list) => {
                        state.examples = Some(Ok(list.examples));
                        Ok(())
                    }
                    Err(error) => {
                        let error = BridgeError::from(error);
                        state.examples = Some(Err(error.clone()));
                        Err(error)
                    }
                }
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            self.inner.borrow_mut().examples_job = Some(handle);
        }
        Some(submission)
    }

    /// Load an example by slug, fetching the index first if it has not been read yet.
    /// This is what `?example=<slug>` resolves to: a link someone can hand out that opens
    /// the app on a named dataset.
    ///
    /// An unknown slug is an error the user has to see, since they followed a link
    /// that promised data, so it lands in the status line rather than only in the
    /// console.
    pub fn open_example(&self, slug: String) -> Submission {
        let bridge = self.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::ListExamples,
            Some(slug.clone()),
            slug.as_bytes().to_vec(),
            async move {
                let known = bridge.inner.borrow().examples.clone();
                let listed = match known {
                    Some(Ok(examples)) => examples,
                    _ => client().list_examples().await?.examples,
                };
                let found = listed.iter().find(|example| example.slug == slug).cloned();
                bridge.inner.borrow_mut().examples = Some(Ok(listed));
                let example = found.ok_or(BridgeError::UnknownExample { slug })?;
                let _submission = bridge.load_bundle(example.url, example.sha256, example.title);
                Ok(())
            },
        )
    }

    /// Replaces the current pile with a whole dataset fetched as one archive:
    /// waveforms, station metadata and an event catalogue in a single request.
    ///
    /// The station and event results come back with the scene and are applied
    /// through the same path a separate metadata load would use, so the event
    /// markers appear exactly as if the catalogue had been opened by hand.
    pub fn load_bundle(&self, url: String, sha256: String, title: String) -> Submission {
        let request = pb::LoadBundleRequest {
            url,
            sha256,
            content: Vec::new(),
            name: title.clone(),
            marker_budget: Some(standard_marker_budget()),
        };
        self.submit_bundle_load(request, title)
    }

    /// The same load for an archive that is already in hand (dropped on the
    /// window, or picked from disk) rather than fetched.
    fn load_bundle_bytes(&self, name: String, data: Vec<u8>) -> Submission {
        let request = pb::LoadBundleRequest {
            url: String::new(),
            sha256: String::new(),
            content: data,
            name: name.clone(),
            marker_budget: Some(standard_marker_budget()),
        };
        self.submit_bundle_load(request, name)
    }

    fn submit_bundle_load(&self, request: pb::LoadBundleRequest, label: String) -> Submission {
        retire_live_session(&self.inner);
        let inner = self.inner.clone();
        submit_job(
            self.inner.clone(),
            self.repaint.clone(),
            Operation::LoadBundle,
            Some(label),
            Vec::new(),
            async move {
                let bundle = client().load_bundle(request).await?;
                js_clear_open_sources();
                inner.borrow_mut().install_bundle_result(bundle);
                Ok(())
            },
        )
    }
}
