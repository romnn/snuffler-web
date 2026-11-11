//! The raster basemaps on offer, and everything that varies per provider.

use egui::Color32;
use strum::EnumIter;

use super::overlay::LabelStyle;

/// The raster basemaps on offer.
///
/// An enum rather than an index into a table, so that everything which varies per
/// provider is one exhaustive `match` away: the tiles, the credit, and the colour
/// of ink that stays legible on top of them.
/// Adding a provider cannot compile until each of those has an answer.
/// `EnumIter` extends that to the picker: a hand-written array of the variants
/// would keep compiling at its old length and quietly leave the new basemap
/// unofferable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter)]
pub(super) enum Basemap {
    OpenStreetMap,
    CartoLight,
    CartoDark,
    Satellite,
}

impl Basemap {
    /// The name shown in the picker.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::OpenStreetMap => "OpenStreetMap",
            Self::CartoLight => "Carto light",
            Self::CartoDark => "Carto dark",
            Self::Satellite => "Satellite",
        }
    }

    /// Tile URL template.
    ///
    /// `{z}/{x}/{y}` are substituted; ESRI orders them `{z}/{y}/{x}`.
    fn url_template(self) -> &'static str {
        match self {
            Self::OpenStreetMap => "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            Self::CartoLight => "https://a.basemaps.cartocdn.com/light_all/{z}/{x}/{y}.png",
            Self::CartoDark => "https://a.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}.png",
            Self::Satellite => {
                "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"
            }
        }
    }

    /// The credit this provider's terms of use ask for.
    pub(super) fn attribution(self) -> &'static str {
        match self {
            Self::OpenStreetMap => "© OpenStreetMap contributors",
            Self::CartoLight | Self::CartoDark => "© OpenStreetMap contributors, © CARTO",
            Self::Satellite => "Esri, Maxar, Earthstar Geographics",
        }
    }

    /// How to paint labels straight onto this basemap's tiles, with no plate
    /// behind them.
    ///
    /// The pale basemaps need dark ink and the dark one needs light ink.
    /// No single colour reads on both, which is why this is a property of the
    /// provider rather than a constant.
    /// Satellite imagery does not settle for either: snow, ocean, desert and forest
    /// can pass under one label, so its glyphs carry their own halo instead of
    /// trusting the ground.
    pub(super) fn label_style(self) -> LabelStyle {
        match self {
            Self::OpenStreetMap | Self::CartoLight => LabelStyle {
                ink: Color32::BLACK,
                halo: None,
            },
            Self::CartoDark => LabelStyle {
                ink: Color32::WHITE,
                halo: None,
            },
            Self::Satellite => LabelStyle {
                ink: Color32::WHITE,
                halo: Some(Color32::from_black_alpha(200)),
            },
        }
    }

    /// The tile URL for one tile at `zoom`, column `x` and row `y`.
    pub(super) fn tile_url(self, zoom: u8, x: u32, y: u32) -> String {
        self.url_template()
            .replace("{z}", &zoom.to_string())
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string())
    }
}
