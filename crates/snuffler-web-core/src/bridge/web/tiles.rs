//! Fetching basemap tiles: the browser fetches and decodes each batch, and every
//! tile is filed into the cache as it lands.

use super::*;

impl Bridge {
    /// Submit every tile the map wants on screen right now.
    ///
    /// This replaces the pending queue rather than adding to it, which is what makes a
    /// zoom or a pan cheap: tiles the view has moved away from and that no batch has
    /// started yet are simply dropped, and the survivors are re-ordered by the priority
    /// the view just scored them with.
    /// Tiles already fetched, in flight, or known to have failed are skipped, so calling
    /// this every frame costs a filter over a screenful of keys.
    pub fn request_tiles(&self, wanted: Vec<TileRequest>) {
        {
            let mut state = self.inner.borrow_mut();
            let mut queue: Vec<TileRequest> = wanted
                .into_iter()
                .filter(|tile| !state.tiles.states.contains_key(&TileKey::from(tile)))
                .collect();
            queue.sort_by_key(|tile| tile.priority);
            state.tiles.queue = queue;
        }
        self.start_tile_batches();
    }

    /// Start batches until the queue empties or the in-flight cap is reached.
    fn start_tile_batches(&self) {
        loop {
            let batch = {
                let mut state = self.inner.borrow_mut();
                let tiles = &mut state.tiles;
                if tiles.batches_in_flight >= TILE_BATCHES_IN_FLIGHT || tiles.queue.is_empty() {
                    return;
                }
                let batch_size = tiles.queue.len().min(TILE_BATCH_SIZE);
                let batch: Vec<TileRequest> = tiles.queue.drain(..batch_size).collect();
                for tile in &batch {
                    tiles
                        .states
                        .insert(TileKey::from(tile), TileState::InFlight);
                }
                tiles.batches_in_flight += 1;
                batch
            };
            self.spawn_tile_batch(batch);
        }
    }

    /// Fetch and decode one batch in the browser, filing each tile as it lands.
    fn spawn_tile_batch(&self, batch: Vec<TileRequest>) {
        let bridge = self.clone();
        let epoch = self.inner.borrow().tiles.epoch;
        let keys: Vec<TileKey> = batch.iter().map(TileKey::from).collect();
        let urls = js_sys::Array::new();
        for tile in &batch {
            urls.push(&JsValue::from_str(&tile.url));
        }

        #[expect(
            clippy::disallowed_methods,
            reason = "the bounded tile-batch counter owns this deferred browser fetch"
        )]
        spawn_local(async move {
            let on_tile = tile_landed_callback(&bridge, keys.clone(), epoch);
            let delivered = js_fetch_tiles(
                urls,
                on_tile.as_ref().unchecked_ref::<js_sys::Function>().clone(),
            )
            .await;
            // The callback must outlive the fetch, which calls it until it resolves.
            drop(on_tile);
            {
                let mut state = bridge.inner.borrow_mut();
                let tiles = &mut state.tiles;
                tiles.batches_in_flight = tiles.batches_in_flight.saturating_sub(1);
                // The batch as a whole can reject before every callback fired (the module
                // failed to load, say).
                // Anything still marked in-flight has no other way to be retired, and
                // would block its key from ever being requested again.
                if tiles.epoch == epoch && delivered.is_err() {
                    for key in keys {
                        if matches!(tiles.states.get(&key), Some(TileState::InFlight)) {
                            tiles.states.insert(key, TileState::Failed);
                        }
                    }
                }
            }
            bridge.repaint.request_repaint();
            bridge.start_tile_batches();
        });
    }
}

/// The JS callback `fetchTiles` calls once per tile: (index in batch, width, height, RGBA).
fn tile_landed_callback(
    bridge: &Bridge,
    keys: Vec<TileKey>,
    epoch: u64,
) -> Closure<dyn FnMut(f64, f64, f64, js_sys::Uint8Array)> {
    let inner = bridge.inner.clone();
    let repaint = bridge.repaint.clone();
    // Tile dimensions arrive as JS numbers that are small non-negative pixel counts.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "tile width/height are small non-negative integers"
    )]
    Closure::<dyn FnMut(f64, f64, f64, js_sys::Uint8Array)>::new(
        move |index: f64, width: f64, height: f64, rgba: js_sys::Uint8Array| {
            let Some(&key) = keys.get(index as usize) else {
                return;
            };
            let (width, height) = (width as u32, height as u32);
            let rgba = rgba.to_vec();
            let decoded =
                width > 0 && height > 0 && rgba.len() == (width as usize) * (height as usize) * 4;
            {
                let mut state = inner.borrow_mut();
                // The basemap changed while this tile was in flight; it now holds the
                // wrong provider's imagery for its key.
                if state.tiles.epoch != epoch {
                    return;
                }
                let tile = if decoded {
                    TileState::Ready {
                        width,
                        height,
                        rgba,
                    }
                } else {
                    TileState::Failed
                };
                state.tiles.states.insert(key, tile);
            }
            repaint.request_repaint();
        },
    )
}
