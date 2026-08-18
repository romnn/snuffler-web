//! Renders Tier 1 web waveform artifacts from the exact oracle payload.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, Context as _, bail};
use emath::{Pos2, Rect, Vec2, vec2};
use prost::Message as _;
use rayon::prelude::*;
use resvg::{tiny_skia, usvg};
use snuffler_web_core::bridge::{ViewData, ViewMeta, ViewParams};
use snuffler_web_core::state::{AppState, ScalingBase, ScalingGroup, Subsort};
use snuffler_web_render::draw::{SvgTarget, encode_png_rgba};
use snuffler_web_render::layout::compute_layout;
use snuffler_web_render::paint::paint;
use snuffler_web_render::palette::{Theme, palette};

const WIDTH: f32 = 1400.0;
const HEIGHT: f32 = 900.0;
const EXPORT_SCALE: f32 = 2.0;
const UBUNTU_REGULAR: &[u8] =
    include_bytes!("../../snuffler-web-egui/assets/fonts/Ubuntu-Regular.ttf");

fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    let mut args = env::args_os().skip(1);
    let input_root = required_path(args.next(), "oracle input directory")?;
    let output_root = required_path(args.next(), "artifact output directory")?;
    let scenarios: Vec<_> = args.map(PathBuf::from).collect();
    if scenarios.is_empty() {
        bail!("at least one scenario name is required");
    }

    scenarios
        .par_iter()
        .try_for_each(|scenario| render_scenario(&input_root, &output_root, scenario))?;
    Ok(())
}

fn required_path(value: Option<std::ffi::OsString>, description: &str) -> eyre::Result<PathBuf> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| eyre::eyre!("missing {description}"))
}

fn render_scenario(input_root: &Path, output_root: &Path, scenario: &Path) -> eyre::Result<()> {
    let source = input_root.join(scenario);
    let params = ViewParams::decode(read(&source.join("params.pb"))?.as_slice())
        .wrap_err_with(|| format!("decode parameters for {}", scenario.display()))?;
    let meta = ViewMeta::decode(read(&source.join("view.pb"))?.as_slice())
        .wrap_err_with(|| format!("decode view for {}", scenario.display()))?;
    let ys = decode_f32(&read(&source.join("ys.f32"))?)?;
    let view = ViewData { meta, ys };
    let state = state_from_params(&params)?;

    let theme = Theme::Light;
    let colors = palette(theme, state.wiggle_theme);
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, HEIGHT));
    let layout = compute_layout(&state, &view, rect);
    let mut target = SvgTarget::new(WIDTH, HEIGHT, HashMap::new(), approximate_text_size);
    paint(
        &mut target,
        &state,
        &view,
        &colors,
        &layout,
        theme,
        false,
        true,
    );

    let destination = output_root.join(scenario);
    fs::create_dir_all(&destination)
        .wrap_err_with(|| format!("create {}", destination.display()))?;
    let png = rasterize_svg(&target.finish())?;
    fs::write(destination.join("web-waveforms.png"), png)
        .wrap_err_with(|| format!("write artifact for {}", scenario.display()))?;
    Ok(())
}

fn read(path: &Path) -> eyre::Result<Vec<u8>> {
    fs::read(path).wrap_err_with(|| format!("read {}", path.display()))
}

fn decode_f32(bytes: &[u8]) -> eyre::Result<Vec<f32>> {
    let mut chunks = bytes.chunks_exact(4);
    let values = chunks
        .by_ref()
        .map(|chunk| {
            let raw: [u8; 4] = chunk.try_into()?;
            Ok(f32::from_le_bytes(raw))
        })
        .collect::<Result<Vec<_>, std::array::TryFromSliceError>>()?;
    if !chunks.remainder().is_empty() {
        bail!(
            "waveform payload length {} is not a multiple of four",
            bytes.len()
        );
    }
    Ok(values)
}

fn state_from_params(params: &ViewParams) -> eyre::Result<AppState> {
    let scaling_group = ScalingGroup::from_wire(&params.scaling_group)
        .ok_or_else(|| eyre::eyre!("unknown scaling group {:?}", params.scaling_group))?;
    let scaling_base = ScalingBase::from_wire(&params.scaling_base)
        .ok_or_else(|| eyre::eyre!("unknown scaling base {:?}", params.scaling_base))?;
    let subsort = Subsort::from_wire(&params.subsort)
        .ok_or_else(|| eyre::eyre!("unknown subsort {:?}", params.subsort))?;
    if params.mode != "wiggle" {
        bail!("Tier 1 artifact renderer only supports wiggle views");
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "display gain is stored as f32 by AppState and parity scenarios use small finite values"
    )]
    let gain = params.gain as f32;
    Ok(AppState {
        tmin: params.tmin,
        tmax: params.tmax,
        gain,
        gain_text: params.gain.to_string(),
        subsort,
        scaling_group,
        scaling_base,
        show_scale_ranges: params.show_scale_ranges,
        show_scale_axes: params.show_scale_axes,
        ..AppState::default()
    })
}

fn approximate_text_size(text: &str, size: f32) -> Vec2 {
    vec2(text.chars().fold(0.0, |width, _| width + size * 0.55), size)
}

fn rasterize_svg(svg: &str) -> eyre::Result<Vec<u8>> {
    let mut options = usvg::Options {
        font_family: "Ubuntu".to_owned(),
        ..usvg::Options::default()
    };
    {
        let database = options.fontdb_mut();
        database.load_font_data(UBUNTU_REGULAR.to_vec());
        database.set_sans_serif_family("Ubuntu");
    }
    let tree = usvg::Tree::from_str(svg, &options).wrap_err("parse rendered SVG")?;
    let width = (WIDTH * EXPORT_SCALE).round();
    let height = (HEIGHT * EXPORT_SCALE).round();

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "positive fixed canvas dimensions fit exactly in u32"
    )]
    let mut pixmap = tiny_skia::Pixmap::new(width as u32, height as u32)
        .ok_or_else(|| eyre::eyre!("allocate {width}x{height} artifact pixmap"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(EXPORT_SCALE, EXPORT_SCALE),
        &mut pixmap.as_mut(),
    );
    encode_png_rgba(pixmap.width(), pixmap.height(), pixmap.data())
        .ok_or_else(|| eyre::eyre!("encode rendered artifact as PNG"))
}
