#![allow(warnings)]

use color_eyre::eyre::{self, WrapErr};
use std::path::{PathBuf, Path};
use std::collections::{HashMap, BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use once_cell::sync::Lazy;


/// Target triples for Linux.
pub static LINUX_TARGET_TRIPLES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
    ]
});

/// Target triples for macOS.
pub static MACOS_TARGET_TRIPLES: Lazy<Vec<&'static str>> =
    Lazy::new(|| vec!["aarch64-apple-darwin", "x86_64-apple-darwin"]);

/// Target triples for Windows.
pub static WINDOWS_TARGET_TRIPLES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        "i686-pc-windows-gnu",
        "i686-pc-windows-msvc",
        "x86_64-pc-windows-gnu",
        "x86_64-pc-windows-msvc",
    ]
});

/// Distribution extensions with known problems on Linux.
///
/// These will never be packaged.
pub static BROKEN_EXTENSIONS_LINUX: Lazy<Vec<String>> = Lazy::new(|| {
    vec![
        // Linking issues.
        "_crypt".to_string(),
        // Linking issues.
        "nis".to_string(),
    ]
});

/// Distribution extensions with known problems on macOS.
///
/// These will never be packaged.
pub static BROKEN_EXTENSIONS_MACOS: Lazy<Vec<String>> = Lazy::new(|| {
    vec![
        // curses and readline have linking issues.
        "curses".to_string(),
        "_curses_panel".to_string(),
        "readline".to_string(),
    ]
});

/// Python modules that we shouldn't generate bytecode for by default.
///
/// These are Python modules in the standard library that don't have valid bytecode.
pub static NO_BYTECODE_MODULES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        "lib2to3.tests.data.bom",
        "lib2to3.tests.data.crlf",
        "lib2to3.tests.data.different_encoding",
        "lib2to3.tests.data.false_encoding",
        "lib2to3.tests.data.py2_test_grammar",
        "lib2to3.tests.data.py3_test_grammar",
        "test.bad_coding",
        "test.badsyntax_3131",
        "test.badsyntax_future3",
        "test.badsyntax_future4",
        "test.badsyntax_future5",
        "test.badsyntax_future6",
        "test.badsyntax_future7",
        "test.badsyntax_future8",
        "test.badsyntax_future9",
        "test.badsyntax_future10",
        "test.badsyntax_pep3120",
    ]
});

/// Represents a software component with licensing information.
#[derive(Clone, Debug)]
pub struct LicensedComponent {
    /// Type of component.
    flavor: ComponentFlavor,

    // /// The type of license.
    // license: LicenseFlavor,
    //
    // /// Location where source code for this component can be obtained.
    // source_location: SourceLocation,

    /// Homepage for project.
    homepage: Option<String>,

    /// List of authors.
    authors: Vec<String>,

    /// Specified license text for this component.
    ///
    /// If empty, license texts will be derived from SPDX identifiers, if available.
    license_texts: Vec<String>,
}

// impl PartialEq for LicensedComponent {
//     fn eq(&self, other: &Self) -> bool {
//         self.flavor.eq(&other.flavor)
//     }
// }
//
// impl Eq for LicensedComponent {}

// impl PartialOrd for LicensedComponent {
//     fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
//         self.flavor.partial_cmp(&other.flavor)
//     }
// }
//
// impl Ord for LicensedComponent {
//     fn cmp(&self, other: &Self) -> Ordering {
//         self.flavor.cmp(&other.flavor)
//     }
// }

// impl LicensedComponent {
//     /// Construct a new instance from parameters.
//     pub fn new(flavor: ComponentFlavor, license: LicenseFlavor) -> Self {
//         Self {
//             flavor,
//             // license,
//             // source_location: SourceLocation::NotSet,
//             homepage: None,
//             authors: vec![],
//             license_texts: vec![],
//         }
//     }
//
//     /// Construct a new instance from an SPDX expression.
//     pub fn new_spdx(flavor: ComponentFlavor, spdx_expression: &str) -> eyre::Result<Self> {
//         let spdx_expression = Expression::parse(spdx_expression).map_err(|e| eyre::bail!("{}", e))?;
//
//         let license = if spdx_expression.evaluate(|req| req.license.id().is_some()) {
//             LicenseFlavor::Spdx(spdx_expression)
//         } else {
//             LicenseFlavor::OtherExpression(spdx_expression)
//         };
//
//         Ok(Self::new(flavor, license))
//     }
// }

/// Describes the type of a software component.
#[derive(Clone, Debug)]
pub enum ComponentFlavor {
    /// A Python distribution.
    PythonDistribution(String),
    /// A Python module in the standard library.
    PythonStandardLibraryModule(String),
    /// A compiled Python extension module in the standard library.
    PythonStandardLibraryExtensionModule(String),
    /// A compiled Python extension module.
    PythonExtensionModule(String),
    /// A Python module.
    PythonModule(String),
    /// A generic software library.
    Library(String),
    /// A Rust crate.
    RustCrate(String),
}

impl std::fmt::Display for ComponentFlavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PythonDistribution(name) => f.write_str(name),
            Self::PythonStandardLibraryModule(name) => {
                f.write_fmt(format_args!("Python stdlib module {}", name))
            }
            Self::PythonStandardLibraryExtensionModule(name) => {
                f.write_fmt(format_args!("Python stdlib extension {}", name))
            }
            Self::PythonExtensionModule(name) => {
                f.write_fmt(format_args!("Python extension module {}", name))
            }
            Self::PythonModule(name) => f.write_fmt(format_args!("Python module {}", name)),
            Self::Library(name) => f.write_fmt(format_args!("library {}", name)),
            Self::RustCrate(name) => f.write_fmt(format_args!("Rust crate {}", name)),
        }
    }
}

// impl PartialEq for ComponentFlavor {
//     fn eq(&self, other: &Self) -> bool {
//         // If both entities have a Python module name, equivalence is whether
//         // the module names agree, as there can only be a single entity for a given
//         // module name.
//         match (self.python_module_name(), other.python_module_name()) {
//             (Some(a), Some(b)) => a.eq(b),
//             // Comparing a module with a non-module is always not equivalent.
//             (Some(_), None) => false,
//             (None, Some(_)) => false,
//             (None, None) => match (self, other) {
//                 (Self::PythonDistribution(a), Self::PythonDistribution(b)) => a.eq(b),
//                 (Self::Library(a), Self::Library(b)) => a.eq(b),
//                 (Self::RustCrate(a), Self::RustCrate(b)) => a.eq(b),
//                 _ => false,
//             },
//         }
//     }
// }

// impl Eq for ComponentFlavor {}
//
// impl PartialOrd for ComponentFlavor {
//     fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
//         match (self.python_module_name(), other.python_module_name()) {
//             (Some(a), Some(b)) => a.partial_cmp(b),
//             _ => {
//                 let a = (self.ordinal_value(), self.to_string());
//                 let b = (other.ordinal_value(), other.to_string());
//
//                 a.partial_cmp(&b)
//             }
//         }
//     }
// }
//
// impl Ord for ComponentFlavor {
//     fn cmp(&self, other: &Self) -> std::cmp::Ordering {
//         self.partial_cmp(other).unwrap()
//     }
// }

pub fn walk_tree_files(path: &Path) -> Box<dyn Iterator<Item = walkdir::DirEntry>> {
    let res = walkdir::WalkDir::new(path).sort_by(|a, b| a.file_name().cmp(b.file_name()));

    let filtered = res.into_iter().filter_map(|entry| {
        let entry = entry.expect("unable to get directory entry");

        let path = entry.path();

        if path.is_dir() {
            None
        } else {
            Some(entry)
        }
    });

    Box::new(filtered)
}

// pub fn find_python_resources<'a>(
//     root_path: &Path,
//     cache_tag: &str,
//     suffixes: &PythonModuleSuffixes,
//     emit_files: bool,
//     emit_non_files: bool,
// ) -> eyre::Result<PythonResourceIterator<'a>> {
//     PythonResourceIterator::new(root_path, cache_tag, suffixes, emit_files, emit_non_files)
// }

/// Represents an abstract location for binary data.
///
/// Data can be backed by the filesystem or in memory.
#[derive(Clone, Debug, PartialEq)]
pub enum FileData {
    Path(PathBuf),
    Memory(Vec<u8>),
}


impl FileData {
    /// Resolve the data for this instance.
    ///
    /// If backed by a file, the file will be read.
    pub fn resolve_content(&self) -> Result<Vec<u8>, std::io::Error> {
        match self {
            Self::Path(p) => {
                let data = std::fs::read(p)?;

                Ok(data)
            }
            Self::Memory(data) => Ok(data.clone()),
        }
    }

    /// Convert this instance to a memory variant.
    ///
    /// This ensures any file-backed data is present in memory.
    pub fn to_memory(&self) -> Result<Self, std::io::Error> {
        Ok(Self::Memory(self.resolve_content()?))
    }

    /// Obtain a filesystem path backing this content.
    pub fn backing_path(&self) -> Option<&Path> {
        match self {
            Self::Path(p) => Some(p.as_path()),
            Self::Memory(_) => None,
        }
    }
}

/// Represents a dependency on a library.
///
/// The library can be defined a number of ways and multiple variants may be
/// present.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryDependency {
    /// Name of the library.
    ///
    /// This will be used to tell the linker what to link.
    pub name: String,

    /// Static library version of library.
    pub static_library: Option<FileData>,

    /// The filename the static library should be materialized as.
    pub static_filename: Option<PathBuf>,

    /// Shared library version of library.
    pub dynamic_library: Option<FileData>,

    /// The filename the dynamic library should be materialized as.
    pub dynamic_filename: Option<PathBuf>,

    /// Whether this is a system framework (macOS).
    pub framework: bool,

    /// Whether this is a system library.
    pub system: bool,
}

impl LibraryDependency {
    pub fn to_memory(&self) -> eyre::Result<Self> {
        Ok(Self {
            name: self.name.clone(),
            static_library: if let Some(data) = &self.static_library {
                Some(data.to_memory()?)
            } else {
                None
            },
            static_filename: self.static_filename.clone(),
            dynamic_library: if let Some(data) = &self.dynamic_library {
                Some(data.to_memory()?)
            } else {
                None
            },
            dynamic_filename: self.dynamic_filename.clone(),
            framework: self.framework,
            system: self.system,
        })
    }
}

#[derive(Debug, serde::Deserialize)]
struct LinkEntry {
    name: String,
    path_static: Option<String>,
    path_dynamic: Option<String>,
    framework: Option<bool>,
    system: Option<bool>,
}

impl LinkEntry {
    fn to_library_dependency(&self, python_path: &Path) -> LibraryDependency {
        LibraryDependency {
            name: self.name.clone(),
            static_library: self
                .path_static
                .clone()
                .map(|p| FileData::Path(python_path.join(p))),
            static_filename: self
                .path_static
                .as_ref()
                .map(|f| PathBuf::from(PathBuf::from(f).file_name().unwrap())),
            dynamic_library: self
                .path_dynamic
                .clone()
                .map(|p| FileData::Path(python_path.join(p))),
            dynamic_filename: self
                .path_dynamic
                .as_ref()
                .map(|f| PathBuf::from(PathBuf::from(f).file_name().unwrap())),
            framework: self.framework.unwrap_or(false),
            system: self.system.unwrap_or(false),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct PythonBuildExtensionInfo {
    in_core: bool,
    init_fn: String,
    licenses: Option<Vec<String>>,
    license_paths: Option<Vec<String>>,
    license_public_domain: Option<bool>,
    links: Vec<LinkEntry>,
    objs: Vec<String>,
    required: bool,
    static_lib: Option<String>,
    shared_lib: Option<String>,
    variant: String,
}

#[derive(Debug, serde::Deserialize)]
struct PythonBuildCoreInfo {
    objs: Vec<String>,
    links: Vec<LinkEntry>,
    shared_lib: Option<String>,
    static_lib: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct PythonBuildInfo {
    core: PythonBuildCoreInfo,
    extensions: BTreeMap<String, Vec<PythonBuildExtensionInfo>>,
    inittab_object: String,
    inittab_source: String,
    inittab_cflags: Vec<String>,
    object_file_format: String,
}

#[derive(Debug, serde::Deserialize)]
struct PythonJsonMain {
    version: String,
    target_triple: String,
    optimizations: String,
    python_tag: String,
    python_abi_tag: Option<String>,
    python_config_vars: HashMap<String, String>,
    python_platform_tag: String,
    python_implementation_cache_tag: String,
    python_implementation_hex_version: u64,
    python_implementation_name: String,
    python_implementation_version: Vec<String>,
    python_version: String,
    python_major_minor_version: String,
    python_paths: HashMap<String, String>,
    python_paths_abstract: HashMap<String, String>,
    python_exe: String,
    python_stdlib_test_packages: Vec<String>,
    python_suffixes: HashMap<String, Vec<String>>,
    python_bytecode_magic_number: String,
    python_symbol_visibility: String,
    python_extension_module_loading: Vec<String>,
    apple_sdk_canonical_name: Option<String>,
    apple_sdk_platform: Option<String>,
    apple_sdk_version: Option<String>,
    apple_sdk_deployment_target: Option<String>,
    libpython_link_mode: String,
    crt_features: Vec<String>,
    run_tests: String,
    build_info: PythonBuildInfo,
    licenses: Option<Vec<String>>,
    license_path: Option<String>,
    tcl_library_path: Option<String>,
    tcl_library_paths: Option<Vec<String>>,
}

fn parse_python_json(path: &Path) -> eyre::Result<PythonJsonMain> {
    if !path.exists() {
        eyre::bail!("PYTHON.json does not exist; are you using an up-to-date Python distribution that conforms with our requirements?");
    }

    let buf = std::fs::read(path)?;

    let value: serde_json::Value = serde_json::from_slice(&buf)?;
    let o = value
        .as_object()
        .ok_or_else(|| eyre::eyre!("PYTHON.json does not parse to an object"))?;

    match o.get("version") {
        Some(version) => {
            let version = version
                .as_str()
                .ok_or_else(|| eyre::eyre!("unable to parse version as a string"))?;

            if version != "7" {
                eyre::bail!(
                    "expected version 7 standalone distribution; found version {}",
                    version
                );
            }
        }
        None => eyre::bail!("version key not present in PYTHON.json"),
    }

    let v: PythonJsonMain = serde_json::from_slice(&buf)?;

    Ok(v)
}

fn parse_python_json_from_distribution(dist_dir: &Path) -> eyre::Result<PythonJsonMain> {
    let python_json_path = dist_dir.join("python").join("PYTHON.json");
    parse_python_json(&python_json_path)
}

/// Resolve the path to a executable in a Python distribution.
pub fn python_exe_path(dist_dir: &Path) -> eyre::Result<PathBuf> {
    let pi = parse_python_json_from_distribution(dist_dir)?;

    Ok(dist_dir.join("python").join(&pi.python_exe))
}


/// Describes the flavor of a distribution.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, PartialEq, Eq)]
pub enum DistributionFlavor {
    /// Distributions coming from the project.
    Standalone,
    /// Statically linked distributions.
    StandaloneStatic,
    /// Dynamically linked distributions.
    StandaloneDynamic,
}

impl Default for DistributionFlavor {
    fn default() -> Self {
        DistributionFlavor::Standalone
    }
}

impl std::fmt::Display for DistributionFlavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Standalone => "standalone",
            Self::StandaloneStatic => "standalone-static",
            Self::StandaloneDynamic => "standalone-dynamic",
        })
    }
}

impl TryFrom<&str> for DistributionFlavor {
    type Error = eyre::Report;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "standalone" => Ok(Self::Standalone),
            "standalone_static" | "standalone-static" => Ok(Self::StandaloneStatic),
            "standalone_dynamic" | "standalone-dynamic" => Ok(Self::StandaloneDynamic),
            _ => Err(eyre::eyre!("distribution flavor {} not recognized", value)),
        }
    }
}

pub fn canonicalize_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    let mut p = path.canonicalize()?;

    // Strip \\?\ prefix on Windows and replace \ with /, which is valid.
    if cfg!(windows) {
        let mut s = p.display().to_string().replace('\\', "/");
        if s.starts_with("//?/") {
            s = s[4..].to_string();
        }

        p = PathBuf::from(s);
    }

    Ok(p)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyembedPythonInterpreterConfig {
    pub config: PythonInterpreterConfig,
    pub allocator_backend: MemoryAllocatorBackend,
    pub allocator_raw: bool,
    pub allocator_mem: bool,
    pub allocator_obj: bool,
    pub allocator_pymalloc_arena: bool,
    pub allocator_debug: bool,
    pub set_missing_path_configuration: bool,
    pub oxidized_importer: bool,
    pub filesystem_importer: bool,
    // pub packed_resources: Vec<PyembedPackedResourcesSource>,
    pub argvb: bool,
    pub multiprocessing_auto_dispatch: bool,
    // pub multiprocessing_start_method: MultiprocessingStartMethod,
    pub sys_frozen: bool,
    pub sys_meipass: bool,
    // pub terminfo_resolution: TerminfoResolution,
    pub tcl_library: Option<PathBuf>,
    pub write_modules_directory_env: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
// #[cfg_attr(feature = "serialization", derive(Deserialize, Serialize))]
// #[cfg_attr(feature = "serialization", serde(default))]
pub struct PythonInterpreterConfig {
/// Profile to use to initialize pre-config and config state of interpreter.
    pub profile: PythonInterpreterProfile,
    // pub allocator: Option<Allocator>,
    pub configure_locale: Option<bool>,
}

impl Default for PyembedPythonInterpreterConfig {
    fn default() -> Self {
        PyembedPythonInterpreterConfig {
            config: PythonInterpreterConfig {
                profile: PythonInterpreterProfile::Isolated,
                // Isolated mode disables configure_locale by default. But this
                // setting is essential for properly initializing encoding at
                // run-time. Without this, UTF-8 arguments are mangled, for
                // example. See
                // https://github.com/indygreg/PyOxidizer/issues/294 for more.
                configure_locale: Some(true),
                ..PythonInterpreterConfig::default()
            },
            allocator_backend: MemoryAllocatorBackend::Default,
            // This setting has no effect by itself. But the default of true
            // makes it so a custom backend is used automatically.
            allocator_raw: true,
            allocator_mem: false,
            allocator_obj: false,
            allocator_pymalloc_arena: false,
            allocator_debug: false,
            set_missing_path_configuration: true,
            oxidized_importer: true,
            filesystem_importer: false,
            // packed_resources: vec![],
            argvb: false,
            multiprocessing_auto_dispatch: true,
            // multiprocessing_start_method: MultiprocessingStartMethod::Auto,
            sys_frozen: true,
            sys_meipass: false,
            // terminfo_resolution: TerminfoResolution::None,
            tcl_library: None,
            write_modules_directory_env: None,
        }
    }
}

// pub static PYTHON_DISTRIBUTIONS: Lazy<PythonDistributionCollection> = Lazy::new(|| {
//     let dists = vec![
//         // Linux glibc linked.
//         PythonDistributionRecord {
//             python_major_minor_version: "3.8".to_string(),
//             location: PythonDistributionLocation::Url {
//                 url: "https://github.com/indygreg/python-build-standalone/releases/download/20221220/cpython-3.8.16%2B20221220-x86_64-unknown-linux-gnu-pgo-full.tar.zst".to_string(),
//                 sha256: "4e62766abe8a1afefe0b001e476b5e4c6c7457df9e39fefc99dad0bf9bb6648e".to_string(),
//             },
//             target_triple: "x86_64-unknown-linux-gnu".to_string(),
//             supports_prebuilt_extension_modules: true,
//         },
//         // Linux musl.
//         PythonDistributionRecord {
//             python_major_minor_version: "3.8".to_string(),
//             location: PythonDistributionLocation::Url {
//                 url: "https://github.com/indygreg/python-build-standalone/releases/download/20221220/cpython-3.8.16%2B20221220-x86_64-unknown-linux-musl-noopt-full.tar.zst".to_string(),
//                 sha256: "93a517597b419f75f16df7cda2b455c9a17751e4f5e337e04ca36a4c62f942e5".to_string(),
//             },
//             target_triple: "x86_64-unknown-linux-musl".to_string(),
//             supports_prebuilt_extension_modules: false,
//         },

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// #[cfg_attr(feature = "serialization", derive(Deserialize, Serialize))]
// #[cfg_attr(feature = "serialization", serde(try_from = "String", into = "String"))]
pub enum PythonInterpreterProfile {
    Isolated,
    Python,
}

impl Default for PythonInterpreterProfile {
    fn default() -> Self {
        PythonInterpreterProfile::Isolated
    }
}

impl ToString for PythonInterpreterProfile {
    fn to_string(&self) -> String {
        match self {
            Self::Isolated => "isolated",
            Self::Python => "python",
        }
        .to_string()
    }
}

impl From<PythonInterpreterProfile> for String {
    fn from(v: PythonInterpreterProfile) -> Self {
        v.to_string()
    }
}

impl TryFrom<&str> for PythonInterpreterProfile {
    type Error = eyre::Report;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "isolated" => Ok(Self::Isolated),
            "python" => Ok(Self::Python),
            _ => Err(eyre::eyre!(
                "{} is not a valid profile; use 'isolated' or 'python'",
                value
            )),
        }
    }
}

impl TryFrom<String> for PythonInterpreterProfile {
    type Error = eyre::Report;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// #[cfg_attr(feature = "serialization", derive(Deserialize, Serialize))]
// #[cfg_attr(feature = "serialization", serde(try_from = "String", into = "String"))]
pub enum MemoryAllocatorBackend {
    Default,
    Jemalloc,
    Mimalloc,
    Snmalloc,
    Rust,
}

impl Default for MemoryAllocatorBackend {
    fn default() -> Self {
        if cfg!(windows) {
            Self::Default
        } else {
            Self::Jemalloc
        }
    }
}

impl ToString for MemoryAllocatorBackend {
    fn to_string(&self) -> String {
        match self {
            Self::Default => "default",
            Self::Jemalloc => "jemalloc",
            Self::Mimalloc => "mimalloc",
            Self::Snmalloc => "snmalloc",
            Self::Rust => "rust",
        }
        .to_string()
    }
}

impl From<MemoryAllocatorBackend> for String {
    fn from(v: MemoryAllocatorBackend) -> Self {
        v.to_string()
    }
}

impl TryFrom<&str> for MemoryAllocatorBackend {
    type Error = eyre::Report;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "default" => Ok(Self::Default),
            "jemalloc" => Ok(Self::Jemalloc),
            "mimalloc" => Ok(Self::Mimalloc),
            "snmalloc" => Ok(Self::Snmalloc),
            "rust" => Ok(Self::Rust),
            _ => Err(eyre::eyre!("{} is not a valid memory allocator backend", value)),
        }
    }
}

impl TryFrom<String> for MemoryAllocatorBackend {
    type Error = eyre::Report;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

#[allow(unused)]
#[derive(Clone, Debug)]
pub struct StandaloneDistribution {
    /// Directory where distribution lives in the filesystem.
    pub base_dir: PathBuf,

    /// Rust target triple that this distribution runs on.
    pub target_triple: String,

    /// Python implementation name.
    pub python_implementation: String,

    /// PEP 425 Python tag value.
    pub python_tag: String,

    /// PEP 425 Python ABI tag.
    pub python_abi_tag: Option<String>,

    /// PEP 425 Python platform tag.
    pub python_platform_tag: String,

    /// Python version string.
    pub version: String,

    /// Path to Python interpreter executable.
    pub python_exe: PathBuf,

    /// Path to Python standard library.
    pub stdlib_path: PathBuf,

    /// Python packages in the standard library providing tests.
    stdlib_test_packages: Vec<String>,

    /// How libpython is linked in this distribution.
    // link_mode: StandaloneDistributionLinkMode,

    /// Symbol visibility for Python symbols.
    pub python_symbol_visibility: String,

    /// Capabilities of distribution to load extension modules.
    extension_module_loading: Vec<String>,

    /// Apple SDK build/targeting settings.
    // apple_sdk_info: Option<AppleSdkInfo>,

    /// Holds license information for the core distribution.
    // pub core_license: Option<LicensedComponent>,

    /// SPDX license shortnames that apply to this distribution.
    ///
    /// Licenses only cover the core distribution. Licenses for libraries
    /// required by extensions are stored next to the extension's linking
    /// info.
    pub licenses: Option<Vec<String>>,

    /// Path to file holding license text for this distribution.
    pub license_path: Option<PathBuf>,

    /// Path to Tcl library files.
    tcl_library_path: Option<PathBuf>,

    tcl_library_paths: Option<Vec<String>>,

    /// Object files providing the core Python implementation.
    ///
    /// Keys are relative paths. Values are filesystem paths.
    pub objs_core: BTreeMap<PathBuf, PathBuf>,

    /// Linking information for the core Python implementation.
    // pub links_core: Vec<LibraryDependency>,

    /// Filesystem location of pythonXY shared library for this distribution.
    ///
    ///pub libpython_shared_library: Option<PathBuf>,

    /// Extension modules available to this distribution.
    // pub extension_modules: BTreeMap<String, PythonExtensionModuleVariants>,

    pub frozen_c: Vec<u8>,

    /// Include files for Python.
    ///
    /// Keys are relative paths. Values are filesystem paths.
    pub includes: BTreeMap<String, PathBuf>,

    /// Static libraries available for linking.
    ///
    /// Keys are library names, without the "lib" prefix or file extension.
    /// Values are filesystem paths where library is located.
    pub libraries: BTreeMap<String, PathBuf>,

    pub py_modules: BTreeMap<String, PathBuf>,

    /// Non-module Python resource files.
    ///
    /// Keys are package names. Values are maps of resource name to data for the resource
    /// within that package.
    pub resources: BTreeMap<String, BTreeMap<String, PathBuf>>,

    /// Path to copy of hacked dist to use for packaging rules venvs
    // pub venv_base: PathBuf,

    /// Path to object file defining _PyImport_Inittab.
    pub inittab_object: PathBuf,

    /// Compiler flags to use to build object containing _PyImport_Inittab.
    pub inittab_cflags: Vec<String>,

    /// Tag to apply to bytecode files.
    pub cache_tag: String,

    /// Suffixes for Python module types.
    // module_suffixes: PythonModuleSuffixes,

    /// List of strings denoting C Runtime requirements.
    pub crt_features: Vec<String>,

    /// Configuration variables used by Python.
    config_vars: HashMap<String, String>,
}

impl StandaloneDistribution {
    // pub fn from_tar_zst_file(path: &Path, extract_dir: &Path) -> eyre::Result<Self> {
    //     let basename = path
    //         .file_name()
    //         .ok_or_else(|| eyre::eyre!("unable to determine filename"))?
    //         .to_string_lossy();
    //
    //     if !basename.ends_with(".tar.zst") {
    //         return Err(eyre::eyre!("unhandled distribution format: {}", path.display()));
    //     }
    //
    //     let fh = std::fs::File::open(path)
    //         .wrap_err_with(|| format!("unable to open {}", path.display()))?;
    //
    //     let reader = std::io::BufReader::new(fh);
    //
    //     Self::from_tar_zst(reader, extract_dir).context("reading tar.zst distribution data")
    // }
    //
    // /// Extract and analyze a standalone distribution from a zstd compressed tar stream.
    // pub fn from_tar_zst(source: impl std::io::Read, extract_dir: &Path) -> eyre::Result<Self> {
    //     let dctx = zstd::stream::Decoder::new(source)?;
    //
    //     Self::from_tar(dctx, extract_dir).context("reading tar distribution data")
    // }
    //
    // /// Extract and analyze a standalone distribution from a tar stream.
    // pub fn from_tar(source: impl std::io::Read, extract_dir: &Path) -> eyre::Result<Self> {
    //     let mut tf = tar::Archive::new(source);
    //
    //     {
    //         // let _lock = DistributionExtractLock::new(extract_dir)?;
    //
    //         // The content of the distribution could change between runs. But caching the extraction does keep things fast.
    //         let test_path = extract_dir.join("python").join("PYTHON.json");
    //         if !test_path.exists() {
    //             std::fs::create_dir_all(extract_dir)?;
    //             let absolute_path = std::fs::canonicalize(extract_dir)?;
    //
    //             let mut symlinks = vec![];
    //
    //             for entry in tf.entries()? {
    //                 let mut entry =
    //                     entry.map_err(|e| anyhow!("failed to iterate over archive: {}", e))?;
    //
    //                 // The mtimes in the archive may be 0 / UNIX epoch. This shouldn't
    //                 // matter. However, pip will sometimes attempt to produce a zip file of
    //                 // its own content and Python's zip code won't handle times before 1980,
    //                 // which is later than UNIX epoch. This can lead to pip blowing up at
    //                 // run-time. We work around this by not adjusting the mtime when
    //                 // extracting the archive. This effectively makes the mtime "now."
    //                 entry.set_preserve_mtime(false);
    // // Windows doesn't support symlinks without special permissions.
    //                 // So we track symlinks explicitly and copy files post extract if
    //                 // running on that platform.
    //                 let link_name = entry.link_name().unwrap_or(None);
    //
    //                 if link_name.is_some() && cfg!(target_family = "windows") {
    //                     // The entry's path is the file to write, relative to the archive's
    //                     // root. We need to expand to an absolute path to facilitate copying.
    //
    //                     // The link name is the file to symlink to, or the file we're copying.
    //                     // This path is relative to the entry path. So we need join with the
    //                     // entry's directory and canonicalize. There is also a security issue
    //                     // at play: archives could contain bogus symlinks pointing outside the
    //                     // archive. So we detect this, just in case.
    //
    //                     let mut dest = absolute_path.clone();
    //                     dest.extend(entry.path()?.components());
    //                     let dest = dest
    //                         .parse_dot()
    //                         .with_context(|| "dedotting symlinked source")?
    //                         .to_path_buf();
    //
    //                     let mut source = dest
    //                         .parent()
    //                         .ok_or_else(|| anyhow!("unable to resolve parent"))?
    //                         .to_path_buf();
    //                     source.extend(link_name.unwrap().components());
    //                     let source = source
    //                         .parse_dot()
    //                         .with_context(|| "dedotting symlink destination")?
    //                         .to_path_buf();
    //
    //                     if !source.starts_with(&absolute_path) {
    //                         return Err(anyhow!("malicious symlink detected in archive"));
    //                     }
    //
    //                     symlinks.push((source, dest));
    //                 } else {
    //                     entry
    //                         .unpack_in(&absolute_path)
    //                         .with_context(|| "unable to extract tar member")?;
    //                 }
    //             }
    //
    //             for (source, dest) in symlinks {
    //                 std::fs::copy(&source, &dest).with_context(|| {
    //                     format!(
    //                         "copying symlinked file {} -> {}",
    //                         source.display(),
    //                         dest.display(),
    //                     )
    //                 })?;
    //             }
    //
    //             // Ensure unpacked files are writable. We've had issues where we
    //             // consume archives with read-only file permissions. When we later
    //             // copy these files, we can run into trouble overwriting a read-only
    //             // file.
    //             let walk = walkdir::WalkDir::new(&absolute_path);
    //             for entry in walk.into_iter() {
    //                 let entry = entry?;
    //
    //                 let metadata = entry.metadata()?;
    //                 let mut permissions = metadata.permissions();
    //
    //                 if permissions.readonly() {
    //                     permissions.set_readonly(false);
    //                     std::fs::set_permissions(entry.path(), permissions).with_context(|| {
    //                         format!("unable to mark {} as writable", entry.path().display())
    //                     })?;
    //                 }
    //             }
    //         }
    //     }
    //
    //     Self::from_directory(extract_dir)
    // }

    /// Obtain an instance by scanning a directory containing an extracted distribution.
    #[allow(clippy::cognitive_complexity)]
    pub fn from_directory(dist_dir: &Path) -> eyre::Result<Self> {
        let mut objs_core: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
        let mut links_core: Vec<LibraryDependency> = Vec::new();
        // let mut extension_modules: BTreeMap<String, PythonExtensionModuleVariants> = BTreeMap::new();
        let mut includes: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut libraries = BTreeMap::new();
        let frozen_c: Vec<u8> = Vec::new();
        let mut py_modules: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut resources: BTreeMap<String, BTreeMap<String, PathBuf>> = BTreeMap::new();

        for entry in std::fs::read_dir(dist_dir)? {
            let entry = entry?;

            match entry.file_name().to_str() {
                Some(".DS_Store") => continue,
                Some("python") => continue,
                Some(value) => {
                    eyre::bail!(
                        "unexpected entry in distribution root directory: {}",
                        value
                    )
                }
                _ => {
                    eyre::bail!(
                        "error listing root directory of Python distribution"
                    )
                }
            };
        }

        let python_path = dist_dir.join("python");

        for entry in std::fs::read_dir(&python_path)? {
            let entry = entry?;

            match entry.file_name().to_str() {
                Some("build") => continue,
                Some("install") => continue,
                Some("lib") => continue,
                Some("licenses") => continue,
                Some("LICENSE.rst") => continue,
                Some("PYTHON.json") => continue,
                Some(value) => {
                    eyre::bail!("unexpected entry in python/ directory: {}", value)
                }
                _ => eyre::bail!("error listing python/ directory"),
            };
        }

        let pi = parse_python_json_from_distribution(dist_dir)?;
        dbg!(&pi);

        // Derive the distribution's license from a license file, if present.
        // let core_license = if let Some(ref python_license_path) = pi.license_path {
        //     let license_path = python_path.join(python_license_path);
        //     let license_text = std::fs::read_to_string(&license_path).with_context(|| {
        //         format!("unable to read Python license {}", license_path.display())
        //     })?;
        //
        //     let expression = pi.licenses.clone().unwrap().join(" OR ");
        //
        //     let mut component = LicensedComponent::new_spdx(
        //         ComponentFlavor::PythonDistribution(pi.python_implementation_name.clone()),
        //         &expression,
        //     )?;
        //     component.add_license_text(license_text);
        //
        //     Some(component)
        // } else {
        //     None
        // };

        // Collect object files for libpython.
        for obj in &pi.build_info.core.objs {
            let rel_path = PathBuf::from(obj);
            let full_path = python_path.join(obj);

            objs_core.insert(rel_path, full_path);
        }

        for entry in &pi.build_info.core.links {
            let depends = entry.to_library_dependency(&python_path);

            if let Some(p) = &depends.static_library {
                if let Some(p) = p.backing_path() {
                    libraries.insert(depends.name.clone(), p.to_path_buf());
                }
            }

            links_core.push(depends);
        }

        // let module_suffixes = PythonModuleSuffixes {
        //     source: pi
        //         .python_suffixes
        //         .get("source")
        //         .ok_or_else(|| eyre::eyre!("distribution does not define source suffixes"))?
        //         .clone(),
        //     bytecode: pi
        //         .python_suffixes
        //         .get("bytecode")
        //         .ok_or_else(|| eyre::eyre!("distribution does not define bytecode suffixes"))?
        //         .clone(),
        //     debug_bytecode: pi
        //         .python_suffixes
        //         .get("debug_bytecode")
        //         .ok_or_else(|| eyre::eyre!("distribution does not define debug bytecode suffixes"))?
        //         .clone(),
        //     optimized_bytecode: pi
        //         .python_suffixes
        //         .get("optimized_bytecode")
        //         .ok_or_else(|| eyre::eyre!("distribution does not define optimized bytecode suffixes"))?
        //         .clone(),
        //     extension: pi
        //         .python_suffixes
        //         .get("extension")
        //         .ok_or_else(|| eyre::eyre!("distribution does not define extension suffixes"))?
        //         .clone(),
        // };
        //
        // // Collect extension modules.
        // for (module, variants) in &pi.build_info.extensions {
        //     let mut ems = PythonExtensionModuleVariants::default();
        //
        //     for entry in variants.iter() {
        //         let extension_file_suffix = if let Some(p) = &entry.shared_lib {
        //             if let Some(idx) = p.rfind('.') {
        //                 p[idx..].to_string()
        //             } else {
        //                 "".to_string()
        //             }
        //         } else {
        //             "".to_string()
        //         };
        //
        //         let object_file_data = entry
        //             .objs
        //             .iter()
        //             .map(|p| FileData::Path(python_path.join(p)))
        //             .collect();
        //         let mut links = Vec::new();
        //
        //         for link in &entry.links {
        //             let depends = link.to_library_dependency(&python_path);
        //
        //             if let Some(p) = &depends.static_library {
        //                 if let Some(p) = p.backing_path() {
        //                     libraries.insert(depends.name.clone(), p.to_path_buf());
        //                 }
        //             }
        //
        //             links.push(depends);
        //         }
        //
        //         let component_flavor =
        //             ComponentFlavor::PythonStandardLibraryExtensionModule(module.clone());
        //
        //         let mut license = if entry.license_public_domain.unwrap_or(false) {
        //             LicensedComponent::new(component_flavor, LicenseFlavor::PublicDomain)
        //         } else if let Some(licenses) = &entry.licenses {
        //             let expression = licenses.join(" OR ");
        //             LicensedComponent::new_spdx(component_flavor, &expression)?
        //         } else if let Some(core) = &core_license {
        //             LicensedComponent::new_spdx(
        //                 component_flavor,
        //                 core.spdx_expression()
        //                     .ok_or_else(|| anyhow!("could not resolve SPDX license for core"))?
        //                     .as_ref(),
        //             )?
        //         } else {
        //             LicensedComponent::new(component_flavor, LicenseFlavor::None)
        //         };
        //
        //         if let Some(license_paths) = &entry.license_paths {
        //             for path in license_paths {
        //                 let path = python_path.join(path);
        //                 let text = std::fs::read_to_string(&path)
        //                     .with_context(|| format!("reading {}", path.display()))?;
        //
        //                 license.add_license_text(text);
        //             }
        //         }
        //
        //         ems.push(PythonExtensionModule {
        //             name: module.clone(),
        //             init_fn: Some(entry.init_fn.clone()),
        //             extension_file_suffix,
        //             shared_library: entry
        //                 .shared_lib
        //                 .as_ref()
        //                 .map(|path| FileData::Path(python_path.join(path))),
        //             object_file_data,
        //             is_package: false,
        //             link_libraries: links,
        //             is_stdlib: true,
        //             builtin_default: entry.in_core,
        //             required: entry.required,
        //             variant: Some(entry.variant.clone()),
        //             license: Some(license),
        //         });
        //     }
        //
        //     extension_modules.insert(module.clone(), ems);
        // }

        let include_path = if let Some(p) = pi.python_paths.get("include") {
            python_path.join(p)
        } else {
            eyre::bail!("include path not defined in distribution");
        };

        for entry in walk_tree_files(&include_path) {
            let full_path = entry.path();
            let rel_path = full_path
                .strip_prefix(&include_path)
                .expect("unable to strip prefix");
            includes.insert(
                String::from(rel_path.to_str().expect("path to string")),
                full_path.to_path_buf(),
            );
        }

        let stdlib_path = if let Some(p) = pi.python_paths.get("stdlib") {
            python_path.join(p)
        } else {
            eyre::bail!("stdlib path not defined in distribution");
        };

        // for entry in find_python_resources(
        //     &stdlib_path,
        //     &pi.python_implementation_cache_tag,
        //     &module_suffixes,
        //     false,
        //     true,
        // )? {
        //     match entry? {
        //         PythonResource::PackageResource(resource) => {
        //             if !resources.contains_key(&resource.leaf_package) {
        //                 resources.insert(resource.leaf_package.clone(), BTreeMap::new());
        //             }
        //
        //             resources.get_mut(&resource.leaf_package).unwrap().insert(
        //                 resource.relative_name.clone(),
        //                 match &resource.data {
        //                     FileData::Path(path) => path.to_path_buf(),
        //                     FileData::Memory(_) => {
        //                         return Err(anyhow!(
        //                             "should not have received in-memory resource data"
        //                         ))
        //                     }
        //                 },
        //             );
        //         }
        //         PythonResource::ModuleSource(source) => match &source.source {
        //             FileData::Path(path) => {
        //                 py_modules.insert(source.name.clone(), path.to_path_buf());
        //             }
        //             FileData::Memory(_) => {
        //                 return Err(anyhow!("should not have received in-memory source data"))
        //             }
        //         },
        //
        //         PythonResource::ModuleBytecodeRequest(_) => {}
        //         PythonResource::ModuleBytecode(_) => {}
        //         PythonResource::PackageDistributionResource(_) => {}
        //         PythonResource::ExtensionModule(_) => {}
        //         PythonResource::EggFile(_) => {}
        //         PythonResource::PathExtension(_) => {}
        //         PythonResource::File(_) => {}
        //     };
        // }

       //  let venv_base = dist_dir.parent().unwrap().join("hacked_base");
       //
       //  let (link_mode, libpython_shared_library) = if pi.libpython_link_mode == "static" {
       //      (StandaloneDistributionLinkMode::Static, None)
       //  } else if pi.libpython_link_mode == "shared" {
       //      (
       //          StandaloneDistributionLinkMode::Dynamic,
       //          Some(python_path.join(pi.build_info.core.shared_lib.unwrap())),
       //      )
       //  } else {
       //      return Err(anyhow!("unhandled link mode: {}", pi.libpython_link_mode));
       //  };
       //
       //  let apple_sdk_info = if let Some(canonical_name) = pi.apple_sdk_canonical_name {
       //      let platform = pi
       //          .apple_sdk_platform
       //          .ok_or_else(|| anyhow!("apple_sdk_platform not defined"))?;
       //      let version = pi
       //          .apple_sdk_version
       //          .ok_or_else(|| anyhow!("apple_sdk_version not defined"))?;
       //      let deployment_target = pi
       //          .apple_sdk_deployment_target
       //          .ok_or_else(|| anyhow!("apple_sdk_deployment_target not defined"))?;
       //
       //      Some(AppleSdkInfo {
       //          canonical_name,
       //          platform,
       //          version,
       //          deployment_target,
       //      })
       //  } else {
       //      None
       // };

        let inittab_object = python_path.join(pi.build_info.inittab_object);

    // let pi = parse_python_json_from_distribution(dist_dir)?;

        let python_exe = dist_dir.join("python").join(&pi.python_exe);

        Ok(Self {
            base_dir: dist_dir.to_path_buf(),
            target_triple: pi.target_triple,
            python_implementation: pi.python_implementation_name,
            python_tag: pi.python_tag,
            python_abi_tag: pi.python_abi_tag,
            python_platform_tag: pi.python_platform_tag,
            version: pi.python_version.clone(),
            // python_exe: python_exe_path(dist_dir)?,
            python_exe,
            stdlib_path,
            stdlib_test_packages: pi.python_stdlib_test_packages,
            // link_mode,
            python_symbol_visibility: pi.python_symbol_visibility,
            extension_module_loading: pi.python_extension_module_loading,
            // apple_sdk_info,
            // core_license,
            licenses: pi.licenses.clone(),
            license_path: pi.license_path.as_ref().map(PathBuf::from),
            tcl_library_path: pi
                .tcl_library_path
                .as_ref()
                .map(|path| dist_dir.join("python").join(path)),
            tcl_library_paths: pi.tcl_library_paths.clone(),
            // extension_modules,
            frozen_c,
            includes,
            // links_core,
            libraries,
            objs_core,
            // libpython_shared_library,
            py_modules,
            resources,
            // venv_base,
            inittab_object,
            inittab_cflags: pi.build_info.inittab_cflags,
            cache_tag: pi.python_implementation_cache_tag,
            // module_suffixes,
            crt_features: pi.crt_features,
            config_vars: pi.python_config_vars,
        })
    }

    /// Whether the distribution is capable of loading filed-based Python extension modules.
    pub fn is_extension_module_file_loadable(&self) -> bool {
        self.extension_module_loading
            .contains(&"shared-library".to_string())
    }
}

fn parse_python_major_minor_version(version: &str) -> String {
    let mut at_least_minor_version = String::from(version);
    if !version.contains('.') {
        at_least_minor_version.push_str(".0");
    }
    at_least_minor_version
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".")
}

/// Defines how Python resources should be packaged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonPackagingPolicy {
    // /// Which extension modules should be included.
    // extension_module_filter: ExtensionModuleFilter,

    /// Preferred variants of extension modules.
    preferred_extension_module_variants: HashMap<String, String>,

    /// Where resources should be placed/loaded from by default.
    resources_location: ConcreteResourceLocation,

    resources_location_fallback: Option<ConcreteResourceLocation>,

    allow_in_memory_shared_library_loading: bool,

    allow_files: bool,

    file_scanner_emit_files: bool,

    file_scanner_classify_files: bool,

    include_classified_resources: bool,

    /// Whether to include source module from the Python distribution.
    include_distribution_sources: bool,

    /// Whether to include Python module source for non-distribution modules.
    include_non_distribution_sources: bool,

    /// Whether to include package resource files.
    include_distribution_resources: bool,

    /// Whether to include test files.
    include_test: bool,

    include_file_resources: bool,

    broken_extensions: HashMap<String, Vec<String>>,

    /// Whether to write Python bytecode at optimization level 0.
    bytecode_optimize_level_zero: bool,

    /// Whether to write Python bytecode at optimization level 1.
    bytecode_optimize_level_one: bool,

    /// Whether to write Python bytecode at optimization level 2.
    bytecode_optimize_level_two: bool,

    /// Python modules for which bytecode should not be generated by default.
    no_bytecode_modules: HashSet<String>,
}

impl Default for PythonPackagingPolicy {
    fn default() -> Self {
        PythonPackagingPolicy {
            // extension_module_filter: ExtensionModuleFilter::All,
            preferred_extension_module_variants: HashMap::new(),
            // resources_location: ConcreteResourceLocation::InMemory,
            // resources_location_fallback: None,
            allow_in_memory_shared_library_loading: false,
            allow_files: false,
            file_scanner_emit_files: false,
            file_scanner_classify_files: true,
            include_classified_resources: true,
            include_distribution_sources: true,
            include_non_distribution_sources: true,
            include_distribution_resources: false,
            include_test: false,
            include_file_resources: false,
            broken_extensions: HashMap::new(),
            bytecode_optimize_level_zero: true,
            bytecode_optimize_level_one: false,
            bytecode_optimize_level_two: false,
            no_bytecode_modules: HashSet::new(),
        }
    }
}

impl PythonPackagingPolicy {
    // /// Obtain the active extension module filter for this instance.
    // pub fn extension_module_filter(&self) -> &ExtensionModuleFilter {
    //     &self.extension_module_filter
    // }
    //
    // /// Set the extension module filter to use.
    // pub fn set_extension_module_filter(&mut self, filter: ExtensionModuleFilter) {
    //     self.extension_module_filter = filter;
    // }
   
    /// Obtain the primary location for added resources.
    pub fn resources_location(&self) -> &ConcreteResourceLocation {
        &self.resources_location
    }

    /// Set the primary location for added resources.
    pub fn set_resources_location(&mut self, location: ConcreteResourceLocation) {
        self.resources_location = location;
    }

    /// Obtain the fallback location for added resources.
    pub fn resources_location_fallback(&self) -> &Option<ConcreteResourceLocation> {
        &self.resources_location_fallback
    }

    /// Set the fallback location for added resources.
    pub fn set_resources_location_fallback(&mut self, location: Option<ConcreteResourceLocation>) {
        self.resources_location_fallback = location;
    }

    /// Obtain the preferred extension module variants for this policy.
    ///
    /// The returned object is a mapping of extension name to its variant
    /// name.
    pub fn preferred_extension_module_variants(&self) -> &HashMap<String, String> {
        &self.preferred_extension_module_variants
    }

    /// Denote the preferred variant for an extension module.
    ///
    /// If set, the named variant will be chosen if it is present.
    pub fn set_preferred_extension_module_variant(&mut self, extension: &str, variant: &str) {
        self.preferred_extension_module_variants
            .insert(extension.to_string(), variant.to_string());
    }

    /// Whether to allow in-memory shared library loading.
    pub fn allow_in_memory_shared_library_loading(&self) -> bool {
        self.allow_in_memory_shared_library_loading
    }

    /// Set the value for whether to allow in-memory shared library loading.
    pub fn set_allow_in_memory_shared_library_loading(&mut self, value: bool) {
        self.allow_in_memory_shared_library_loading = value;
    }

    // /// Obtain the primary location for added resources.
    // pub fn resources_location(&self) -> &ConcreteResourceLocation {
    //     &self.resources_location
    // }
    //
    // /// Set the primary location for added resources.
    // pub fn set_resources_location(&mut self, location: ConcreteResourceLocation) {
    //     self.resources_location = location;
    // }
    //
    // /// Obtain the fallback location for added resources.
    // pub fn resources_location_fallback(&self) -> &Option<ConcreteResourceLocation> {
    //     &self.resources_location_fallback
    // }
    //
    // /// Set the fallback location for added resources.
    // pub fn set_resources_location_fallback(&mut self, location: Option<ConcreteResourceLocation>) {
    //     self.resources_location_fallback = location;
    // }

    /// Mark an extension as broken on a target platform, preventing it from being used.
    pub fn register_broken_extension(&mut self, target_triple: &str, extension: &str) {
        if !self.broken_extensions.contains_key(target_triple) {
            self.broken_extensions
                .insert(target_triple.to_string(), vec![]);
        }

        self.broken_extensions
            .get_mut(target_triple)
            .unwrap()
            .push(extension.to_string());
    }

    /// Register a Python module as one that should not generate bytecode.
    ///
    /// When source modules matching names registered with this function are added,
    /// their default settings for adding bytecode will always be false.
    ///
    /// It is still possible to force bytecode generation by setting the add context
    /// fields to true or explicitly adding a bytecode resource.
    pub fn register_no_bytecode_module(&mut self, name: &str) {
        self.no_bytecode_modules.insert(name.to_string());
    }

    // /// Set the primary location for added resources.
    // pub fn set_resources_location(&mut self, location: ConcreteResourceLocation) {
    //     // self.resources_location = location;
    // }
    //
    // /// Set the fallback location for added resources.
    // pub fn set_resources_location_fallback(&mut self, location: Option<ConcreteResourceLocation>) {
    //     // self.resources_location_fallback = location;
    // }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrePackagedResource {
    pub name: String,
    pub is_package: bool,
    pub is_namespace_package: bool,
    pub in_memory_source: Option<FileData>,
    pub in_memory_bytecode: Option<PythonModuleBytecodeProvider>,
    pub in_memory_bytecode_opt1: Option<PythonModuleBytecodeProvider>,
    pub in_memory_bytecode_opt2: Option<PythonModuleBytecodeProvider>,
    pub in_memory_extension_module_shared_library: Option<FileData>,
    pub in_memory_resources: Option<BTreeMap<String, FileData>>,
    pub in_memory_distribution_resources: Option<BTreeMap<String, FileData>>,
    pub in_memory_shared_library: Option<FileData>,
    pub shared_library_dependency_names: Option<Vec<String>>,
    // (prefix, source code)
    pub relative_path_module_source: Option<(String, FileData)>,
    // (prefix, bytecode tag, source code)
    // pub relative_path_bytecode: Option<(String, String, PythonModuleBytecodeProvider)>,
    // pub relative_path_bytecode_opt1: Option<(String, String, PythonModuleBytecodeProvider)>,
    // pub relative_path_bytecode_opt2: Option<(String, String, PythonModuleBytecodeProvider)>,
    // (path, data)
    pub relative_path_extension_module_shared_library: Option<(PathBuf, FileData)>,
    pub relative_path_package_resources: Option<BTreeMap<String, (PathBuf, FileData)>>,
    pub relative_path_distribution_resources: Option<BTreeMap<String, (PathBuf, FileData)>>,
    pub relative_path_shared_library: Option<(String, PathBuf, FileData)>,
    pub is_module: bool,
    pub is_builtin_extension_module: bool,
    pub is_frozen_module: bool,
    pub is_extension_module: bool,
    pub is_shared_library: bool,
    pub is_utf8_filename_data: bool,
    pub file_executable: bool,
    pub file_data_embedded: Option<FileData>,
    pub file_data_utf8_relative_path: Option<(PathBuf, FileData)>,
}

#[derive(Clone)]
pub struct PythonResourceCollector {
    /// Where resources can be placed.
    allowed_locations: Vec<AbstractResourceLocation>,
    allowed_extension_module_locations: Vec<AbstractResourceLocation>,
    allow_new_builtin_extension_modules: bool,
    allow_files: bool,
    resources: BTreeMap<String, PrePackagedResource>,
    // licensed_components: LicensedComponents,
}

impl PythonResourceCollector {
    pub fn new(
        allowed_locations: Vec<AbstractResourceLocation>,
        allowed_extension_module_locations: Vec<AbstractResourceLocation>,
        allow_new_builtin_extension_modules: bool,
        allow_files: bool,
    ) -> Self {
        Self {
            allowed_locations,
            allowed_extension_module_locations,
            allow_new_builtin_extension_modules,
            allow_files,
            resources: BTreeMap::new(),
            // licensed_components: LicensedComponents::default(),
        }
    }

    /// Searches for Python sources for references to __file__.
    ///
    /// __file__ usage can be problematic for in-memory modules. This method searches
    /// for its occurrences and returns module names having it present.
    pub fn find_dunder_file(&self) -> eyre::Result<BTreeSet<String>> {
        let mut res = BTreeSet::new();

        for (name, module) in &self.resources {
            if let Some(location) = &module.in_memory_source {
                if has_dunder_file(&location.resolve_content()?)? {
                    res.insert(name.clone());
                }
            }

            if let Some(PythonModuleBytecodeProvider::FromSource(location)) =
                &module.in_memory_bytecode
            {
                if has_dunder_file(&location.resolve_content()?)? {
                    res.insert(name.clone());
                }
            }

            if let Some(PythonModuleBytecodeProvider::FromSource(location)) =
                &module.in_memory_bytecode_opt1
            {
                if has_dunder_file(&location.resolve_content()?)? {
                    res.insert(name.clone());
                }
            }

            if let Some(PythonModuleBytecodeProvider::FromSource(location)) =
                &module.in_memory_bytecode_opt2
            {
                if has_dunder_file(&location.resolve_content()?)? {
                    res.insert(name.clone());
                }
            }
        }

        Ok(res)
    }
}

static RE_CODING: Lazy<regex::bytes::Regex> = Lazy::new(|| {
    regex::bytes::Regex::new(r"^[ \t\f]*#.*?coding[:=][ \t]*([-_.a-zA-Z0-9]+)").unwrap()
});

/// Derive the source encoding from Python source code.
pub fn python_source_encoding(source: &[u8]) -> Vec<u8> {
    // Default source encoding is UTF-8. But per PEP 263, the first or second
    // line of source can match a regular expression to define a custom
    // encoding.
    let lines = source.split(|v| v == &b'\n');

    for (i, line) in lines.enumerate() {
        if i > 1 {
            break;
        }

        if let Some(m) = RE_CODING.find(line) {
            return m.as_bytes().to_vec();
        }
    }

    b"utf-8".to_vec()
}

/// Whether __file__ occurs in Python source code.
pub fn has_dunder_file(source: &[u8]) -> eyre::Result<bool> {
    // We can't just look for b"__file__ because the source file may be in
    // encodings like UTF-16. So we need to decode to Unicode first then look for
    // the code points.
    let encoding = python_source_encoding(source);

    let encoder = match encoding_rs::Encoding::for_label(&encoding) {
        Some(encoder) => encoder,
        None => encoding_rs::UTF_8,
    };

    let (source, ..) = encoder.decode(source);

    Ok(source.contains("__file__"))
}

/// Describes the concrete location of a Python resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConcreteResourceLocation {
    /// Resource is loaded from memory.
    InMemory,
    /// Reosurce is loaded from a relative filesystem path.
    RelativePath(String),
}

impl From<&ConcreteResourceLocation> for AbstractResourceLocation {
    fn from(l: &ConcreteResourceLocation) -> Self {
        match l {
            ConcreteResourceLocation::InMemory => AbstractResourceLocation::InMemory,
            ConcreteResourceLocation::RelativePath(_) => AbstractResourceLocation::RelativePath,
        }
    }
}

impl ToString for ConcreteResourceLocation {
    fn to_string(&self) -> String {
        match self {
            ConcreteResourceLocation::InMemory => "in-memory".to_string(),
            ConcreteResourceLocation::RelativePath(prefix) => {
                format!("filesystem-relative:{}", prefix)
            }
        }
    }
}

impl From<ConcreteResourceLocation> for String {
    fn from(location: ConcreteResourceLocation) -> Self {
        location.to_string()
    }
}

impl TryFrom<&str> for ConcreteResourceLocation {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value == "in-memory" {
            Ok(Self::InMemory)
        } else {
            let parts = value.splitn(2, ':').collect::<Vec<_>>();

            if parts.len() != 2 {
                Err(format!("{} is not a valid resource location", value))
            } else {
                let prefix = parts[0];
                let suffix = parts[1];

                if prefix == "filesystem-relative" {
                    Ok(Self::RelativePath(suffix.to_string()))
                } else {
                    Err(format!("{} is not a valid resource location", value))
                }
            }
        }
    }

    
}

// impl PythonDistribution for StandaloneDistribution {
impl StandaloneDistribution {
    // fn clone_trait(&self) -> Arc<dyn PythonDistribution> {
    //     Arc::new(self.clone())
    // }

    fn target_triple(&self) -> &str {
        &self.target_triple
    }

    fn compatible_host_triples(&self) -> Vec<String> {
        let mut res = vec![self.target_triple.clone()];

        res.extend(
            match self.target_triple() {
                "aarch64-unknown-linux-gnu" => vec![],
                // musl libc linked distributions run on GNU Linux.
                "aarch64-unknown-linux-musl" => vec!["aarch64-unknown-linux-gnu"],
                "x86_64-unknown-linux-gnu" => vec![],
                // musl libc linked distributions run on GNU Linux.
                "x86_64-unknown-linux-musl" => vec!["x86_64-unknown-linux-gnu"],
                "aarch64-apple-darwin" => vec![],
                "x86_64-apple-darwin" => vec![],
                // 32-bit Windows GNU on 32-bit Windows MSVC and 64-bit Windows.
                "i686-pc-windows-gnu" => vec![
                    "i686-pc-windows-msvc",
                    "x86_64-pc-windows-gnu",
                    "x86_64-pc-windows-msvc",
                ],
                // 32-bit Windows MSVC runs on 32-bit Windows MSVC and 64-bit Windows.
                "i686-pc-windows-msvc" => vec![
                    "i686-pc-windows-gnu",
                    "x86_64-pc-windows-gnu",
                    "x86_64-pc-windows-msvc",
                ],
                // 64-bit Windows GNU/MSVC runs on the other.
                "x86_64-pc-windows-gnu" => vec!["x86_64-pc-windows-msvc"],
                "x86_64-pc-windows-msvc" => vec!["x86_64-pc-windows-gnu"],
                _ => vec![],
            }
            .iter()
            .map(|x| x.to_string()),
        );

        res
    }

    fn python_exe_path(&self) -> &Path {
        &self.python_exe
    }

    fn python_version(&self) -> &str {
        &self.version
    }

    fn python_major_minor_version(&self) -> String {
        parse_python_major_minor_version(&self.version)
    }

    fn python_implementation(&self) -> &str {
        &self.python_implementation
    }

    fn python_implementation_short(&self) -> &str {
        // TODO capture this in distribution metadata
        match self.python_implementation.as_str() {
            "cpython" => "cp",
            "python" => "py",
            "pypy" => "pp",
            "ironpython" => "ip",
            "jython" => "jy",
            s => panic!("unsupported Python implementation: {}", s),
        }
    }

    fn python_tag(&self) -> &str {
        &self.python_tag
    }

    fn python_abi_tag(&self) -> Option<&str> {
        match &self.python_abi_tag {
            Some(tag) => {
                if tag.is_empty() {
                    None
                } else {
                    Some(tag)
                }
            }
            None => None,
        }
    }

    fn python_platform_tag(&self) -> &str {
        &self.python_platform_tag
    }

    fn python_platform_compatibility_tag(&self) -> &str {
        // TODO capture this in distribution metadata.
        if !self.is_extension_module_file_loadable() {
            return "none";
        }

        match self.python_platform_tag.as_str() {
            "linux-aarch64" => "manylinux2014_aarch64",
            "linux-x86_64" => "manylinux2014_x86_64",
            "linux-i686" => "manylinux2014_i686",
            "macosx-10.9-x86_64" => "macosx_10_9_x86_64",
            "macosx-11.0-arm64" => "macosx_11_0_arm64",
            "win-amd64" => "win_amd64",
            "win32" => "win32",
            p => panic!("unsupported Python platform: {}", p),
        }
    }

    fn cache_tag(&self) -> &str {
        &self.cache_tag
    }

    // fn python_module_suffixes(&self) -> eyre::Result<PythonModuleSuffixes> {
    //     Ok(self.module_suffixes.clone())
    // }

    fn python_config_vars(&self) -> &HashMap<String, String> {
        &self.config_vars
    }

    fn stdlib_test_packages(&self) -> Vec<String> {
        self.stdlib_test_packages.clone()
    }

    // fn apple_sdk_info(&self) -> Option<&AppleSdkInfo> {
    //     self.apple_sdk_info.as_ref()
    // }

    // fn create_bytecode_compiler(
    //     &self,
    //     env: &Environment,
    // ) -> Result<Box<dyn PythonBytecodeCompiler>> {
    //     let temp_dir = env.temporary_directory("pyoxidizer-bytecode-compiler")?;
    //
    //     Ok(Box::new(BytecodeCompiler::new(
    //         &self.python_exe,
    //         temp_dir.path(),
    //     )?))
    // }

    fn create_packaging_policy(&self) -> eyre::Result<PythonPackagingPolicy> {
        let mut policy = PythonPackagingPolicy::default();

        // In-memory shared library loading is brittle. Disable this configuration
        // even if supported because it leads to pain.
        if self.supports_in_memory_shared_library_loading() {
            policy.set_resources_location(ConcreteResourceLocation::InMemory);
            policy.set_resources_location_fallback(Some(ConcreteResourceLocation::RelativePath(
                "lib".to_string(),
            )));
        }

        for triple in LINUX_TARGET_TRIPLES.iter() {
            for ext in BROKEN_EXTENSIONS_LINUX.iter() {
                policy.register_broken_extension(triple, ext);
            }
        }

        for triple in MACOS_TARGET_TRIPLES.iter() {
            for ext in BROKEN_EXTENSIONS_MACOS.iter() {
                policy.register_broken_extension(triple, ext);
            }
        }

        for name in NO_BYTECODE_MODULES.iter() {
            policy.register_no_bytecode_module(name);
        }

        Ok(policy)
    }

    fn create_python_interpreter_config(&self) -> eyre::Result<PyembedPythonInterpreterConfig> {
        let embedded_default = PyembedPythonInterpreterConfig::default();

        Ok(PyembedPythonInterpreterConfig {
            config: PythonInterpreterConfig {
                profile: PythonInterpreterProfile::Isolated,
                ..embedded_default.config
            },
            // allocator_backend: default_memory_allocator(self.target_triple()),
            allocator_raw: true,
            oxidized_importer: true,
            filesystem_importer: false,
            // terminfo_resolution: TerminfoResolution::Dynamic,
            ..embedded_default
        })
    }

    // fn as_python_executable_builder(
    //     &self,
    //     host_triple: &str,
    //     target_triple: &str,
    //     name: &str,
    //     libpython_link_mode: LibpythonLinkMode,
    //     policy: &PythonPackagingPolicy,
    //     config: &PyembedPythonInterpreterConfig,
    //     host_distribution: Option<StandaloneDistribution,
    //     // host_distribution: Option<Arc<dyn PythonDistribution>>,
    // ) -> eyre::Result<Box<dyn PythonBinaryBuilder>> {
    //     // TODO can we avoid these clones?
    //     let target_distribution = Arc::new(self.clone());
    //     let host_distribution: Arc<dyn PythonDistribution> =
    //         host_distribution.unwrap_or_else(|| Arc::new(self.clone()));
    //
    //     let builder = StandalonePythonExecutableBuilder::from_distribution(
    //         host_distribution,
    //         target_distribution,
    //         host_triple.to_string(),
    //         target_triple.to_string(),
    //         name.to_string(),
    //         libpython_link_mode,
    //         policy.clone(),
    //         config.clone(),
    //     )?;
    //
    //     Ok(builder as Box<dyn PythonBinaryBuilder>)
    // }

    // fn python_resources<'a>(&self) -> Vec<PythonResource<'a>> {
    //     let extension_modules = self
    //         .extension_modules
    //         .iter()
    //         .flat_map(|(_, exts)| exts.iter().map(|e| PythonResource::from(e.to_owned())));
    //
    //     let module_sources = self.py_modules.iter().map(|(name, path)| {
    //         PythonResource::from(PythonModuleSource {
    //             name: name.clone(),
    //             source: FileData::Path(path.clone()),
    //             is_package: is_package_from_path(path),
    //             cache_tag: self.cache_tag.clone(),
    //             is_stdlib: true,
    //             is_test: self.is_stdlib_test_package(name),
    //         })
    //     });
    //
    //     let resource_datas = self.resources.iter().flat_map(|(package, inner)| {
    //         inner.iter().map(move |(name, path)| {
    //             PythonResource::from(PythonPacageResource {
    //                 leaf_package: package.clone(),
    //                 relative_name: name.clone(),
    //                 data: FileData::Path(path.clone()),
    //                 is_stdlib: true,
    //                 is_test: self.is_stdlib_test_package(package),
    //             })
    //         })
    //     });
    //
    //     extension_modules
    //         .chain(module_sources)
    //         .chain(resource_datas)
    //         .collect::<Vec<PythonResource<'a>>>()
    // }

    // /// Ensure pip is available to run in the distribution.
    // fn ensure_pip(&self) -> Result<PathBuf> {
    //     let dist_prefix = self.base_dir.join("python").join("install");
    //     let python_paths = resolve_python_paths(&dist_prefix, &self.version);
    //
    //     let pip_path = python_paths.bin_dir.join(PIP_EXE_BASENAME);
    //
    //     if !pip_path.exists() {
    //         println!("{} doesnt exist", pip_path.display().to_string());
    //         invoke_python(&python_paths, &["-m", "ensurepip"]);
    //     }
    //
    //     Ok(pip_path)
    // }

    // fn resolve_distutils(
    //     &self,
    //     libpython_link_mode: LibpythonLinkMode,
    //     dest_dir: &Path,
    //     extra_python_paths: &[&Path],
    // ) -> Result<HashMap<String, String>> {
    //     let mut res = match libpython_link_mode {
    //         // We need to patch distutils if the distribution is statically linked.
    //         LibpythonLinkMode::Static => prepare_hacked_distutils(
    //             &self.stdlib_path.join("distutils"),
    //             dest_dir,
    //             extra_python_paths,
    //         ),
    //         LibpythonLinkMode::Dynamic => Ok(HashMap::new()),
    //     }?;
    //
    //     // Modern versions of setuptools vendor their own copy of distutils
    //     // and use it by default. If we hacked distutils above, we need to ensure
    //     // that hacked copy is used. Even if we don't hack distutils, there is an
    //     // unknown change in behavior in a release after setuptools 63.2.0 causing
    //     // extension module building to fail due to missing Python.h. In older
    //     // versions the CFLAGS has ath the path to our standalone
    //     // distribution. But in modern versions it uses the install/include/pythonX.Y        // path from sysconfig with the proper prefixing. This bug was exposed when
    //     // we attempted to upgrade PBS distributions from 20220802 to 20221002.
    //     // We'll need to fix this before Python 3.12, which drops distutils from the
    //     // stdlib.
    //     //
    //     // The actual value of the environment variable doesn't matter as long as it
    //     // isn't "local". However, the setuptools docs suggest using "stdlib."
    //     res.insert("SETUPTOOLS_USE_DISTUTILS".to_string(), "stdlib".to_string());
    //
    //     Ok(res)
    // }

    /// Determines whether dynamically linked extension modules can be loaded from memory.
    fn supports_in_memory_shared_library_loading(&self) -> bool {
        // Loading from memory is only supported on Windows where symbols are
        // declspec(dllexport) and the distribution is capable of loading
        // shared library extensions.
        self.target_triple.contains("pc-windows")
            && self.python_symbol_visibility == "dllexport"
            && self
                .extension_module_loading
                .contains(&"shared-library".to_string())
    }

    // fn tcl_files(&self) -> Result<Vec<(PathBuf, FileEntry)>> {
    //     let mut res = vec![];
    //
    //     if let Some(root) = &self.tcl_library_path {
    //         if let Some(paths) = &self.tcl_library_paths {
    //             for subdir in paths {
    //                 for entry in walkdir::WalkDir::new(root.join(subdir))
    //                     .sort_by(|a, b| a.file_name().cmp(b.file_name()))
    //                     .into_iter()
    //                 {
    //                     let entry = entry?;
    //
    //                     let path = entry.path();
    //
    //                     if path.is_dir() {
    //                         continue;
    //                     }
    //
    //                     let rel_path = path.strip_prefix(root)?;
    //
    //                     res.push((rel_path.to_path_buf(), FileEntry::try_from(path)?));
    //                 }
    //             }
    //         }
    //     }
    //
    //     Ok(res)
    // }

    // fn tcl_library_path_directory(&self) -> Option<String> {
    //     // TODO this should probably be exposed from the JSON metadata.
    //     Some("tcl8.6".to_string())
    // }
}

// /// Describes a generic way to build a Python binary.
// ///
// /// Binary here means an executable or library containing or linking to a
// /// Python interpreter. It also includes embeddable resources within that
// /// binary.
// ///
// /// Concrete implementations can be turned into build artifacts or binaries
// /// themselves.
// pub trait PythonBinaryBuilder {
//     /// Clone self into a Box'ed trait object.
//     fn clone_trait(&self) -> Arc<dyn PythonBinaryBuilder>;
//
//     /// The name of the binary.
//     fn name(&self) -> String;
//
//     /// How the binary will link against libpython.
//     fn libpython_link_mode(&self) -> LibpythonLinkMode;
//
//     /// Rust target triple the binary will run on.
//     fn target_triple(&self) -> &str;
//
//     /// Obtain run-time requirements for the Visual C++ Redistributable.
//     fn vc_runtime_requirements(&self) -> Option<(String, VcRedistributablePlatform)>;
//
//     /// Obtain the cache tag to apply to Python bytecode modules.
//     fn cache_tag(&self) -> &str;
//
//      fn python_packaging_policy(&self) -> &PythonPackagingPolicy;
//
//     /// Path to Python executable that can be used to derive info at build time.
//     ///
//     /// The produced binary is effectively a clone of the Python distribution behind the
//     /// returned executable.
//     fn host_python_exe_path(&self) -> &Path;
//
//     /// Path to Python executable that is native to the target architecture.
//     // TODO this should not need to exist if we properly supported cross-compiling.
//     fn target_python_exe_path(&self) -> &Path;
//
//     /// Apple SDK build/targeting information.
//     fn apple_sdk_info(&self) -> Option<&AppleSdkInfo>;
//
//     /// Obtain how Windows runtime DLLs will be handled during builds.
//     ///
//     /// See the enum's documentation for behavior.
//     ///
//     /// This setting is ignored for binaries that don't need the Windows runtime
//     /// DLLs.
//     fn windows_runtime_dlls_mode(&self) -> &WindowsRuntimeDllsMode;
//
//     /// The directory to install tcl/tk files into.
//     fn tcl_files_path(&self) -> &Option<String>;
//
//     /// Set the directory to install tcl/tk files into.
//     fn set_tcl_files_path(&mut self, value: Option<String>);
//
//     /// Obtain the path of a filename to write containing a licensing report.
//     fn licenses_filename(&self) -> Option<&str>;
//
//     /// Set the path of a filename to write containing a licensing report.
//     fn set_licenses_filename(&mut self, value: Option<String>);
//
//     /// How packed Python resources will be loaded by the binary.
//     fn packed_resources_load_mode(&self) -> &PackedResourcesLoadMode;
//
//     /// Set how packed Python resources will be loaded by the binary.
//     fn set_packed_resources_load_mode(&mut self, load_mode: PackedResourcesLoadMode);
//
//     fn iter_resources<'a>(
//         &'a self,
//     ) -> Box<dyn Iterator<Item = (&'a String, &'a PrePackagedResource)> + 'a>;
//
//     fn index_package_license_info_from_resources<'a>(
//         &mut self,
//         resources: &[PythonResource<'a>],
//     ) -> Result<()>;
//
//     fn pip_download(
//         &mut self,
//         env: &Environment,
//         verbose: bool,
//         args: &[String],
//     ) -> Result<Vec<PythonResource>>;
//
//     fn pip_install(
//         &mut self,
//         env: &Environment,
//         verbose: bool,
//         install_args: &[String],
//         extra_envs: &HashMap<String, String>,
//     ) -> Result<Vec<PythonResource>>;
//
//     fn read_package_root(
//         &mut self,
//         path: &Path,
//         packages: &[String],
//     ) -> Result<Vec<PythonResource>>;
//     }

/// A self-contained Python executable before it is compiled.
#[derive(Clone)]
pub struct StandalonePythonExecutableBuilder {
    /// The target triple we are running on.
    host_triple: String,

    /// The target triple we are building for.
    target_triple: String,

    /// The name of the executable to build.
    exe_name: String,

    // /// The Python distribution being used to build this executable.
    // host_distribution: Arc<dyn PythonDistribution>,

    /// The Python distribution this executable is targeting.
    target_distribution: StandaloneDistribution,
    // target_distribution: Arc<StandaloneDistribution>,

    /// How libpython should be linked.
    link_mode: LibpythonLinkMode,

    /// Whether the built binary is capable of loading dynamically linked
    /// extension modules from memory.
    #[allow(dead_code)]
    supports_in_memory_dynamically_linked_extension_loading: bool,

    /// Policy to apply to added resources.
    packaging_policy: PythonPackagingPolicy,

    /// Python resources to be embedded in the binary.
    resources_collector: PythonResourceCollector,

    // /// How packed resources will be loaded at run-time.
    // resources_load_mode: PackedResourcesLoadMode,
    //
    // /// Holds state necessary to link libpython.
    // core_build_context: LibPythonBuildContext,

    // /// Holds linking context for individual extensions.
    // ///
    // /// We need to track per-extension state separately since we need
    // /// to support filtering extensions as part of building.
    // extension_build_contexts: BTreeMap<String, LibPythonBuildContext>,

    /// Configuration of the embedded Python interpreter.
    config: PyembedPythonInterpreterConfig,

    // /// Path to python executable that can be invoked at build time.
    // host_python_exe: PathBuf,

    /// Filename to write out with licensing information.
    licenses_filename: Option<String>,
    windows_subsystem: String,

    /// Path to install tcl/tk files into.
    tcl_files_path: Option<String>,

    // /// Describes how Windows runtime DLLs should be handled during builds.
    // windows_runtime_dlls_mode: WindowsRuntimeDllsMode,
}

impl StandalonePythonExecutableBuilder {
    fn add_distribution_core_state(&mut self) -> eyre::Result<()> {
        // self.core_build_context.inittab_cflags =
        //     Some(self.target_distribution.inittab_cflags.clone());
        //
        // for (name, path) in &self.target_distribution.includes {
        //     self.core_build_context
        //         .includes
        //         .insert(PathBuf::from(name), FileData::Path(path.clone()));
        // }
        //
        // // Add the distribution's object files from Python core to linking context.
        // for fs_path in self.target_distribution.objs_core.values() {
        //     // libpython generation derives its own PyImport_Inittab So ignore
        //     // the object file containing it.
        //     if fs_path == &self.target_distribution.inittab_object {
        //         continue;
        //     }
        //
        //     self.core_build_context
        //         .object_files
        //         .push(FileData::Path(fs_path.clone()));
        // }
        //
        // for entry in &self.target_distribution.links_core {
        //     if entry.framework {
        //         self.core_build_context
        //             .frameworks
        //             .insert(entry.name.clone());
        //     } else if entry.system {
        //         self.core_build_context
        //             .system_libraries
        //             .insert(entry.name.clone());
        //     }
        //     // TODO handle static/dynamic libraries.
        // }
        //
        // for path in self.target_distribution.libraries.values() {
        //     self.core_build_context.library_search_paths.insert(
        //         path.parent()
        //             .ok_or_else(|| anyhow!("unable to resolve parent directory"))?
        //             .to_path_buf(),
        //     );
        // }
        //
        // // Windows requires dynamic linking against msvcrt. Ensure that happens.
        // if crate::environment::WINDOWS_TARGET_TRIPLES.contains(&self.target_triple.as_str()) {
        //     self.core_build_context
        //         .system_libraries
        //         .insert("msvcrt".to_string());
        // }
        //
        // if let Some(component) = &self.target_distribution.core_license {
        //     self.core_build_context
        //         .licensed_components
        //         .add_component(component.clone());
        // }

        Ok(())
    }

    pub fn to_embedded_python_context(
        &self,
        // env: &Environment,
        opt_level: &str,
    ) -> eyre::Result<EmbeddedPythonContext> {
        let mut file_seen = false;
        for module in self.resources_collector.find_dunder_file()? {
            file_seen = true;
            log::warn!("warning: {} contains __file__", module);
        }

        if file_seen {
            log::warn!("__file__ was encountered in some embedded modules");
            log::warn!("PyOxidizer does not set __file__ and this may create problems at run-time");
            log::warn!("See https://github.com/indygreg/PyOxidizer/issues/69 for more");
        }

        // let compiled_resources = {
        //     let temp_dir = env.temporary_directory("pyoxidizer-bytecode-compile")?;
        //     let mut compiler = BytecodeCompiler::new(self.host_python_exe_path(), temp_dir.path())?;
        //     let resources = self.resources_collector.compile_resources(&mut compiler)?;
        //
        //     temp_dir.close().context("closing temporary directory")?;
        //
        //     resources
        // };

        let mut pending_resources = vec![];

        let mut extra_files = compiled_resources.extra_files_manifest()?;

        let mut config = self.config.clone();

        match &self.resources_load_mode {
            PackedResourcesLoadMode::None => {}
            PackedResourcesLoadMode::EmbeddedInBinary(filename) => {
                pending_resources.push((compiled_resources, PathBuf::from(filename)));
                config
                    .packed_resources
                    .push(PyembedPackedResourcesSource::MemoryIncludeBytes(
                        PathBuf::from(filename),
                    ));
            }
            PackedResourcesLoadMode::BinaryRelativePathMemoryMapped(path) => {
                // We need to materialize the file in extra_files. So compile now.
                let mut buffer = vec![];
                compiled_resources
                    .write_packed_resources(&mut buffer)
                    .context("serializing packed resources")?;
                extra_files.add_file_entry(Path::new(path), buffer)?;

                config
                    .packed_resources
                    .push(PyembedPackedResourcesSource::MemoryMappedPath(
                        PathBuf::from("$ORIGIN").join(path),
                    ));
            }
        }

        let link_settings = self.resolve_python_link_settings(env, opt_level)?;

        if self.link_mode == LibpythonLinkMode::Dynamic {
            if let Some(p) = &self.target_distribution.libpython_shared_library {
                let manifest_path = Path::new(p.file_name().unwrap());
                let content = std::fs::read(p)?;

                extra_files.add_file_entry(manifest_path, content)?;

                // Always look for and add the python3.dll variant if it exists. This DLL
                // exports the stable subset of the Python ABI and it is required by some
                // extensions.
                let python3_dll_path = p.with_file_name("python3.dll");
                let manifest_path = Path::new(python3_dll_path.file_name().unwrap());
                if python3_dll_path.exists() {
                    let content = std::fs::read(&python3_dll_path)?;

                    extra_files.add_file_entry(manifest_path, content)?;
                }
            }
        }

        if let Some(tcl_files_path) = self.tcl_files_path() {
            for (path, location) in self.target_distribution.tcl_files()? {
                let install_path = PathBuf::from(tcl_files_path).join(path);

                extra_files.add_file_entry(&install_path, location)?;
            }
        }

        // Install Windows runtime DLLs if told to do so.
        extra_files.add_manifest(&self.resolve_windows_runtime_dll_files()?)?;

        let python_implementation = if self
            .target_distribution
            .python_implementation
            .starts_with("cpython")
        {
            PythonImplementation::CPython
        } else if self
            .target_distribution
            .python_implementation
            .starts_with("pypy")
        {
            PythonImplementation::PyPy
        } else {
            return Err(anyhow!(
                "unknown Python implementation: {}",
                self.target_distribution.python_implementation
            ));
        };

        let python_version =
            PythonVersion::from_str(&self.target_distribution.python_major_minor_version())
                .map_err(|e| anyhow!("unable to determine Python version: {}", e))?;

        // Populate build flags that influence PyO3 configuration.
        let mut python_build_flags = BuildFlags::new();

        if self
            .target_distribution
            .python_config_vars()
            .get("Py_DEBUG")
            == Some(&"1".to_string())
        {
            python_build_flags.0.insert(BuildFlag::Py_DEBUG);
        }
        if self
            .target_distribution
            .python_config_vars()
            .get("Py_REF_DEBUG")
            == Some(&"1".to_string())
        {
            python_build_flags.0.insert(BuildFlag::Py_REF_DEBUG);
        }
        if self
            .target_distribution
            .python_config_vars()
            .get("Py_TRACE_REFS")
            == Some(&"1".to_string())
        {
            python_build_flags.0.insert(BuildFlag::Py_TRACE_REFS);
        }
        if self
            .target_distribution
            .python_config_vars()
            .get("COUNT_ALLOCS")
            == Some(&"1".to_string())
        {
            python_build_flags.0.insert(BuildFlag::COUNT_ALLOCS);
        }

        let mut context = EmbeddedPythonContext {
            config,
            link_settings,
            pending_resources,
            extra_files,
            host_triple: self.host_triple.clone(),
            target_triple: self.target_triple.clone(),
            python_implementation,
            python_version,
            python_exe_host: self.host_python_exe.clone(),
            python_build_flags,
            licensing_filename: self.licenses_filename.clone(),
            licensing: self.licensed_components()?,
        };

        context.synchronize_licensing()?;

        Ok(context)
    }
}

/// Generate artifacts for embedding Python in a binary.
pub fn generate_python_embedding_artifacts(
    // env: &Environment,
    // target_triple: &str,
    // flavor: &str,
    // python_version: Option<&str>,
    dest_path: &Path,
) -> eyre::Result<()> {
    // let flavor = DistributionFlavor::try_from(flavor)?;
        // .map_err(|e| eyre::eyre!("{}", e))?;

    std::fs::create_dir_all(dest_path)
        .wrap_err_with(|| format!("creating directory {}", dest_path.display()))?;

    let dest_path = canonicalize_path(dest_path).wrap_err("cannot canonicalize destination directory")?;

    // let distribution_record = PYTHON_DISTRIBUTIONS
    //     .find_distribution(target_triple, &flavor, python_version)
    //     .ok_or_else(|| anyhow!("could not find Python distribution matching requirements"))?;

    // let distribution_cache = DistributionCache::new(Some(&env.python_distributions_dir()));

    

    // let dist = StandaloneDistribution::from_location(location, dest_dir)?;
    let dist = PathBuf::from("/Users/roman/Downloads/cpython-3.12.3+20240415-x86_64-apple-darwin-pgo+lto-full");
    let dist = StandaloneDistribution::from_directory(&dist)?;

    // let target_dist = dist
    //     .resolve_distribution(&distribution_record.location, None)
    //     .context("resolving Python distribution")?;

    // let host_dist = dist
    //     .host_distribution(Some(dist.python_major_minor_version().as_str()), None)
    //     .wrap_err("resolving host distribution")?;

    let packaging_policy = dist
        .create_packaging_policy()
        .context("creating packaging policy")?;
    dbg!(&packaging_policy);

    let mut interpreter_config = dist
        .create_python_interpreter_config()
        .context("creating Python interpreter config")?;
    dbg!(&interpreter_config);

    interpreter_config.config.profile = PythonInterpreterProfile::Python;
    interpreter_config.allocator_backend = MemoryAllocatorBackend::Default;

    // dbg!(
    // let mut builder = dist.as_python_executable_builder(
    //     &dist.target_triple,
    //     &dist.target_triple,
    //     // default_target_triple(),
    //     // target_triple,
    //     "python",
    //     BinaryLibpythonLinkMode::Default,
    //     &policy,
    //     &interpreter_config,
    //     None,
    //     // Some(host_dist.clone_trait()),
    // )?;
    //
    
    let link_mode = LibpythonLinkMode::Static;
    let supports_in_memory_dynamically_linked_extension_loading =
        dist.supports_in_memory_shared_library_loading();

        let mut allowed_locations = vec![AbstractResourceLocation::from(
            &packaging_policy.resources_location,
        )];
        if let Some(fallback) = packaging_policy.resources_location_fallback() {
            allowed_locations.push(AbstractResourceLocation::from(fallback));
        }

        let mut allowed_extension_module_locations = vec![];

        if supports_in_memory_dynamically_linked_extension_loading
            && packaging_policy.allow_in_memory_shared_library_loading()
        {
            allowed_extension_module_locations.push(AbstractResourceLocation::InMemory);
        }

        if dist.is_extension_module_file_loadable() {
            allowed_extension_module_locations.push(AbstractResourceLocation::RelativePath);
        }

        let allow_new_builtin_extension_modules = link_mode == LibpythonLinkMode::Static;

        // let host_python_exe = host_dist.python_exe_path().to_path_buf();

        let target_distribution = dist.clone();
        let mut builder = Box::new(StandalonePythonExecutableBuilder {
            host_triple: dist.target_triple.clone(),
            target_triple: dist.target_triple,
            exe_name: "python".to_string(),
            // host_distribution: dist,
            target_distribution,
            link_mode,
            supports_in_memory_dynamically_linked_extension_loading,
            packaging_policy: packaging_policy.clone(),
            resources_collector: PythonResourceCollector::new(
                allowed_locations,
                allowed_extension_module_locations,
                allow_new_builtin_extension_modules,
                packaging_policy.allow_files(),
            ),
            // resources_load_mode: PackedResourcesLoadMode::EmbeddedInBinary(
            //     "packed-resources".to_string(),
            // ),
            // core_build_context: LibPythonBuildContext::default(),
            // extension_build_contexts: BTreeMap::new(),
            config: interpreter_config,
            // host_python_exe,
            licenses_filename: Some("COPYING.txt".into()),
            windows_subsystem: "console".to_string(),
            tcl_files_path: None,
            // windows_runtime_dlls_mode: WindowsRuntimeDllsMode::WhenPresent,
        });
       
        builder.add_distribution_core_state()?;

        // Ok(builder)

    // builder.set_tcl_files_path(Some("tcl".to_string()));
    //
    // builder
    //     .add_distribution_resources(None)
    //     .context("adding distribution resources")?;

    let embedded_context = builder
        .to_embedded_python_context("1")
        .context("resolving embedded context")?;

    // embedded_context
    //     .write_files(&dest_path)
    //     .context("writing embedded artifact files")?;
    //
    // embedded_context
    //     .extra_files
    //     .materialize_files(&dest_path)
    //     .context("writing extra files")?;
    //
    // // Write out a copy of the standard library.
    // let mut m = FileManifest::default();
    // for resource in find_python_resources(
    //     &dist.stdlib_path,
    //     dist.cache_tag(),
    //     &dist.python_module_suffixes()?,
    //     true,
    //     false,
    // )? {
    //     if let PythonResource::File(file) = resource? {
    //         m.add_file_entry(file.path(), file.entry())?;
    //     } else {
    //         panic!("find_python_resources() should only emit File variant");
    //     }
    // }
    //
    // m.materialize_files_with_replace(dest_path.join("stdlib"))
    //     .context("writing standard library")?;

    Ok(())
}

/// The default target triple to build for.
///
/// This typically matches the triple of the current binary. But in some
/// cases we remap to a more generic target.
pub fn default_target_triple() -> String {
    match std::env::var("TARGET").unwrap().as_str() {
        // Release binaries are typically musl. But Linux GNU is a more
        // user friendly target to build for. So we perform this mapping.
        "aarch64-unknown-linux-musl" => "aarch64-unknown-linux-gnu".to_string(),
        "x86_64-unknown-linux-musl" => "x86_64-unknown-linux-gnu".to_string(),
        v => v.to_string(),
    }
}

// /// Denotes how a binary should link libpython.
// #[derive(Clone, Debug, PartialEq, Eq)]
// pub enum BinaryLibpythonLinkMode {
//     /// Use default link mode semantics.
//     Default,
//     /// Statically link libpython into the binary.
//     Static,
//     /// Binary should dynamically link libpython.
//     Dynamic,
// }

/// How a binary should link against libpython.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibpythonLinkMode {
    /// Libpython will be statically linked into the binary.
    Static,
    /// The binary will dynamically link against libpython.
    Dynamic,
}

/// Describes the location of a Python resource.
///
/// The location is abstract because a concrete location (such as the
/// relative path) is not specified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbstractResourceLocation {
    /// Resource is loaded from memory.
    InMemory,
    /// Resource is loaded from a relative filesystem path.
    RelativePath,
}
