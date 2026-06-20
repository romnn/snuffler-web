pub use snuffler_proto::bridge::v1 as pb;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::pb;
    use snuffler_proto::prost::Message;

    use crate::bridge::{
        CakeDiscontinuity, CakeLayer, CakeLegendEntry, CakeModelPlot, CakePoint, CakeRay,
        CakeRaysPlot, MapData, MapEvent, MapStation, SceneMeta, SnufMarker, SnufflingRequestParams,
        SnufflingResult, SpecMeta, SpecTrack, SpectrumData, TraceMeta, ViewData, ViewMeta,
        ViewParams, ViewTrack,
    };

    use crate::bridge::CommandResult;

    pub fn encode_message<M: Message>(msg: &M) -> Vec<u8> {
        let mut out = Vec::with_capacity(msg.encoded_len());
        if msg.encode(&mut out).is_err() {
            return Vec::new();
        }
        out
    }

    pub fn decode_message<M: Message + Default>(bytes: &[u8], what: &str) -> Result<M, String> {
        M::decode(bytes).map_err(|e| format!("{what}: {e}"))
    }

    pub fn scene_from_bytes(bytes: &[u8], what: &str) -> Result<SceneMeta, String> {
        decode_message::<pb::SceneMeta>(bytes, what).map(scene_from_proto)
    }

    pub fn view_data_from_parts(meta_bytes: &[u8], ys_bytes: &[u8]) -> Result<ViewData, String> {
        Ok(ViewData {
            meta: decode_message::<pb::ViewMeta>(meta_bytes, "view.meta")
                .map(view_meta_from_proto)?,
            ys: f32_vec_from_le_bytes(ys_bytes, "view.ys")?,
        })
    }

    pub fn spectrum_data_from_parts(
        meta_bytes: &[u8],
        ys_bytes: &[u8],
    ) -> Result<SpectrumData, String> {
        Ok(SpectrumData {
            meta: decode_message::<pb::SpecMeta>(meta_bytes, "spectrum.meta")
                .map(spec_meta_from_proto)?,
            ys: f32_vec_from_le_bytes(ys_bytes, "spectrum.ys")?,
        })
    }

    pub fn scene_from_proto(msg: pb::SceneMeta) -> SceneMeta {
        SceneMeta {
            tmin: msg.tmin,
            tmax: msg.tmax,
            tracks: msg
                .tracks
                .into_iter()
                .map(|t| TraceMeta {
                    net: t.net,
                    sta: t.sta,
                    loc: t.loc,
                    cha: t.cha,
                    label: t.label,
                    tmin: t.tmin,
                    tmax: t.tmax,
                    deltat: t.deltat,
                    nsamples: usize_from_u64(t.nsamples),
                })
                .collect(),
        }
    }

    pub fn snuffling_result_from_proto(msg: pb::SnufflingResult) -> SnufflingResult {
        SnufflingResult {
            show_window: msg.show_window,
            text: msg.text,
            markers: msg.markers.into_iter().map(marker_from_proto).collect(),
            scene: msg.scene.map(scene_from_proto),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn command_from_proto(msg: pb::CommandResult) -> CommandResult {
        CommandResult {
            action: msg.action,
            t: msg.t,
            scene: msg.scene.map(scene_from_proto),
            message: msg.message,
        }
    }

    pub fn map_data_from_proto(msg: pb::MapData) -> MapData {
        MapData {
            stations: msg
                .stations
                .into_iter()
                .map(|s| MapStation {
                    label: s.label,
                    lat: s.lat,
                    lon: s.lon,
                })
                .collect(),
            events: msg
                .events
                .into_iter()
                .map(|e| MapEvent {
                    lat: e.lat,
                    lon: e.lon,
                    mag: e.mag,
                })
                .collect(),
        }
    }

    pub fn cake_model_from_proto(msg: pb::CakeModelPlot) -> CakeModelPlot {
        CakeModelPlot {
            title: msg.title,
            vp: msg.vp,
            vs: msg.vs,
            discontinuities: msg
                .discontinuities
                .into_iter()
                .map(cake_disc_from_proto)
                .collect(),
            layers: msg.layers.into_iter().map(cake_layer_from_proto).collect(),
            vmax_km: msg.vmax_km,
            zmax_km: msg.zmax_km,
        }
    }

    pub fn cake_rays_from_proto(msg: pb::CakeRaysPlot) -> CakeRaysPlot {
        CakeRaysPlot {
            title: msg.title,
            rays: msg
                .rays
                .into_iter()
                .map(|r| CakeRay {
                    color: u8_from_u32(r.color),
                    segs: r.segs.into_iter().map(|s| s.values).collect(),
                })
                .collect(),
            discontinuities: msg
                .discontinuities
                .into_iter()
                .map(cake_disc_from_proto)
                .collect(),
            layers: msg.layers.into_iter().map(cake_layer_from_proto).collect(),
            source: cake_point_from_proto(msg.source),
            receivers: msg
                .receivers
                .into_iter()
                .map(|p| cake_point_from_proto(Some(p)))
                .collect(),
            legend: msg
                .legend
                .into_iter()
                .map(|l| CakeLegendEntry {
                    phase: l.phase,
                    color: u8_from_u32(l.color),
                })
                .collect(),
            xmin_km: msg.xmin_km,
            xmax_km: msg.xmax_km,
            zmax_km: msg.zmax_km,
        }
    }

    pub fn marker_from_proto(msg: pb::SnufMarker) -> SnufMarker {
        SnufMarker {
            t: msg.t,
            tmax: msg.tmax,
            phase: msg.phase,
            kind: u8_from_u32(msg.kind),
            is_event: msg.is_event,
            nslc: nslc_from_proto(msg.nslc),
            polarity: msg.polarity.map(i8_from_i32),
        }
    }

    pub fn view_params_to_proto(params: &ViewParams) -> pb::ViewParams {
        pb::ViewParams {
            tmin: params.tmin,
            tmax: params.tmax,
            npx: params.npx,
            highpass: params.highpass,
            lowpass: params.lowpass,
            sort: params.sort.clone(),
            subsort: params.subsort.clone(),
            rotate: params.rotate,
            remove_response: params.remove_response,
            mode: params.mode.clone(),
            demean: params.demean,
            degap: params.degap,
            allow_downsampling: params.allow_downsampling,
            fft_filtering: params.fft_filtering,
            bandpass_lphp: params.bandpass_lphp,
            distances_3d: params.distances_3d,
            scaling_base: params.scaling_base.clone(),
            scaling_group: params.scaling_group.clone(),
            wf_abs: params.wf_abs,
            wf_integrate: params.wf_integrate,
            wf_color_scale: params.wf_color_scale.clone(),
            track_offset: u64_from_usize(params.track_offset),
            track_count: u64_from_usize(params.track_count),
        }
    }

    pub fn snuffling_request_to_proto(
        params: &SnufflingRequestParams,
    ) -> pb::SnufflingRequestParams {
        let sn = &params.snufflings;
        pb::SnufflingRequestParams {
            view: Some(view_params_to_proto(&params.view)),
            snufflings: Some(pb::SnufflingParams {
                spectrum: Some(pb::SpectrumSnufflingParams {
                    spec_smoothing: sn.spectrum.spec_smoothing,
                    spec_smoothing_bandwidth: sn.spectrum.spec_smoothing_bandwidth,
                }),
                stalta: Some(pb::StaltaSnufflingParams {
                    stalta_highpass: sn.stalta.stalta_highpass,
                    stalta_lowpass: sn.stalta.stalta_lowpass,
                    stalta_swin: sn.stalta.stalta_swin,
                    stalta_ratio: sn.stalta.stalta_ratio,
                    stalta_level: sn.stalta.stalta_level,
                    stalta_show_level_traces: sn.stalta.stalta_show_level_traces,
                    stalta_variant: sn.stalta.stalta_variant.clone(),
                    stalta_scalingmethod: sn.stalta.stalta_scalingmethod.clone(),
                    stalta_apply_to_sum: sn.stalta.stalta_apply_to_sum,
                }),
                polarization: Some(pb::PolarizationSnufflingParams {
                    polarization_t_length: sn.polarization.polarization_t_length,
                    polarization_ft_offset: sn.polarization.polarization_ft_offset,
                    polarization_azimuth: sn.polarization.polarization_azimuth,
                    polarization_highpass: sn.polarization.polarization_highpass,
                    polarization_lowpass: sn.polarization.polarization_lowpass,
                    polarization_dot_position: sn.polarization.polarization_dot_position,
                    polarization_rotate_to_rt: sn.polarization.polarization_rotate_to_rt,
                    polarization_show_eigensystem_2d: sn
                        .polarization
                        .polarization_show_eigensystem_2d,
                    polarization_show_eigensystem_3d: sn
                        .polarization
                        .polarization_show_eigensystem_3d,
                    polarization_fix_scale: sn.polarization.polarization_fix_scale,
                }),
                rms: Some(pb::RmsSnufflingParams {
                    rms_highpass: sn.rms.rms_highpass,
                    rms_lowpass: sn.rms.rms_lowpass,
                    rms_block: sn.rms.rms_block,
                    rms_group: sn.rms.rms_group,
                    rms_log: sn.rms.rms_log,
                }),
                cake: Some(pb::CakeSnufflingParams {
                    cake_model: sn.cake.cake_model.clone(),
                    cake_phases: sn.cake.cake_phases.clone(),
                    cake_tshift: sn.cake.cake_tshift,
                    cake_use_station_depth: sn.cake.cake_use_station_depth,
                    cake_mode: sn.cake.cake_mode.clone(),
                }),
            }),
        }
    }

    pub fn encode_string_request(value: String) -> Vec<u8> {
        encode_message(&pb::StringRequest { value })
    }

    fn view_meta_from_proto(msg: pb::ViewMeta) -> ViewMeta {
        ViewMeta {
            view_tmin: msg.view_tmin,
            view_tmax: msg.view_tmax,
            tracks: msg
                .tracks
                .into_iter()
                .map(|t| ViewTrack {
                    net: t.net,
                    sta: t.sta,
                    loc: t.loc,
                    cha: t.cha,
                    label: t.label,
                    t0_rel: t.t0_rel,
                    dt: t.dt,
                    count: usize_from_u64(t.count),
                    offset: usize_from_u64(t.offset),
                    ymin: t.ymin,
                    ymax: t.ymax,
                    ymean: t.ymean,
                    yscale: t.yscale,
                    nf: usize_from_u64(t.nf),
                    nt: usize_from_u64(t.nt),
                    fmax: t.fmax,
                })
                .collect(),
        }
    }

    fn spec_meta_from_proto(msg: pb::SpecMeta) -> SpecMeta {
        SpecMeta {
            fmin: msg.fmin,
            fmax: msg.fmax,
            tracks: msg
                .tracks
                .into_iter()
                .map(|t| SpecTrack {
                    label: t.label,
                    offset: usize_from_u64(t.offset),
                    count: usize_from_u64(t.count),
                    ymin: t.ymin,
                    ymax: t.ymax,
                })
                .collect(),
        }
    }

    fn nslc_from_proto(msg: Option<pb::Nslc>) -> Option<(String, String, String, String)> {
        msg.map(|n| (n.net, n.sta, n.loc, n.cha))
    }

    fn cake_disc_from_proto(msg: pb::CakeDiscontinuity) -> CakeDiscontinuity {
        CakeDiscontinuity {
            z_km: msg.z_km,
            name: msg.name,
        }
    }

    fn cake_layer_from_proto(msg: pb::CakeLayer) -> CakeLayer {
        CakeLayer {
            ztop_km: msg.ztop_km,
            zbot_km: msg.zbot_km,
            water: msg.water,
            gradient: msg.gradient,
        }
    }

    fn cake_point_from_proto(msg: Option<pb::CakePoint>) -> CakePoint {
        msg.map_or_else(CakePoint::default, |p| CakePoint {
            x_km: p.x_km,
            z_km: p.z_km,
        })
    }

    fn f32_vec_from_le_bytes(bytes: &[u8], what: &str) -> Result<Vec<f32>, String> {
        let mut chunks = bytes.chunks_exact(4);
        let mut out = Vec::with_capacity(bytes.len() / 4);
        for chunk in &mut chunks {
            let arr: [u8; 4] = chunk
                .try_into()
                .map_err(|_| format!("{what}: invalid f32 chunk"))?;
            out.push(f32::from_le_bytes(arr));
        }
        if !chunks.remainder().is_empty() {
            return Err(format!("{what}: byte length is not divisible by four"));
        }
        Ok(out)
    }

    fn usize_from_u64(value: u64) -> usize {
        usize::try_from(value).unwrap_or(usize::MAX)
    }

    fn u64_from_usize(value: usize) -> u64 {
        u64::try_from(value).unwrap_or(u64::MAX)
    }

    fn u8_from_u32(value: u32) -> u8 {
        u8::try_from(value).unwrap_or(u8::MAX)
    }

    fn i8_from_i32(value: i32) -> i8 {
        i8::try_from(value).unwrap_or(if value.is_negative() {
            i8::MIN
        } else {
            i8::MAX
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::*;
