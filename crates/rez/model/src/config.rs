//! Rez configuration system with 100+ config keys.
//!
//! Config loading order: defaults → site rezconfig → user rezconfig → env vars.
//! Paths stored as strings; use `expand_path` / `expanded_packages_path` for `RezPath`, or
//! `expanded_packages_path_os` for `PathBuf` at fs/Command boundary.

use std::collections::{HashMap, HashSet};

use crate::{log_debug, log_info, log_trace};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::constants::{RezToolsVisibility, SuiteVisibility, VariantSelectMode};

// ---------------------------------------------------------------------------
// Enums for config-specific settings
// ---------------------------------------------------------------------------

/// Script creation mode
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExecutableScriptMode {
    /// Single combined script
    #[default]
    Single,
    /// Python script only
    Py,
    /// Platform-specific scripts
    PlatformSpecific,
    /// Both .py and platform-specific
    Both,
}

/// Package preprocess ordering
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PreprocessMode {
    /// Preprocess runs before package definition
    Before,
    /// Preprocess runs after package definition
    After,
    /// Preprocess overrides package definition
    #[default]
    Override,
}

/// Build thread count: specific number or auto-detect
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum BuildThreadCount {
    /// Fixed number of threads
    Count(usize),
    /// Auto-detect: "physical_cores" or "logical_cores"
    Auto(String),
}

impl<'de> Deserialize<'de> for BuildThreadCount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Number(number) => number
                .as_u64()
                .and_then(|count| usize::try_from(count).ok())
                .filter(|count| *count > 0)
                .map(Self::Count)
                .ok_or_else(|| {
                    serde::de::Error::custom("build_thread_count must be a positive integer")
                }),
            serde_json::Value::String(name)
                if matches!(name.as_str(), "physical_cores" | "logical_cores") =>
            {
                Ok(Self::Auto(name))
            }
            _ => Err(serde::de::Error::custom(
                "build_thread_count must be a positive integer, physical_cores, or logical_cores",
            )),
        }
    }
}

impl Default for BuildThreadCount {
    fn default() -> Self {
        Self::Auto("physical_cores".into())
    }
}

impl BuildThreadCount {
    /// Resolve to actual thread count
    pub fn resolve(&self) -> usize {
        match self {
            Self::Count(n) => *n,
            Self::Auto(s) => match s.as_str() {
                "logical_cores" => crate::platform::logical_cores(),
                _ => crate::platform::physical_cores(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Color style settings
// ---------------------------------------------------------------------------

/// Color configuration for a log level or element
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColorStyle {
    pub fore: Option<String>,
    pub back: Option<String>,
    pub styles: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Main config struct
// ---------------------------------------------------------------------------

/// Complete rez configuration with all ~150 keys.
/// All fields have sensible defaults matching Python rez's rezconfig.py.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RezConfig {
    // -- Recipe environment (rez-rs additions) --
    #[serde(default)]
    pub sources_path: Option<String>,
    #[serde(default)]
    pub wheel_cache_path: Option<String>,
    #[serde(default)]
    pub user_path: Option<String>,
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub offline: bool,
    #[serde(default, deserialize_with = "deserialize_log_level")]
    pub log_level: Option<String>,

    // -- Paths --
    pub packages_path: Vec<String>,
    pub local_packages_path: String,
    pub release_packages_path: String,
    /// Per-builder release paths. When set, releases go here; else fallback to release_packages_path.
    pub release_bind_path: Option<String>,
    pub release_pip_path: Option<String>,
    pub release_build_path: Option<String>,
    /// Route package installs into category repositories such as int/ext/dcc/pip/tool.
    pub rez_install_categories: bool,
    /// Install local builds into the configured release repository instead of the local repository.
    pub rez_install_location: bool,
    /// Tag-to-subdirectory mapping. Packages with a matching tag install into the mapped subdir.
    pub tag_paths: HashMap<String, String>,
    pub tmpdir: Option<String>,
    pub context_tmpdir: Option<String>,
    pub package_definition_build_python_paths: Vec<String>,
    pub package_definition_python_path: Option<String>,

    // -- Extensions --
    pub plugin_path: Vec<String>,
    pub bind_module_path: Vec<String>,

    // -- Caching --
    pub resolve_caching: bool,
    pub cache_package_files: bool,
    pub cache_listdir: bool,
    pub resource_caching_maxsize: i64,
    pub memcached_uri: Vec<String>,
    pub memcached_package_file_min_compress_len: usize,
    pub memcached_context_file_min_compress_len: usize,
    pub memcached_listdir_min_compress_len: usize,
    pub memcached_resolve_min_compress_len: usize,

    // -- Hashed variants --
    pub default_hashed_variants: bool,

    // -- Package relocatability --
    pub default_relocatable: bool,
    pub default_relocatable_per_package: Option<HashMap<String, Option<bool>>>,
    pub default_relocatable_per_repository: Option<HashMap<String, Option<bool>>>,

    // -- Package caching --
    pub default_cachable: Option<bool>,
    pub default_cachable_per_package: Option<HashMap<String, Option<bool>>>,
    pub default_cachable_per_repository: Option<HashMap<String, Option<bool>>>,
    pub cache_packages_path: Option<String>,
    pub read_package_cache: bool,
    pub write_package_cache: bool,
    pub package_cache_max_variant_days: u32,
    pub package_cache_during_build: bool,
    pub package_cache_async: bool,
    pub package_cache_local: bool,
    pub package_cache_same_device: bool,
    pub package_cache_clean_limit: f64,
    pub package_cache_log_days: u32,
    pub package_cache_space_buffer: u64,
    pub package_cache_used_threshold: u32,

    // -- Package resolution --
    pub implicit_packages: Vec<String>,
    pub platform_map: HashMap<String, crate::platform::PlatformRules>,
    /// Prepared runtime identity; not a user setting and never serialized.
    #[serde(skip)]
    #[doc(hidden)]
    pub system_info: Option<crate::platform::SystemInfo>,
    pub prune_failed_graph: bool,
    pub variant_select_mode: VariantSelectMode,
    pub package_filter: Option<serde_json::Value>,
    pub package_orderers: Option<serde_json::Value>,
    pub allow_unversioned_packages: bool,
    pub error_on_missing_variant_requires: bool,

    // -- Environment resolution --
    pub parent_variables: Vec<String>,
    pub all_parent_variables: bool,
    /// Start resolved shells from the OS baseline and explicit parent allowlist.
    pub clean_shell_environment: bool,
    /// Append system paths to PATH after all package commands.
    pub append_sys_path: bool,
    pub resetting_variables: Vec<String>,
    pub all_resetting_variables: bool,
    pub default_shell: String,
    pub terminal_emulator_command: Option<String>,
    pub new_session_popen_args: Option<HashMap<String, serde_json::Value>>,
    pub env_var_separators: HashMap<String, String>,
    pub pathed_env_vars: Vec<String>,
    pub suite_visibility: SuiteVisibility,
    pub rez_tools_visibility: RezToolsVisibility,
    pub package_commands_sourced_first: bool,
    /// System PATH entries appended by `append_sys_path`; empty means the OS defaults.
    pub standard_system_paths: Vec<String>,

    // -- Build/release --
    pub package_preprocess_function: Option<String>,
    pub package_preprocess_mode: PreprocessMode,
    pub build_directory: String,
    pub build_thread_count: BuildThreadCount,
    pub release_hooks: Vec<String>,
    pub prompt_release_message: bool,
    pub make_package_temporarily_writable: bool,
    pub variant_shortlinks_dirname: Option<String>,
    pub use_variant_shortlinks: bool,
    pub default_build_process: String,

    // -- Suites --
    pub suite_alias_prefix_char: char,

    // -- Context tracking --
    pub context_tracking_host: String,
    pub context_tracking_context_fields: Vec<String>,
    pub context_tracking_extra_fields: HashMap<String, serde_json::Value>,
    pub context_tracking_amqp: HashMap<String, serde_json::Value>,

    // -- Debugging --
    pub rez_1_environment_variables: bool,
    pub disable_rez_1_compatibility: bool,
    pub error_old_commands: bool,
    pub warn_old_commands: bool,
    pub debug_old_commands: bool,
    pub warn_shell_startup: bool,
    pub warn_untimestamped: bool,
    pub warn_all: bool,
    pub warn_none: bool,
    pub debug_file_loads: bool,
    pub debug_plugins: bool,
    pub debug_package_release: bool,
    pub debug_bind_modules: bool,
    pub debug_resources: bool,
    pub debug_package_exclusions: bool,
    pub debug_resolve_memcache: bool,
    pub debug_memcache: bool,
    pub debug_context_tracking: bool,
    pub debug_all: bool,
    pub debug_none: bool,
    pub catch_rex_errors: bool,
    pub shell_error_truncate_cap: usize,

    // -- Appearance --
    pub quiet: bool,
    pub show_progress: bool,
    pub editor: Option<String>,
    pub image_viewer: Option<String>,
    pub browser: Option<String>,
    pub difftool: Option<String>,
    pub dot_image_format: String,
    pub set_prompt: bool,
    pub prefix_prompt: bool,
    pub color_enabled: bool,

    // -- Colorization --
    pub critical_color: ColorStyle,
    pub error_color: ColorStyle,
    pub warning_color: ColorStyle,
    pub info_color: ColorStyle,
    pub debug_color: ColorStyle,
    pub heading_color: ColorStyle,
    pub local_color: ColorStyle,
    pub implicit_color: ColorStyle,
    pub ephemeral_color: ColorStyle,
    pub alias_color: ColorStyle,

    // -- Misc --
    pub max_package_changelog_chars: usize,
    pub max_package_changelog_revisions: usize,
    pub create_executable_script_mode: ExecutableScriptMode,
    pub pip_default_python_version: Option<String>,
    pub pip_default_no_deps: bool,
    pub pip_install_prefix: Option<String>,
    pub pip_detect_system_markers: bool,
    pub pip_release_lock_backend: String,
    pub pip_extra_args: Vec<String>,
    pub pip_install_remaps: Vec<HashMap<String, String>>,
    pub optionvars: Option<HashMap<String, serde_json::Value>>,
    pub plugins: HashMap<String, serde_json::Value>,
    pub documentation_url: String,
}

// ---------------------------------------------------------------------------
// Default implementation (matches Python rezconfig.py)
// ---------------------------------------------------------------------------

impl Default for RezConfig {
    fn default() -> Self {
        Self {
            sources_path: None,
            wheel_cache_path: None,
            user_path: None,
            repo_path: None,
            offline: false,
            log_level: None,
            // Paths
            packages_path: vec![
                "~/.rez/packages/local/int".into(),
                "~/.rez/packages/int".into(),
                "~/.rez/packages/ext".into(),
            ],
            local_packages_path: "~/.rez/packages/local/int".into(),
            release_packages_path: "~/.rez/packages/int".into(),
            release_bind_path: Some("~/.rez/packages/bind".into()),
            release_pip_path: Some("~/.rez/packages/pip".into()),
            release_build_path: None,
            rez_install_categories: true,
            rez_install_location: true,
            tag_paths: HashMap::new(),
            tmpdir: None,
            context_tmpdir: None,
            package_definition_build_python_paths: vec![],
            package_definition_python_path: None,

            // Extensions
            plugin_path: vec![],
            bind_module_path: vec![],

            // Caching
            resolve_caching: true,
            cache_package_files: true,
            cache_listdir: true,
            resource_caching_maxsize: -1,
            memcached_uri: vec![],
            memcached_package_file_min_compress_len: 16384,
            memcached_context_file_min_compress_len: 1,
            memcached_listdir_min_compress_len: 16384,
            memcached_resolve_min_compress_len: 1,

            // Hashed variants
            default_hashed_variants: crate::constants::DEFAULT_HASHED_VARIANTS,

            // Relocatability
            default_relocatable: true,
            default_relocatable_per_package: None,
            default_relocatable_per_repository: None,

            // Package caching
            default_cachable: Some(false),
            default_cachable_per_package: None,
            default_cachable_per_repository: None,
            cache_packages_path: None,
            read_package_cache: true,
            write_package_cache: true,
            package_cache_max_variant_days: 30,
            package_cache_during_build: false,
            package_cache_async: true,
            package_cache_local: false,
            package_cache_same_device: false,
            package_cache_clean_limit: 0.5,
            package_cache_log_days: 7,
            package_cache_space_buffer: 104_857_600, // 100 MB
            package_cache_used_threshold: 80,

            // Resolution
            implicit_packages: vec![
                "~platform=={system.platform}".into(),
                "~arch=={system.arch}".into(),
                "~os=={system.os}".into(),
            ],
            platform_map: HashMap::new(),
            system_info: None,
            prune_failed_graph: true,
            variant_select_mode: VariantSelectMode::VersionPriority,
            package_filter: None,
            package_orderers: None,
            allow_unversioned_packages: true,
            error_on_missing_variant_requires: true,

            // Environment
            parent_variables: vec![],
            all_parent_variables: false,
            clean_shell_environment: false,
            append_sys_path: true,
            resetting_variables: vec![],
            all_resetting_variables: false,
            default_shell: String::new(),
            terminal_emulator_command: None,
            new_session_popen_args: None,
            env_var_separators: HashMap::from([
                ("CMAKE_MODULE_PATH".into(), ";".into()),
                ("DOXYGEN_TAGFILES".into(), " ".into()),
            ]),
            pathed_env_vars: vec!["*PATH".into()],
            suite_visibility: SuiteVisibility::Always,
            rez_tools_visibility: RezToolsVisibility::Append,
            package_commands_sourced_first: true,
            standard_system_paths: vec![],

            // Build/release
            package_preprocess_function: None,
            package_preprocess_mode: PreprocessMode::Override,
            build_directory: "build".into(),
            build_thread_count: BuildThreadCount::default(),
            release_hooks: vec![],
            prompt_release_message: false,
            make_package_temporarily_writable: true,
            variant_shortlinks_dirname: Some("_v".into()),
            use_variant_shortlinks: true,
            default_build_process: "local".into(),

            // Suites
            suite_alias_prefix_char: '+',

            // Context tracking
            context_tracking_host: String::new(),
            context_tracking_context_fields: vec![
                "status".into(),
                "timestamp".into(),
                "solve_time".into(),
                "load_time".into(),
                "from_cache".into(),
                "package_requests".into(),
                "implicit_packages".into(),
                "resolved_packages".into(),
            ],
            context_tracking_extra_fields: HashMap::new(),
            context_tracking_amqp: HashMap::from([
                ("userid".into(), serde_json::Value::String(String::new())),
                ("password".into(), serde_json::Value::String(String::new())),
                ("connect_timeout".into(), serde_json::json!(10)),
                (
                    "exchange_name".into(),
                    serde_json::Value::String(String::new()),
                ),
                (
                    "exchange_routing_key".into(),
                    serde_json::Value::String("REZ.CONTEXT".into()),
                ),
                ("message_delivery_mode".into(), serde_json::json!(1)),
            ]),

            // Debugging
            rez_1_environment_variables: false,
            disable_rez_1_compatibility: true,
            error_old_commands: false,
            warn_old_commands: true,
            debug_old_commands: false,
            warn_shell_startup: false,
            warn_untimestamped: false,
            warn_all: false,
            warn_none: false,
            debug_file_loads: false,
            debug_plugins: false,
            debug_package_release: false,
            debug_bind_modules: false,
            debug_resources: false,
            debug_package_exclusions: false,
            debug_resolve_memcache: false,
            debug_memcache: false,
            debug_context_tracking: false,
            debug_all: false,
            debug_none: false,
            catch_rex_errors: true,
            shell_error_truncate_cap: 750,

            // Appearance
            quiet: false,
            show_progress: true,
            editor: None,
            image_viewer: None,
            browser: None,
            difftool: None,
            dot_image_format: "png".into(),
            set_prompt: true,
            prefix_prompt: true,
            color_enabled: cfg!(unix),

            // Colorization
            critical_color: ColorStyle {
                fore: Some("red".into()),
                back: None,
                styles: Some(vec!["bright".into()]),
            },
            error_color: ColorStyle {
                fore: Some("red".into()),
                back: None,
                styles: None,
            },
            warning_color: ColorStyle {
                fore: Some("yellow".into()),
                back: None,
                styles: None,
            },
            info_color: ColorStyle {
                fore: Some("green".into()),
                back: None,
                styles: None,
            },
            debug_color: ColorStyle {
                fore: Some("blue".into()),
                back: None,
                styles: None,
            },
            heading_color: ColorStyle {
                fore: None,
                back: None,
                styles: Some(vec!["bright".into()]),
            },
            local_color: ColorStyle {
                fore: Some("green".into()),
                back: None,
                styles: None,
            },
            implicit_color: ColorStyle {
                fore: Some("cyan".into()),
                back: None,
                styles: None,
            },
            ephemeral_color: ColorStyle {
                fore: Some("blue".into()),
                back: None,
                styles: None,
            },
            alias_color: ColorStyle {
                fore: Some("cyan".into()),
                back: None,
                styles: None,
            },

            // Misc
            max_package_changelog_chars: 65536,
            max_package_changelog_revisions: 0,
            create_executable_script_mode: ExecutableScriptMode::Single,
            pip_default_python_version: None,
            pip_default_no_deps: false,
            pip_install_prefix: None,
            pip_detect_system_markers: false,
            pip_release_lock_backend: String::new(),
            pip_extra_args: vec![],
            pip_install_remaps: vec![
                HashMap::from([
                    ("record_path".into(), r"^{p}{s}{p}{s}(bin{s}.*)".into()),
                    ("pip_install".into(), r"\1".into()),
                    ("rez_install".into(), r"\1".into()),
                ]),
                HashMap::from([
                    (
                        "record_path".into(),
                        r"^{p}{s}{p}{s}lib{s}python{s}(.*)".into(),
                    ),
                    ("pip_install".into(), r"\1".into()),
                    ("rez_install".into(), r"python{s}\1".into()),
                ]),
                // pip --target relocates every wheel data-scheme directory under
                // its target, including share/ and include/. Keep their layout;
                // the publisher still validates paths and canonical containment.
                HashMap::from([
                    ("record_path".into(), r"^(?:{p}{s})+(.+)".into()),
                    ("pip_install".into(), r"\1".into()),
                    ("rez_install".into(), r"\1".into()),
                ]),
            ],
            optionvars: None,
            plugins: HashMap::new(),
            documentation_url: "https://rez.readthedocs.io".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

static CONFIG_SEED: OnceLock<RezConfig> = OnceLock::new();
static CONFIG_LOAD_ERRORS: OnceLock<String> = OnceLock::new();

/// Fail before executing package code or acquisition after invalid global settings.
pub fn ensure_valid() -> Result<(), crate::errors::RezError> {
    LazyLock::force(&CONFIG);
    match CONFIG_LOAD_ERRORS.get() {
        Some(errors) => Err(crate::errors::RezError::Config(errors.clone())),
        None => Ok(()),
    }
}
static CONFIG_INITIALIZATION: Mutex<bool> = Mutex::new(false);

/// Supply a typed worker snapshot before any interpreter or global configuration access.
pub fn initialize(mut config: RezConfig) -> Result<(), crate::errors::RezError> {
    let initialized = CONFIG_INITIALIZATION.lock().map_err(|_| {
        crate::errors::RezError::Config("Configuration initialization lock is poisoned".into())
    })?;
    if *initialized || CONFIG_SEED.get().is_some() {
        return Err(crate::errors::RezError::Config(
            "Configuration has already been initialized or seeded".into(),
        ));
    }
    // Serialized snapshots contain already expanded values but intentionally omit
    // the prepared runtime identity. Restore it without rereading files or env.
    config.system_info = Some(crate::platform::RAW_SYSTEM.mapped(&config.platform_map)?);
    CONFIG_SEED.set(config).map_err(|_| {
        crate::errors::RezError::Config("Configuration has already been seeded".into())
    })
}

/// Global configuration singleton
pub static CONFIG: LazyLock<RezConfig> = LazyLock::new(|| {
    let mut initialized = CONFIG_INITIALIZATION
        .lock()
        .expect("configuration initialization lock poisoned");
    *initialized = true;
    if let Some(config) = CONFIG_SEED.get() {
        return config.clone();
    }
    drop(initialized);
    log_info!("config", "Initializing config");
    let mut config = RezConfig::default();
    let mut errors = Vec::new();

    // Load from config files
    if let Err(error) = config.load_config_files() {
        errors.push(error.to_string());
    }

    // Apply environment variable overrides
    log_trace!("config", "Applying REZ_* env overrides");
    if let Err(error) = config.apply_env_overrides(None) {
        errors.push(error.to_string());
    }

    // Expand {system.*} and ${ENV} variables in all config values
    log_trace!("config", "Expanding system/env variables");
    if let Err(error) = config.expand_all_values() {
        errors.push(error.to_string());
    }

    if !errors.is_empty() {
        let _ = CONFIG_LOAD_ERRORS.set(errors.join("\n"));
    }

    log_info!(
        "config",
        "Config ready, packages_path len={}",
        config.packages_path.len()
    );
    config
});

impl RezConfig {
    /// Configured recipe controls exported through the shared Rex/build environment.
    /// Publication targets remain separate from the repository lookup list.
    pub fn recipe_environment(&self) -> HashMap<String, String> {
        let mut values = HashMap::new();
        for (name, path) in [
            ("REZ_SOURCES_PATH", &self.sources_path),
            ("REZ_WHEEL_CACHE_PATH", &self.wheel_cache_path),
            ("REZ_USER_PATH", &self.user_path),
            ("REZ_REPO_PATH", &self.repo_path),
        ] {
            if let Some(path) = path.as_ref().filter(|path| !path.trim().is_empty()) {
                values.insert(name.to_owned(), Self::expand_path(path).to_string());
            }
        }
        values.insert("REZ_OFFLINE".to_owned(), self.offline.to_string());
        if let Some(level) = &self.log_level {
            values.insert("REZ_LOG_LEVEL".to_owned(), level.to_ascii_uppercase());
        }
        values
    }

    /// Pip keeps an explicit find-links setting; configured local wheels are a fallback.
    pub fn pip_environment(&self, parent: &HashMap<String, String>) -> HashMap<String, String> {
        let mut environment = parent.clone();
        environment.extend(self.recipe_environment());
        if !environment.contains_key("PIP_FIND_LINKS") {
            let wheels = self
                .wheel_cache_path
                .as_ref()
                .map(|path| Self::expand_path(path).to_os())
                .or_else(|| {
                    self.sources_path
                        .as_ref()
                        .map(|path| Self::expand_path(path).to_os().join("wheels"))
                });
            if let Some(wheels) = wheels {
                environment.insert(
                    "PIP_FIND_LINKS".into(),
                    wheels.to_string_lossy().into_owned(),
                );
            }
        }
        if self.offline {
            environment.insert("PIP_NO_INDEX".into(), "true".into());
            environment.insert("PIP_DISABLE_PIP_VERSION_CHECK".into(), "true".into());
            environment.remove("PIP_INDEX_URL");
            environment.remove("PIP_EXTRA_INDEX_URL");
        }
        environment
    }

    /// Variables retained by generated build launchers.
    pub fn is_recipe_environment_variable(name: &str) -> bool {
        matches!(
            name,
            "REZ_SOURCES_PATH"
                | "REZ_WHEEL_CACHE_PATH"
                | "REZ_USER_PATH"
                | "REZ_REPO_PATH"
                | "REZ_OFFLINE"
                | "REZ_LOG_LEVEL"
        ) || name.starts_with("REZ_PBS_")
    }

    /// Serialize default config to TOML string (for --default-config).
    pub fn default_config_toml() -> Result<String, crate::errors::RezError> {
        let config = Self::default();
        toml::to_string_pretty(&config).map_err(|e| {
            crate::errors::RezError::Config(format!("Failed to serialize config: {e}"))
        })
    }

    /// Serialize default config to Python rezconfig.py format (for --default-config).
    pub fn default_config_py() -> String {
        let c = Self::default();
        let fmt_list = |v: &[String]| {
            let items: Vec<String> = v.iter().map(|s| format!("    {s:?}")).collect();
            format!("[\n{}\n]", items.join(",\n"))
        };
        let fmt_str = |s: &str| format!("{s:?}");
        let fmt_bool = |b: bool| if b { "True" } else { "False" };

        let variant_select = serde_json::json!(c.variant_select_mode).to_string();
        let suite_vis = serde_json::json!(c.suite_visibility).to_string();
        let rez_vis = serde_json::json!(c.rez_tools_visibility).to_string();
        let preprocess_mode = serde_json::json!(c.package_preprocess_mode).to_string();
        let build_thread = match &c.build_thread_count {
            crate::config::BuildThreadCount::Count(n) => format!("{n}"),
            crate::config::BuildThreadCount::Auto(s) => fmt_str(s),
        };
        let shortlinks_dir = c
            .variant_shortlinks_dirname
            .as_deref()
            .map_or("None".into(), fmt_str);
        let suite_char = format!("{:?}", c.suite_alias_prefix_char);
        let default_build = fmt_str(&c.default_build_process);
        let use_shortlinks = fmt_bool(c.use_variant_shortlinks);
        let prompt_release = fmt_bool(c.prompt_release_message);
        let make_writable = fmt_bool(c.make_package_temporarily_writable);
        let std_system_paths = fmt_list(&c.standard_system_paths);
        let build_dir = fmt_str(&c.build_directory);
        let package_commands_sourced_first = fmt_bool(c.package_commands_sourced_first);
        let pathed_env_vars = fmt_list(&c.pathed_env_vars);
        let default_shell = fmt_str(&c.default_shell);
        let packages_path = fmt_list(&c.packages_path);
        let local_packages_path = fmt_str(&c.local_packages_path);
        let release_packages_path = fmt_str(&c.release_packages_path);
        let rez_install_categories = fmt_bool(c.rez_install_categories);
        let rez_install_location = fmt_bool(c.rez_install_location);
        let memcached_uri = fmt_list(&c.memcached_uri);
        let implicit_packages = fmt_list(&c.implicit_packages);
        let default_hashed_variants = fmt_bool(c.default_hashed_variants);
        format!(
            r#"# Rez configuration - default values
# Generated by: rez --write-config
# See: https://github.com/AcademySoftwareFoundation/rez

# Recipe archive mirror (REZ_SOURCES_PATH); one directory, not a search list.
sources_path = None
# Python wheel cache (REZ_WHEEL_CACHE_PATH), retaining explicit PIP_FIND_LINKS.
wheel_cache_path = None
# User data root (REZ_USER_PATH); recipes place tool caches under cache/<tool>.
user_path = None
# Common publication root (REZ_REPO_PATH); CLI and per-builder targets take priority.
# This does not replace packages_path, which controls package lookup.
repo_path = None
# Managed downloads/installers honor REZ_OFFLINE=true/false; not a network sandbox.
offline = False
# Optional REZ_LOG_LEVEL: ERROR, WARNING, INFO, DEBUG, TRACE, or OFF.
log_level = None

# Package repository paths, searched in order. First match wins.
packages_path = {packages_path}
# Where rez-build installs packages locally.
local_packages_path = {local_packages_path}
# Where rez-release deploys packages. Use site-wide path for production.
release_packages_path = {release_packages_path}
# Route build/release installs by package_type within category repository roots.
rez_install_categories = {rez_install_categories}
# Send `rez build -i` to the release repository when enabled, local_packages_path otherwise.
# `rez release` always publishes to the release repository.
rez_install_location = {rez_install_location}
# Per-builder release paths, then repo_path, then release_packages_path.
release_bind_path = "~/.rez/packages/bind"
release_pip_path = "~/.rez/packages/pip"
release_build_path = None
# Tag-to-subdirectory mapping. Packages with a matching tag go to the mapped subdir.
# Example: tag_paths = {{ "dcc": "dcc", "studio": "studio/tools" }}
# Then a package with tags = ["dcc"] installs to <release_path>/dcc/<name>/<version>/.
# CLI --tag overrides this. First matching tag wins.
tag_paths = {tag_paths}
# Temp directory. None = system default (e.g. /tmp).
tmpdir = None
# Temp dir for context scripts. Separate from tmpdir (e.g. for NFS).
context_tmpdir = None
# Extra Python paths added during package build (preprocess, @early).
package_definition_build_python_paths = []
# Shared code dir for packages via @include(). Modules copied into install.
package_definition_python_path = None

# Search path for rez plugins.
plugin_path = []
# Search path for rez-bind modules (custom binders).
bind_module_path = []

# Cache resolves to memcached. Needs memcached_uri.
resolve_caching = {resolve_caching}
# Cache package file reads to memcached.
cache_package_files = {cache_package_files}
# Cache directory listings to memcached.
cache_listdir = {cache_listdir}
# In-process resource cache size (entries). -1 = unlimited, 0 = disabled.
resource_caching_maxsize = {resource_caching_maxsize}
# Memcached server(s). E.g. ["127.0.0.1:11211"]. Empty = memcached disabled.
memcached_uri = {memcached_uri}

# If True, variant subdirs use SHA1 hash. False = readable platform-windows/arch-x86_64/os-... paths.
default_hashed_variants = {default_hashed_variants}
# Whether packages can be moved. Affects bundle, copy, cache.
default_relocatable = {default_relocatable}
# Whether variants can be cached to local disk. None = use relocatable.
default_cachable = {default_cachable}
# Use cached variants when present in package cache.
read_package_cache = {read_package_cache}
# Cache variants when resolving/sourcing.
write_package_cache = {write_package_cache}
# Delete cached variants unused for N days. 0 = disable cleanup.
package_cache_max_variant_days = {package_cache_max_variant_days}
# Cache asynchronously (no blocking on resolve).
package_cache_async = {package_cache_async}
# Allow caching local packages (testing).
package_cache_local = {package_cache_local}
# Allow cache when source and cache on same device (testing).
package_cache_same_device = {package_cache_same_device}

# Auto-added to every resolve. Use {{system.platform}}, {{system.arch}}, {{system.os}}.
implicit_packages = {implicit_packages}
# Simplify failure messages by pruning unrelated packages from graph.
prune_failed_graph = {prune_failed_graph}
# "version_priority" or "intersection_priority". Which variant to prefer.
variant_select_mode = {variant_select}
# Allow packages without version in request.
allow_unversioned_packages = {allow_unversioned_packages}
# Fail immediately if variant requires missing package (vs trying other variants).
error_on_missing_variant_requires = {error_on_missing_variant_requires}

# Start resolved shells with OS defaults and only parent_variables inherited.
clean_shell_environment = {clean_shell_environment}
# Append system paths after all package commands, so packages shadow host tools.
append_sys_path = {append_sys_path}
# Default shell for rez-env. Empty = auto-detect (bash/cmd/PowerShell).
default_shell = {default_shell}
# Env vars treated as path lists (":" or ";" separator). "*PATH" matches all PATH-like.
pathed_env_vars = {pathed_env_vars}
# Suite visibility: "never" | "always" | "parent" | "parent_priority".
suite_visibility = {suite_vis}
# rez tools in PATH: "never" | "append" | "prepend".
rez_tools_visibility = {rez_vis}
# If True: source package commands before shell init (.bashrc etc). If False: after.
package_commands_sourced_first = {package_commands_sourced_first}
# System paths appended by append_sys_path. Empty = OS defaults.
standard_system_paths = {std_system_paths}

# Preprocess order: "before" | "after" | "override".
package_preprocess_mode = {preprocess_mode}
# Directory for rez-build. Relative to package.
build_directory = {build_dir}
# Build threads: number or "physical_cores" / "logical_cores".
build_thread_count = {build_thread}
# Hooks run on release.
release_hooks = []
# Prompt for release message.
prompt_release_message = {prompt_release}
# Make package writable during release.
make_package_temporarily_writable = {make_writable}
# Dir name for variant shortlinks (_v). None = disabled.
variant_shortlinks_dirname = {shortlinks_dir}
# Use shortlinks for faster variant lookup.
use_variant_shortlinks = {use_shortlinks}
# Default build process: "local" or plugin name.
default_build_process = {default_build}
# Prefix for suite aliases (e.g. '+' for +maya).
suite_alias_prefix_char = {suite_char}

# Rez-1 compatibility is disabled by default. Opt-in does not affect normal Rez-2 variables.
rez_1_environment_variables = {rez_1_environment_variables}
disable_rez_1_compatibility = {disable_rez_1_compatibility}
error_old_commands = {error_old_commands}
warn_old_commands = {warn_old_commands}
debug_old_commands = {debug_old_commands}
"#,
            packages_path = packages_path,
            tag_paths = crate::serialise::python_repr(&serde_json::json!(c.tag_paths)),
            local_packages_path = local_packages_path,
            release_packages_path = release_packages_path,
            rez_install_categories = rez_install_categories,
            rez_install_location = rez_install_location,
            resolve_caching = fmt_bool(c.resolve_caching),
            cache_package_files = fmt_bool(c.cache_package_files),
            cache_listdir = fmt_bool(c.cache_listdir),
            resource_caching_maxsize = c.resource_caching_maxsize,
            memcached_uri = memcached_uri,
            default_hashed_variants = default_hashed_variants,
            default_relocatable = fmt_bool(c.default_relocatable),
            default_cachable = c.default_cachable.map_or("None", fmt_bool),
            read_package_cache = fmt_bool(c.read_package_cache),
            write_package_cache = fmt_bool(c.write_package_cache),
            package_cache_max_variant_days = c.package_cache_max_variant_days,
            package_cache_async = fmt_bool(c.package_cache_async),
            package_cache_local = fmt_bool(c.package_cache_local),
            package_cache_same_device = fmt_bool(c.package_cache_same_device),
            implicit_packages = implicit_packages,
            prune_failed_graph = fmt_bool(c.prune_failed_graph),
            allow_unversioned_packages = fmt_bool(c.allow_unversioned_packages),
            error_on_missing_variant_requires = fmt_bool(c.error_on_missing_variant_requires),
            clean_shell_environment = fmt_bool(c.clean_shell_environment),
            append_sys_path = fmt_bool(c.append_sys_path),
            default_shell = default_shell,
            pathed_env_vars = pathed_env_vars,
            package_commands_sourced_first = package_commands_sourced_first,
            std_system_paths = std_system_paths,
            build_dir = build_dir,
            rez_1_environment_variables = fmt_bool(c.rez_1_environment_variables),
            disable_rez_1_compatibility = fmt_bool(c.disable_rez_1_compatibility),
            error_old_commands = fmt_bool(c.error_old_commands),
            warn_old_commands = fmt_bool(c.warn_old_commands),
            debug_old_commands = fmt_bool(c.debug_old_commands),
        )
    }

    /// Expand ~ in path to user home directory.
    /// Returns RezPath (Unix format) for consistent internal representation.
    pub fn expand_path(path: &str) -> crate::rez_path::RezPath {
        if let Some(rest) = path.strip_prefix("~/") {
            if let Some(home) = crate::platform::SystemInfo::home() {
                let mut result = PathBuf::from(&home);
                for component in rest.split('/').filter(|s| !s.is_empty()) {
                    result.push(component);
                }
                return crate::rez_path::RezPath::from_os(&result);
            }
        }
        crate::rez_path::RezPath::new(path)
    }

    /// Get expanded packages_path (Unix format).
    pub fn expanded_packages_path(&self) -> Vec<crate::rez_path::RezPath> {
        self.packages_path
            .iter()
            .map(|p| Self::expand_path(p))
            .collect()
    }

    /// Get expanded packages_path as OS-native PathBufs. Use at fs/Command boundary (FilesystemRepository::new, Provider::from_paths).
    /// When release_bind_path, release_pip_path, or release_build_path are set, they are included for scanning.
    pub fn expanded_packages_path_os(&self) -> Vec<PathBuf> {
        let release_base = self.expanded_release_packages_path().to_os();
        let mut extra: Vec<PathBuf> = Vec::new();
        for opt in [
            &self.release_bind_path,
            &self.release_pip_path,
            &self.release_build_path,
        ] {
            for p in opt.iter() {
                let expanded = Self::expand_path(p).to_os();
                if expanded != release_base && !extra.iter().any(|e| e == &expanded) {
                    extra.push(expanded);
                }
            }
        }
        let base = self
            .expanded_packages_path()
            .into_iter()
            .map(|path| path.to_os());
        let mut result = Vec::new();
        for path in base {
            if path == release_base {
                for e in &extra {
                    result.push(e.clone());
                }
            }
            result.push(path);
        }
        crate::install::category_paths(result, self.rez_install_categories)
    }

    /// Get package paths used for visible-package operations, excluding one exact
    /// local repository entry as Rez's `nonlocal_packages_path` does.
    ///
    /// This intentionally uses only `packages_path`; release-only scan paths
    /// added by `expanded_packages_path_os` are not part of the Rez setting.
    pub fn expanded_nonlocal_packages_path_os(&self) -> Vec<PathBuf> {
        let mut removed_local = false;

        crate::install::category_paths(
            self.packages_path.iter().filter_map(|path| {
                if !removed_local && path == &self.local_packages_path {
                    removed_local = true;
                    None
                } else {
                    Some(Self::expand_path(path).to_os())
                }
            }),
            self.rez_install_categories,
        )
    }

    /// Get package definition filename stems in repository search order.
    /// Matches Rez's `plugins.package_repository.filesystem.package_filenames` setting.
    pub fn package_definition_stems(&self) -> crate::errors::Result<Vec<String>> {
        let Some(package_repository) = self.plugins.get("package_repository") else {
            return Ok(vec!["package".to_owned()]);
        };
        let package_repository = package_repository.as_object().ok_or_else(|| {
            crate::errors::RezError::Config(
                "plugins.package_repository must be an object".to_owned(),
            )
        })?;
        let Some(filesystem) = package_repository.get("filesystem") else {
            return Ok(vec!["package".to_owned()]);
        };
        let filesystem = filesystem.as_object().ok_or_else(|| {
            crate::errors::RezError::Config(
                "plugins.package_repository.filesystem must be an object".to_owned(),
            )
        })?;
        let Some(names) = filesystem.get("package_filenames") else {
            return Ok(vec!["package".to_owned()]);
        };
        let names = names.as_array().ok_or_else(|| {
            crate::errors::RezError::Config(
                "plugins.package_repository.filesystem.package_filenames must be a list of strings"
                    .to_owned(),
            )
        })?;

        names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                name.as_str().map(str::to_owned).ok_or_else(|| {
                    crate::errors::RezError::Config(format!(
                        "plugins.package_repository.filesystem.package_filenames[{index}] must be a string"
                    ))
                })
            })
            .collect()
    }

    /// Select and route the repository used by build installation and release.
    pub fn build_install_path(
        &self,
        package: &crate::package::Package,
        explicit_path: Option<&Path>,
        release: bool,
    ) -> PathBuf {
        let base = explicit_path.map(Path::to_path_buf).unwrap_or_else(|| {
            if release || self.rez_install_location {
                self.expanded_release_path_for_build().to_os()
            } else if let Some(path) = &self.repo_path {
                Self::expand_path(path).to_os()
            } else {
                self.expanded_local_packages_path().to_os()
            }
        });
        crate::install::route_install_path(&base, package, self.rez_install_categories)
    }

    /// Get expanded local_packages_path (Unix format).
    pub fn expanded_local_packages_path(&self) -> crate::rez_path::RezPath {
        Self::expand_path(&self.local_packages_path)
    }

    /// Get expanded release_packages_path (Unix format).
    pub fn expanded_release_packages_path(&self) -> crate::rez_path::RezPath {
        Self::expand_path(&self.release_packages_path)
    }

    /// Release path for bind: release_bind_path, then repo_path, then release_packages_path.
    pub fn expanded_release_path_for_bind(&self) -> crate::rez_path::RezPath {
        self.release_bind_path
            .as_ref()
            .or(self.repo_path.as_ref())
            .map(|p| Self::expand_path(p))
            .unwrap_or_else(|| self.expanded_release_packages_path())
    }

    /// Release path for pip: release_pip_path, then repo_path, then release_packages_path.
    pub fn expanded_release_path_for_pip(&self) -> crate::rez_path::RezPath {
        self.release_pip_path
            .as_ref()
            .or(self.repo_path.as_ref())
            .map(|p| Self::expand_path(p))
            .unwrap_or_else(|| self.expanded_release_packages_path())
    }

    /// Release path for build/release: release_build_path, then repo_path, then release_packages_path.
    pub fn expanded_release_path_for_build(&self) -> crate::rez_path::RezPath {
        self.release_build_path
            .as_ref()
            .or(self.repo_path.as_ref())
            .map(|p| Self::expand_path(p))
            .unwrap_or_else(|| self.expanded_release_packages_path())
    }

    /// Look up the first matching tag in tag_paths and return the mapped subdirectory.
    pub fn resolve_tag_path(&self, tags: &[String]) -> Option<&str> {
        tags.iter()
            .find_map(|tag| self.tag_paths.get(tag).map(|s| s.as_str()))
    }

    /// Resolve implicit_packages with actual system values
    pub fn resolved_implicit_packages(&self) -> Vec<String> {
        let sys = self
            .system_info
            .as_ref()
            .unwrap_or(&crate::platform::RAW_SYSTEM);
        self.implicit_packages
            .iter()
            .map(|s| {
                s.replace("{system.platform}", sys.platform.name())
                    .replace("{system.arch}", &sys.arch.to_string())
                    .replace("{system.os}", &sys.os.to_string())
            })
            .collect()
    }

    /// Recursively merge plugin config objects while replacing non-object values.
    /// Rez uses this behavior for nested config dictionaries; arrays such as
    /// `package_filenames` are replaced as a whole and retain their declared order.
    fn merge_plugin_config(
        current: serde_json::Value,
        overrides: serde_json::Value,
    ) -> serde_json::Value {
        if let (Some(current), Some(overrides)) = (current.as_object(), overrides.as_object()) {
            let mut merged = current.clone();
            for (key, value) in overrides {
                if let Some(existing) = merged.get(key).cloned() {
                    merged.insert(
                        key.clone(),
                        Self::merge_plugin_config(existing, value.clone()),
                    );
                } else {
                    merged.insert(key.clone(), value.clone());
                }
            }
            serde_json::Value::Object(merged)
        } else {
            overrides
        }
    }

    /// Apply one source's settings independently so a malformed typed value
    /// cannot discard unrelated valid settings from the same source.
    #[doc(hidden)]
    pub fn merge_config_values(
        &mut self,
        source: &str,
        values: HashMap<String, serde_json::Value>,
    ) -> crate::errors::Result<()> {
        let mut current = serde_json::to_value(&*self).map_err(|error| {
            crate::errors::RezError::Config(format!(
                "{source}: failed to serialize current configuration: {error}"
            ))
        })?;
        let mut errors = Vec::new();

        for (key, value) in values {
            let mut candidate = current.clone();
            let object = candidate.as_object_mut().ok_or_else(|| {
                crate::errors::RezError::Config(format!(
                    "{source}: serialized configuration is not an object"
                ))
            })?;
            if key == "plugins" {
                let merged = object
                    .get(&key)
                    .cloned()
                    .map(|current| Self::merge_plugin_config(current, value.clone()))
                    .unwrap_or(value);
                object.insert(key.clone(), merged);
            } else {
                object.insert(key.clone(), value);
            }

            match serde_json::from_value::<Self>(candidate.clone()) {
                Ok(config) => {
                    *self = config;
                    current = candidate;
                }
                Err(error) => errors.push(format!(
                    "{source}: invalid configuration key '{key}': {error}"
                )),
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(crate::errors::RezError::Config(errors.join("\n")))
        }
    }

    /// Load config from .py / .toml files in standard locations.
    /// At each directory: rezconfig.py is checked first (matching Python rez),
    /// then rezconfig.toml as fallback.
    fn load_config_files(&mut self) -> crate::errors::Result<()> {
        log_info!("config", "Loading config files");
        let mut dirs = Vec::new();

        // System config dir
        #[cfg(unix)]
        {
            dirs.push(PathBuf::from("/etc/rez"));
            log_debug!("config", "Search dir: /etc/rez");
        }
        #[cfg(windows)]
        if let Ok(pd) = env::var("PROGRAMDATA") {
            let d = PathBuf::from(pd).join("rez");
            dirs.push(d.clone());
            log_debug!("config", "Search dir: {}", d.display());
        }

        // User config dir (.rez directory for compatibility)
        if let Some(home) = crate::platform::SystemInfo::home() {
            let d = PathBuf::from(home).join(".rez");
            dirs.push(d.clone());
            log_debug!("config", "Search dir: {}", d.display());
        }

        let mut errors = Vec::new();
        for dir in &dirs {
            if let Err(error) = self.merge_config_dir(dir) {
                errors.push(error.to_string());
            }
        }

        // Explicit override via REZ_CONFIG_FILE (supports multiple paths separated by pathsep)
        if let Ok(config_file) = env::var("REZ_CONFIG_FILE") {
            #[cfg(windows)]
            const PATH_SEP: char = ';';
            #[cfg(not(windows))]
            const PATH_SEP: char = ':';

            log_info!("config", "REZ_CONFIG_FILE override: {}", config_file);
            for file_path in config_file.split(PATH_SEP) {
                let path = PathBuf::from(file_path.trim());
                if path.exists() {
                    log_debug!("config", "Loading from REZ_CONFIG_FILE: {}", path.display());
                    let result = if path.extension().and_then(|e| e.to_str()) == Some("py") {
                        self.merge_py_file(&path)
                    } else {
                        self.merge_toml_file(&path)
                    };
                    if let Err(error) = result {
                        errors.push(error.to_string());
                    }
                }
            }
        }

        // Home config files (Python rez compatibility: ~/.rezconfig.py, ~/.rezconfig)
        // Skip if REZ_DISABLE_HOME_CONFIG is set
        if !Self::home_config_disabled() {
            if let Some(home) = crate::platform::SystemInfo::home() {
                let home_path = PathBuf::from(home);

                // Try ~/.rezconfig.py first
                let py_config = home_path.join(".rezconfig.py");
                let result = if py_config.exists() {
                    log_debug!("config", "Loading home config: {}", py_config.display());
                    Some(self.merge_py_file(&py_config))
                } else {
                    // Try ~/.rezconfig.toml
                    let toml_config = home_path.join(".rezconfig.toml");
                    if toml_config.exists() {
                        log_debug!("config", "Loading home config: {}", toml_config.display());
                        Some(self.merge_toml_file(&toml_config))
                    } else {
                        // Try ~/.rezconfig as YAML fallback
                        let yaml_config = home_path.join(".rezconfig");
                        if yaml_config.exists() {
                            log_debug!("config", "Loading home config: {}", yaml_config.display());
                            Some(self.merge_yaml_file(&yaml_config))
                        } else {
                            None
                        }
                    }
                };
                if let Some(Err(error)) = result {
                    errors.push(error.to_string());
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(crate::errors::RezError::Config(errors.join("\n")))
        }
    }

    /// Merge config from a directory: try rezconfig.py first, then rezconfig.toml.
    fn merge_config_dir(&mut self, dir: &Path) -> crate::errors::Result<()> {
        let py_path = dir.join("rezconfig.py");
        if py_path.exists() {
            log_trace!("config", "Merging rezconfig.py from {}", dir.display());
            return self.merge_py_file(&py_path);
        }
        let toml_path = dir.join("rezconfig.toml");
        if toml_path.exists() {
            log_trace!("config", "Merging rezconfig.toml from {}", dir.display());
            return self.merge_toml_file(&toml_path);
        }
        Ok(())
    }

    /// Merge a Python config file into current config via embedded Python VM.
    fn merge_py_file(&mut self, path: &Path) -> crate::errors::Result<()> {
        let table = load_rezconfig_py(path).map_err(|error| {
            crate::errors::RezError::Config(format!("{}: {error}", path.display()))
        })?;
        self.merge_config_values(&path.display().to_string(), table)
    }

    /// Merge a TOML config file into current config.
    fn merge_toml_file(&mut self, path: &Path) -> crate::errors::Result<()> {
        let content = std::fs::read_to_string(path).map_err(|error| {
            crate::errors::RezError::Config(format!(
                "{}: failed to read configuration: {error}",
                path.display()
            ))
        })?;
        let table: HashMap<String, serde_json::Value> =
            toml::from_str(&content).map_err(|error| {
                crate::errors::RezError::Config(format!(
                    "{}: failed to parse TOML configuration: {error}",
                    path.display()
                ))
            })?;
        self.merge_config_values(&path.display().to_string(), table)
    }

    /// Apply REZ_* environment variable overrides.
    fn apply_env_overrides(
        &mut self,
        overrides: Option<&HashMap<String, String>>,
    ) -> crate::errors::Result<()> {
        let environment = overrides.cloned().unwrap_or_else(|| env::vars().collect());
        let current = serde_json::to_value(&*self).map_err(|error| {
            crate::errors::RezError::Config(format!(
                "environment overrides: failed to serialize current configuration: {error}"
            ))
        })?;
        let Some(map) = current.as_object() else {
            return Err(crate::errors::RezError::Config(
                "environment overrides: serialized configuration is not an object".into(),
            ));
        };

        let keys: Vec<String> = map.keys().cloned().collect();
        let mut errors = Vec::new();
        let mut environment_names = HashMap::<String, String>::new();
        let mut colliding_keys = HashSet::new();
        for key in &keys {
            let environment_name = config_environment_name(key);
            if let Some(previous_key) =
                environment_names.insert(environment_name.clone(), key.clone())
            {
                colliding_keys.insert(previous_key.clone());
                colliding_keys.insert(key.clone());
                errors.push(format!(
                    "configuration keys '{previous_key}' and '{key}' map to the same environment variable '{environment_name}'"
                ));
            }
        }
        for key in keys {
            if colliding_keys.contains(&key) {
                continue;
            }
            let env_key = config_environment_name(&key);

            // Rez gives the plain environment variable precedence over its JSON form.
            let override_value = if let Some(value) = environment.get(&env_key) {
                Some(
                    convert_env_value(value, &map[&key], Some(&key)).map_err(|error| {
                        crate::errors::RezError::Config(format!("{env_key}: {error}"))
                    }),
                )
            } else {
                let json_env_key = format!("{env_key}_JSON");
                environment.get(&json_env_key).map(|value| {
                    serde_json::from_str::<serde_json::Value>(value).map_err(|error| {
                        crate::errors::RezError::Config(format!(
                            "{json_env_key}: expected a JSON value: {error}"
                        ))
                    })
                })
            };

            match override_value {
                Some(Ok(value)) => {
                    if let Err(error) =
                        self.merge_config_values(&env_key, HashMap::from([(key, value)]))
                    {
                        errors.push(error.to_string());
                    }
                }
                Some(Err(error)) => errors.push(error.to_string()),
                None => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(crate::errors::RezError::Config(errors.join("\n")))
        }
    }

    /// Check if a specific warning is enabled
    pub fn is_warn_enabled(&self, name: &str) -> bool {
        if self.warn_none {
            return false;
        }
        if self.warn_all {
            return true;
        }
        match name {
            "old_commands" => self.warn_old_commands,
            "shell_startup" => self.warn_shell_startup,
            "untimestamped" => self.warn_untimestamped,
            _ => false,
        }
    }

    /// Check if a specific debug flag is enabled
    pub fn is_debug_enabled(&self, name: &str) -> bool {
        if self.debug_none {
            return false;
        }
        if self.debug_all {
            return true;
        }
        match name {
            "old_commands" => self.debug_old_commands,
            "file_loads" => self.debug_file_loads,
            "plugins" => self.debug_plugins,
            "package_release" => self.debug_package_release,
            "bind_modules" => self.debug_bind_modules,
            "resources" => self.debug_resources,
            "package_exclusions" => self.debug_package_exclusions,
            "resolve_memcache" => self.debug_resolve_memcache,
            "memcache" => self.debug_memcache,
            "context_tracking" => self.debug_context_tracking,
            _ => false,
        }
    }

    /// Check if home config loading is disabled via REZ_DISABLE_HOME_CONFIG
    fn home_config_disabled() -> bool {
        env::var("REZ_DISABLE_HOME_CONFIG")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "t" | "true" | "yes"))
            .unwrap_or(false)
    }

    /// Expand {system.*} and ${ENV} variables in all config values
    fn expand_all_values(&mut self) -> crate::errors::Result<()> {
        let system = crate::platform::RAW_SYSTEM.mapped(&self.platform_map)?;
        // Serialize to JSON, expand all values, deserialize back
        let json = match serde_json::to_value(&*self) {
            Ok(v) => v,
            Err(error) => {
                return Err(crate::errors::RezError::Config(format!(
                    "failed to serialize config for expansion: {error}"
                )));
            }
        };

        // Regex patterns/replacements are opaque configuration, not path templates.
        // Expanding $ or {system.*} inside platform_map would alter Python regex semantics.
        let mut json = json;
        let platform_map = json
            .as_object_mut()
            .and_then(|object| object.remove("platform_map"));
        let mut expanded = Self::expand_config_value(json, &system);
        if let Some(platform_map) = platform_map {
            expanded
                .as_object_mut()
                .expect("serialized configuration is an object")
                .insert("platform_map".into(), platform_map);
        }
        let mut config: Self = serde_json::from_value(expanded).map_err(|error| {
            crate::errors::RezError::Config(format!(
                "failed to deserialize expanded config: {error}"
            ))
        })?;
        config.system_info = Some(system);
        *self = config;
        Ok(())
    }

    /// Merge a YAML config file (for ~/.rezconfig fallback)
    fn merge_yaml_file(&mut self, path: &Path) -> crate::errors::Result<()> {
        let content = std::fs::read_to_string(path).map_err(|error| {
            crate::errors::RezError::Config(format!(
                "{}: failed to read configuration: {error}",
                path.display()
            ))
        })?;
        let table: HashMap<String, serde_json::Value> =
            serde_yaml::from_str(&content).map_err(|error| {
                crate::errors::RezError::Config(format!(
                    "{}: failed to parse YAML configuration: {error}",
                    path.display()
                ))
            })?;
        self.merge_config_values(&path.display().to_string(), table)
    }

    /// Expand config value: {system.*} variables and ${ENV} environment variables
    fn expand_config_value(
        value: serde_json::Value,
        sys: &crate::platform::SystemInfo,
    ) -> serde_json::Value {
        use serde_json::Value;

        match value {
            Value::String(s) => {
                let mut expanded = s
                    .replace("{system.platform}", sys.platform.name())
                    .replace("{system.arch}", &sys.arch.to_string())
                    .replace("{system.os}", &sys.os.to_string())
                    .replace("{system.user}", &crate::platform::SystemInfo::user());

                // Expand ${VAR} and $VAR environment variables
                expanded = Self::expand_env_vars(&expanded);

                Value::String(expanded)
            }
            Value::Array(arr) => Value::Array(
                arr.into_iter()
                    .map(|value| Self::expand_config_value(value, sys))
                    .collect(),
            ),
            Value::Object(obj) => Value::Object(
                obj.into_iter()
                    .map(|(k, v)| (k, Self::expand_config_value(v, sys)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Expand environment variables in string: ${VAR} and $VAR
    fn expand_env_vars(text: &str) -> String {
        let mut result = String::new();
        let mut chars = text.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '$' {
                if chars.peek() == Some(&'{') {
                    // ${VAR} syntax
                    chars.next(); // consume '{'
                    let mut var_name = String::new();
                    let mut found_close = false;

                    while let Some(&c) = chars.peek() {
                        if c == '}' {
                            chars.next();
                            found_close = true;
                            break;
                        }
                        if let Some(c) = chars.next() {
                            var_name.push(c);
                        }
                    }

                    if found_close {
                        if let Ok(val) = env::var(&var_name) {
                            result.push_str(&val);
                        } else {
                            // Keep original if not found
                            result.push_str(&format!("${{{}}}", var_name));
                        }
                    } else {
                        result.push_str(&format!("${{{}", var_name));
                    }
                } else if chars
                    .peek()
                    .map(|c| c.is_alphanumeric() || *c == '_')
                    .unwrap_or(false)
                {
                    // $VAR syntax
                    let mut var_name = String::new();

                    while let Some(&c) = chars.peek() {
                        if c.is_alphanumeric() || c == '_' {
                            if let Some(c) = chars.next() {
                                var_name.push(c);
                            }
                        } else {
                            break;
                        }
                    }

                    if let Ok(val) = env::var(&var_name) {
                        result.push_str(&val);
                    } else {
                        result.push_str(&format!("${}", var_name));
                    }
                } else {
                    result.push(ch);
                }
            } else {
                result.push(ch);
            }
        }

        result
    }
}

// ---------------------------------------------------------------------------
// rezconfig.py loading via embedded Python VM
// ---------------------------------------------------------------------------

/// Load configuration from a rezconfig.py file by executing it via embedded Python VM.
pub fn load_rezconfig_py(
    path: &std::path::Path,
) -> Result<HashMap<String, serde_json::Value>, crate::errors::RezError> {
    let filename = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut inject = HashMap::new();
    inject.insert("__name__".into(), serde_json::Value::String(filename));
    inject.insert(
        "__file__".into(),
        serde_json::Value::String(path.display().to_string()),
    );
    inject.insert(
        "rez_version".into(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").into()),
    );

    // Preamble with rez config helpers (ModifyList, DelayLoad)
    let preamble = "class ModifyList(list): pass\nclass DelayLoad:\n    def __init__(self, func): self.func = func\n";
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::errors::RezError::Python(format!("read {}: {}", path.display(), e)))?;
    let full = format!("{}\n{}", preamble, content);

    python_runtime::exec_py_globals(
        &full,
        &path.file_name().unwrap_or_default().to_string_lossy(),
        Some(&inject),
        &["ModifyList", "DelayLoad"],
    )
}

// ---------------------------------------------------------------------------
// Helper: environment variable names and typed values
// ---------------------------------------------------------------------------

fn config_environment_name(key: &str) -> String {
    match key {
        "rez_install_categories" => "REZ_INSTALL_CATEGORIES".into(),
        "rez_install_location" => "REZ_INSTALL_LOCATION".into(),
        _ => format!("REZ_{}", key.to_uppercase()),
    }
}

fn deserialize_log_level<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|value| {
            let value = value.trim().to_ascii_uppercase();
            if matches!(
                value.as_str(),
                "ERROR" | "WARNING" | "INFO" | "DEBUG" | "TRACE" | "OFF"
            ) {
                Ok(value)
            } else {
                Err(serde::de::Error::custom(
                    "log_level must be ERROR, WARNING, INFO, DEBUG, TRACE, or OFF",
                ))
            }
        })
        .transpose()
}

fn convert_env_value(
    val: &str,
    current: &serde_json::Value,
    key: Option<&str>,
) -> std::result::Result<serde_json::Value, String> {
    if key == Some("offline") {
        return match val.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(serde_json::Value::Bool(true)),
            "false" => Ok(serde_json::Value::Bool(false)),
            _ => Err("expected true or false".into()),
        };
    }
    if key == Some("build_thread_count") {
        return Ok(val
            .trim()
            .parse::<i64>()
            .map(serde_json::Value::from)
            .unwrap_or_else(|_| serde_json::Value::String(val.to_owned())));
    }
    match current {
        // OptionalBool accepts the same environment spellings even when a
        // Python config selected None. Plain environment overrides cannot select
        // None; configuration files and JSON overrides can.
        serde_json::Value::Null if key == Some("default_cachable") => {
            convert_env_value(val, &serde_json::Value::Bool(false), None)
        }
        serde_json::Value::Bool(_) => match val.to_lowercase().as_str() {
            "1" | "true" | "t" | "yes" | "y" | "on" => Ok(serde_json::Value::Bool(true)),
            "0" | "false" | "f" | "no" | "n" | "off" => Ok(serde_json::Value::Bool(false)),
            _ => Err("expected a Rez boolean value (1/0, true/false, yes/no, on/off)".into()),
        },
        serde_json::Value::Number(number) if number.is_i64() => val
            .parse::<i64>()
            .map(serde_json::Value::from)
            .map_err(|_| "expected an integer value".into()),
        serde_json::Value::Number(number) if number.is_u64() => val
            .parse::<u64>()
            .map(serde_json::Value::from)
            .map_err(|_| "expected an unsigned integer value".into()),
        serde_json::Value::Number(_) => {
            let value = val
                .parse::<f64>()
                .map_err(|_| "expected a floating-point value".to_owned())?;
            serde_json::Number::from_f64(value)
                .map(serde_json::Value::Number)
                .ok_or_else(|| "expected a finite floating-point value".into())
        }
        serde_json::Value::Array(_) => {
            // Preserve comma-separated overrides and support the OS path-list separator.
            let items: Vec<serde_json::Value> = val
                .split(',')
                .flat_map(std::env::split_paths)
                .map(|path| serde_json::Value::String(path.to_string_lossy().trim().to_string()))
                .filter(|value| value.as_str().is_some_and(|path| !path.is_empty()))
                .collect();
            Ok(serde_json::Value::Array(items))
        }
        _ => Ok(serde_json::Value::String(val.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_environment_overrides_are_typed_and_keep_lookup_paths() {
        let mut config = RezConfig::default();
        let lookup = config.packages_path.clone();
        config
            .apply_env_overrides(Some(&HashMap::from([
                ("REZ_SOURCES_PATH".into(), "/archive mirror".into()),
                ("REZ_WHEEL_CACHE_PATH".into(), "/wheels".into()),
                ("REZ_USER_PATH".into(), "/user".into()),
                ("REZ_REPO_PATH".into(), "/publish".into()),
                ("REZ_OFFLINE".into(), "true".into()),
                ("REZ_LOG_LEVEL".into(), "debug".into()),
            ])))
            .unwrap();
        assert_eq!(
            config.recipe_environment()["REZ_SOURCES_PATH"],
            "/archive mirror"
        );
        assert_eq!(config.recipe_environment()["REZ_OFFLINE"], "true");
        assert_eq!(config.recipe_environment()["REZ_LOG_LEVEL"], "DEBUG");
        assert_eq!(config.packages_path, lookup);
        for value in ["1", "yes", "not-a-bool"] {
            assert!(config
                .apply_env_overrides(Some(&HashMap::from(
                    [("REZ_OFFLINE".into(), value.into()),]
                )))
                .unwrap_err()
                .to_string()
                .contains("expected true or false"));
        }
        assert!(config
            .apply_env_overrides(Some(&HashMap::from([(
                "REZ_LOG_LEVEL".into(),
                "verbose".into()
            ),])))
            .is_err());
        config
            .apply_env_overrides(Some(&HashMap::from([(
                "REZ_OFFLINE".into(),
                "false".into(),
            )])))
            .unwrap();
        assert_eq!(config.recipe_environment()["REZ_OFFLINE"], "false");
    }

    #[test]
    fn offline_pip_preserves_find_links_and_clears_indexes() {
        let config = RezConfig {
            sources_path: Some("/mirror".into()),
            wheel_cache_path: Some("/cache".into()),
            offline: true,
            ..RezConfig::default()
        };
        let parent = HashMap::from([
            ("PIP_FIND_LINKS".into(), "/explicit".into()),
            (
                "PIP_INDEX_URL".into(),
                "https://example.invalid/simple".into(),
            ),
            (
                "PIP_EXTRA_INDEX_URL".into(),
                "https://example.invalid/extra".into(),
            ),
        ]);
        let environment = config.pip_environment(&parent);
        assert_eq!(environment["PIP_FIND_LINKS"], "/explicit");
        assert_eq!(environment["PIP_NO_INDEX"], "true");
        assert!(!environment.contains_key("PIP_INDEX_URL"));
        assert!(!environment.contains_key("PIP_EXTRA_INDEX_URL"));
        assert_eq!(
            config.pip_environment(&HashMap::new())["PIP_FIND_LINKS"].replace('\\', "/"),
            "/cache"
        );
        let config = RezConfig {
            wheel_cache_path: None,
            ..config
        };
        assert_eq!(
            config.pip_environment(&HashMap::new())["PIP_FIND_LINKS"].replace('\\', "/"),
            "/mirror/wheels"
        );
    }

    #[test]
    fn common_publication_root_respects_specific_targets() {
        let package = crate::package::Package::new("sample", version::Version::new("1.0").unwrap());
        let mut config = RezConfig {
            repo_path: Some("/common".into()),
            rez_install_categories: false,
            release_bind_path: None,
            release_pip_path: None,
            ..RezConfig::default()
        };
        assert_eq!(
            config.build_install_path(&package, None, false),
            PathBuf::from("/common")
        );
        config.rez_install_location = false;
        assert_eq!(
            config.build_install_path(&package, None, false),
            PathBuf::from("/common")
        );
        assert_eq!(
            config.build_install_path(&package, Some(Path::new("/explicit")), true),
            PathBuf::from("/explicit")
        );
        assert_eq!(
            config.expanded_release_path_for_pip().to_os(),
            PathBuf::from("/common")
        );
        config.release_build_path = Some("/specific".into());
        assert_eq!(
            config.build_install_path(&package, None, true),
            PathBuf::from("/specific")
        );
        config.release_pip_path = Some("/pip".into());
        assert_eq!(
            config.expanded_release_path_for_pip().to_os(),
            PathBuf::from("/pip")
        );
    }

    #[test]
    fn test_default_config() {
        let config = RezConfig::default();
        assert_eq!(config.packages_path.len(), 3);
        assert_eq!(config.local_packages_path, "~/.rez/packages/local/int");
        assert!(config.resolve_caching);
        assert!(config.default_relocatable);
        assert_eq!(config.build_directory, "build");
        assert_eq!(config.suite_alias_prefix_char, '+');
    }

    #[test]
    fn test_cache_policy_nullable_config() {
        let fields = [
            "default_relocatable_per_package",
            "default_relocatable_per_repository",
            "default_cachable_per_package",
            "default_cachable_per_repository",
        ];
        for default in [
            serde_json::Value::Null,
            serde_json::json!(false),
            serde_json::json!(true),
        ] {
            let mut values = serde_json::Map::new();
            values.insert("default_cachable".into(), default.clone());
            for field in fields {
                values.insert(
                    field.into(),
                    serde_json::json!({"skip": null, "off": false, "on": true}),
                );
            }
            let mut config = RezConfig::default();
            config
                .merge_config_values("nullable JSON policy", values.clone().into_iter().collect())
                .unwrap();
            let encoded = serde_json::to_value(&config).unwrap();
            for (field, expected) in values {
                assert_eq!(encoded[&field], expected);
            }

            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("config.py");
            let default = match default {
                serde_json::Value::Null => "None",
                serde_json::Value::Bool(false) => "False",
                serde_json::Value::Bool(true) => "True",
                _ => unreachable!(),
            };
            let mut source = format!("default_cachable = {default}\n");
            for field in fields {
                source.push_str(&format!(
                    "{field} = {{'skip': None, 'off': False, 'on': True}}\n"
                ));
            }
            std::fs::write(&path, source).unwrap();
            let mut python = RezConfig::default();
            python.merge_py_file(&path).unwrap();
            assert_eq!(serde_json::to_value(python).unwrap(), encoded);
        }

        let defaults = RezConfig::default();
        assert_eq!(defaults.default_cachable, Some(false));
        assert!(RezConfig::default_config_py().contains("default_cachable = False"));
        // Keep typed bool/null validation: unrelated objects are not truthiness-cast.
        for field in fields.into_iter().chain(["default_cachable"]) {
            let mut config = RezConfig::default();
            assert!(config
                .merge_config_values(
                    "invalid policy",
                    [(field.into(), serde_json::json!("invalid"))]
                        .into_iter()
                        .collect(),
                )
                .is_err());
        }
    }

    #[test]
    fn test_optional_cache_bool_environment() {
        for current in [
            serde_json::Value::Null,
            serde_json::json!(false),
            serde_json::json!(true),
        ] {
            for (value, expected) in [("true", true), ("off", false), ("1", true), ("0", false)] {
                assert_eq!(
                    convert_env_value(value, &current, Some("default_cachable")).unwrap(),
                    serde_json::json!(expected),
                );
            }
            for value in ["None", "maybe"] {
                assert!(convert_env_value(value, &current, Some("default_cachable")).is_err());
            }
        }
    }

    #[test]
    fn test_optional_cache_bool_environment_overrides() {
        for initial in [None, Some(false), Some(true)] {
            for (plain, json, expected) in [
                (Some("true"), None, Some(true)),
                (Some("off"), None, Some(false)),
                (None, Some("null"), None),
                (None, Some("false"), Some(false)),
                (None, Some("true"), Some(true)),
                (Some("true"), Some("null"), Some(true)),
                (Some("off"), Some("true"), Some(false)),
                (Some("true"), Some("invalid JSON"), Some(true)),
            ] {
                let mut config = RezConfig {
                    default_cachable: initial,
                    ..RezConfig::default()
                };
                let mut overrides = HashMap::new();
                if let Some(value) = plain {
                    overrides.insert("REZ_DEFAULT_CACHABLE".into(), value.into());
                }
                if let Some(value) = json {
                    overrides.insert("REZ_DEFAULT_CACHABLE_JSON".into(), value.into());
                }
                config.apply_env_overrides(Some(&overrides)).unwrap();
                assert_eq!(config.default_cachable, expected);
            }
            for (plain, json) in [
                (Some("None"), None),
                (Some("invalid"), Some("true")),
                (None, Some("invalid JSON")),
                (None, Some("\"invalid\"")),
            ] {
                let mut config = RezConfig {
                    default_cachable: initial,
                    ..RezConfig::default()
                };
                let mut overrides = HashMap::new();
                if let Some(value) = plain {
                    overrides.insert("REZ_DEFAULT_CACHABLE".into(), value.into());
                }
                if let Some(value) = json {
                    overrides.insert("REZ_DEFAULT_CACHABLE_JSON".into(), value.into());
                }
                assert!(config.apply_env_overrides(Some(&overrides)).is_err());
                assert_eq!(config.default_cachable, initial);
            }
        }
    }

    #[test]
    fn test_expand_path() {
        let path = RezConfig::expand_path("~/packages");
        // Should expand ~ to home directory (RezPath stores Unix format)
        let path_str = path.as_str();
        assert!(!path_str.starts_with("~/") || crate::platform::SystemInfo::home().is_none());
    }

    #[test]
    fn test_enum_configuration_uses_reference_names() {
        let cases: &[(&str, &[&str])] = &[
            ("rez_tools_visibility", &["never", "append", "prepend"]),
            (
                "suite_visibility",
                &["never", "always", "parent", "parent_priority"],
            ),
            (
                "variant_select_mode",
                &["version_priority", "intersection_priority"],
            ),
            ("package_preprocess_mode", &["before", "after", "override"]),
            (
                "create_executable_script_mode",
                &["single", "py", "platform_specific", "both"],
            ),
        ];
        for (key, names) in cases {
            for name in *names {
                let mut config = RezConfig::default();
                config
                    .merge_config_values(
                        "enum fixture",
                        HashMap::from([((*key).into(), serde_json::json!(name))]),
                    )
                    .unwrap();
                assert_eq!(serde_json::to_value(&config).unwrap()[*key], *name);
                let environment = HashMap::from([(config_environment_name(key), (*name).into())]);
                config.apply_env_overrides(Some(&environment)).unwrap();
                assert_eq!(serde_json::to_value(&config).unwrap()[*key], *name);
            }
        }
        for (key, name) in [
            ("rez_tools_visibility", "Prepend"),
            ("suite_visibility", "ParentPriority"),
        ] {
            let mut config = RezConfig::default();
            assert!(config
                .merge_config_values(
                    "case fixture",
                    HashMap::from([(key.into(), serde_json::json!(name))])
                )
                .is_err());
        }
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("rezconfig.py");
        std::fs::write(&file, RezConfig::default_config_py()).unwrap();
        let mut config = RezConfig::default();
        config.merge_py_file(&file).unwrap();
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            serde_json::to_value(RezConfig::default()).unwrap(),
            "generated Python configuration must preserve every canonical default"
        );
        assert_eq!(config.rez_tools_visibility, RezToolsVisibility::Append);
        assert_eq!(config.suite_visibility, SuiteVisibility::Always);
        assert_eq!(config.package_preprocess_mode, PreprocessMode::Override);
        assert!(!config.rez_1_environment_variables);
        assert!(config.disable_rez_1_compatibility);
        assert!(!config.error_old_commands);
        assert!(config.warn_old_commands);
        assert!(!config.debug_old_commands);
        let source = RezConfig::default_config_py();
        for line in [
            "rez_1_environment_variables = False",
            "disable_rez_1_compatibility = True",
            "error_old_commands = False",
            "warn_old_commands = True",
            "debug_old_commands = False",
        ] {
            assert!(source.contains(line), "{line}");
        }
    }

    #[test]
    fn test_build_thread_count_schema_and_environment() {
        for value in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("bogus"),
            serde_json::json!("3"),
        ] {
            assert!(
                serde_json::from_value::<BuildThreadCount>(value.clone()).is_err(),
                "{value}"
            );
            let mut config = RezConfig::default();
            assert!(config
                .merge_config_values(
                    "thread fixture",
                    HashMap::from([("build_thread_count".into(), value)])
                )
                .is_err());
        }
        for (input, expected) in [
            ("3", BuildThreadCount::Count(3)),
            (" \t3\n ", BuildThreadCount::Count(3)),
            (
                "physical_cores",
                BuildThreadCount::Auto("physical_cores".into()),
            ),
            (
                "logical_cores",
                BuildThreadCount::Auto("logical_cores".into()),
            ),
        ] {
            let mut config = RezConfig::default();
            config
                .apply_env_overrides(Some(&HashMap::from([(
                    "REZ_BUILD_THREAD_COUNT".into(),
                    input.into(),
                )])))
                .unwrap();
            assert_eq!(config.build_thread_count, expected);
            config
                .apply_env_overrides(Some(&HashMap::from([(
                    "REZ_BUILD_THREAD_COUNT".into(),
                    "2".into(),
                )])))
                .unwrap();
            assert_eq!(config.build_thread_count, BuildThreadCount::Count(2));
        }
        for input in ["0", "-1", "bogus", "1.5"] {
            let mut config = RezConfig::default();
            assert!(
                config
                    .apply_env_overrides(Some(&HashMap::from([(
                        "REZ_BUILD_THREAD_COUNT".into(),
                        input.into()
                    )])))
                    .is_err(),
                "{input}"
            );
        }
        let mut config = RezConfig::default();
        assert!(config
            .apply_env_overrides(Some(&HashMap::from([(
                "REZ_BUILD_THREAD_COUNT_JSON".into(),
                "\"3\"".into()
            )])))
            .is_err());
        config
            .apply_env_overrides(Some(&HashMap::from([
                ("REZ_BUILD_THREAD_COUNT".into(), "3".into()),
                ("REZ_BUILD_THREAD_COUNT_JSON".into(), "\"bogus\"".into()),
            ])))
            .unwrap();
        assert_eq!(config.build_thread_count, BuildThreadCount::Count(3));
        assert!(!config.rez_1_environment_variables);
        config
            .apply_env_overrides(Some(&HashMap::from([(
                "REZ_REZ_1_ENVIRONMENT_VARIABLES".into(),
                "true".into(),
            )])))
            .unwrap();
        assert!(config.rez_1_environment_variables);
    }

    #[test]
    fn test_build_thread_count_resolve() {
        let auto = BuildThreadCount::Auto("physical_cores".into());
        assert!(auto.resolve() >= 1);

        let fixed = BuildThreadCount::Count(4);
        assert_eq!(fixed.resolve(), 4);
    }

    #[test]
    fn clean_shell_configuration_preserves_the_source_key_and_defaults() {
        let mut config = RezConfig::default();
        assert!(!config.clean_shell_environment);
        assert!(config.append_sys_path);
        assert_eq!(
            config_environment_name("append_sys_path"),
            "REZ_APPEND_SYS_PATH"
        );
        assert_eq!(
            config_environment_name("clean_shell_environment"),
            "REZ_CLEAN_SHELL_ENVIRONMENT"
        );
        config
            .merge_config_values(
                "test",
                HashMap::from([
                    ("clean_shell_environment".into(), serde_json::json!(true)),
                    ("append_sys_path".into(), serde_json::json!(false)),
                ]),
            )
            .unwrap();
        assert!(config.clean_shell_environment);
        assert!(!config.append_sys_path);
        assert!(RezConfig::default_config_py().contains("clean_shell_environment = False"));
        assert!(RezConfig::default_config_py().contains("append_sys_path = True"));
    }

    #[test]
    fn test_convert_env_bool() {
        let current = serde_json::Value::Bool(false);
        assert_eq!(
            convert_env_value("true", &current, None).unwrap(),
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            convert_env_value("y", &current, None).unwrap(),
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            convert_env_value("off", &current, None).unwrap(),
            serde_json::Value::Bool(false)
        );
        assert!(convert_env_value("maybe", &current, None).is_err());
    }

    #[test]
    fn test_convert_env_number() {
        let current = serde_json::json!(42);
        assert_eq!(
            convert_env_value("100", &current, None).unwrap(),
            serde_json::json!(100)
        );
        assert!(convert_env_value("invalid", &current, None).is_err());
    }

    #[test]
    fn test_convert_env_list() {
        let current = serde_json::json!(["a"]);
        let result = convert_env_value("foo,bar,baz", &current, None).unwrap();
        assert_eq!(result, serde_json::json!(["foo", "bar", "baz"]));
    }

    #[test]
    fn test_convert_env_path_list_preserves_path_components() {
        let current = serde_json::json!(["a"]);
        let paths = if cfg!(windows) {
            vec![
                PathBuf::from(r"C:\rez\packages"),
                PathBuf::from(r"D:\shared\packages"),
            ]
        } else {
            vec![
                PathBuf::from("/opt/rez/packages"),
                PathBuf::from("/home/rez/packages"),
            ]
        };
        let value = env::join_paths(&paths)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let result = convert_env_value(&value, &current, None).unwrap();
        let expected: Vec<String> = paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();

        assert_eq!(result, serde_json::json!(expected));
    }

    #[test]
    fn test_config_file_typed_errors_keep_valid_siblings() {
        let dir =
            std::env::temp_dir().join(format!("rez_config_typed_errors_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let toml_path = dir.join("typed.toml");
        std::fs::write(
            &toml_path,
            "resolve_caching = \"invalid\"\nbuild_directory = \"from_toml\"\n",
        )
        .unwrap();
        let mut toml_config = RezConfig::default();
        let error = toml_config
            .merge_toml_file(&toml_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains(&toml_path.display().to_string()));
        assert!(error.contains("resolve_caching"));
        assert!(toml_config.resolve_caching);
        assert_eq!(toml_config.build_directory, "from_toml");

        let yaml_path = dir.join("typed.yaml");
        std::fs::write(
            &yaml_path,
            "resolve_caching: invalid\nbuild_directory: from_yaml\n",
        )
        .unwrap();
        let mut yaml_config = RezConfig::default();
        let error = yaml_config
            .merge_yaml_file(&yaml_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains(&yaml_path.display().to_string()));
        assert!(error.contains("resolve_caching"));
        assert!(yaml_config.resolve_caching);
        assert_eq!(yaml_config.build_directory, "from_yaml");

        let py_path = dir.join("typed.py");
        std::fs::write(
            &py_path,
            "resolve_caching = 'invalid'\nbuild_directory = 'from_python'\n",
        )
        .unwrap();
        let mut py_config = RezConfig::default();
        let error = py_config.merge_py_file(&py_path).unwrap_err().to_string();
        assert!(error.contains(&py_path.display().to_string()));
        assert!(error.contains("resolve_caching"));
        assert!(py_config.resolve_caching);
        assert_eq!(py_config.build_directory, "from_python");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_invalid_environment_overrides_report_keys_and_keep_valid_siblings() {
        let overrides = HashMap::from([
            ("REZ_RESOLVE_CACHING".into(), "perhaps".into()),
            ("REZ_RESOURCE_CACHING_MAXSIZE".into(), "not-a-number".into()),
            ("REZ_MEMCACHED_URI_JSON".into(), "{".into()),
            ("REZ_BUILD_DIRECTORY".into(), "still_applied".into()),
        ]);
        let mut config = RezConfig::default();

        let error = config
            .apply_env_overrides(Some(&overrides))
            .unwrap_err()
            .to_string();

        assert!(error.contains("REZ_RESOLVE_CACHING"));
        assert!(error.contains("REZ_RESOURCE_CACHING_MAXSIZE"));
        assert!(error.contains("REZ_MEMCACHED_URI_JSON"));
        assert!(config.resolve_caching);
        assert_eq!(config.resource_caching_maxsize, -1);
        assert!(config.memcached_uri.is_empty());
        assert_eq!(config.build_directory, "still_applied");
    }

    #[test]
    fn test_install_flags_use_confirmed_environment_names_and_rez_precedence() {
        let overrides = HashMap::from([
            ("REZ_INSTALL_CATEGORIES".into(), "no".into()),
            ("REZ_INSTALL_CATEGORIES_JSON".into(), "true".into()),
            ("REZ_INSTALL_LOCATION".into(), "0".into()),
        ]);
        let mut config = RezConfig::default();

        config.apply_env_overrides(Some(&overrides)).unwrap();

        assert!(!config.rez_install_categories);
        assert!(!config.rez_install_location);
        assert_eq!(
            config_environment_name("rez_install_categories"),
            "REZ_INSTALL_CATEGORIES"
        );
        assert_eq!(
            config_environment_name("rez_install_location"),
            "REZ_INSTALL_LOCATION"
        );
    }

    #[test]
    fn test_build_install_path_selects_configured_location_and_category() {
        let mut package = crate::package::Package::default();
        package
            .attributes
            .insert("package_type".into(), serde_json::json!("dcc"));
        let mut config = RezConfig {
            release_build_path: Some("/release/int".into()),
            local_packages_path: "/local/int".into(),
            ..RezConfig::default()
        };

        assert_eq!(
            config.build_install_path(&package, None, false),
            PathBuf::from("/release/dcc")
        );
        config.rez_install_categories = false;
        assert_eq!(
            config.build_install_path(&package, None, false),
            PathBuf::from("/release")
        );
        config.rez_install_location = false;
        assert_eq!(
            config.build_install_path(&package, None, false),
            PathBuf::from("/local")
        );
        config.rez_install_categories = true;
        assert_eq!(
            config.build_install_path(&package, None, false),
            PathBuf::from("/local/dcc")
        );
        config.release_build_path = None;
        config.release_packages_path = "/shared/int".into();
        config.rez_install_location = true;
        assert_eq!(
            config.build_install_path(&package, None, false),
            PathBuf::from("/shared/dcc")
        );
        assert_eq!(
            config.build_install_path(&package, Some(Path::new("/custom/packages")), false),
            PathBuf::from("/custom/packages")
        );
    }

    #[test]
    fn test_default_install_layout_and_search_agree() {
        let mut config = RezConfig::default();
        let mut package = crate::package::Package::default();
        package
            .attributes
            .insert("package_type".into(), serde_json::json!("dcc"));
        for shared in [true, false] {
            for categories in [true, false] {
                config.rez_install_location = shared;
                config.rez_install_categories = categories;
                let destination = config.build_install_path(&package, None, false);
                let base = if shared {
                    config.expanded_release_packages_path().to_os()
                } else {
                    config.expanded_local_packages_path().to_os()
                };
                let root = base.parent().unwrap();
                assert_eq!(
                    destination,
                    if categories {
                        root.join("dcc")
                    } else {
                        root.to_path_buf()
                    }
                );
                assert!(config.expanded_packages_path_os().contains(&destination));
                let release = config.build_install_path(&package, None, true);
                let shared_root = config.expanded_release_packages_path().to_os();
                let shared_root = shared_root.parent().unwrap();
                assert_eq!(
                    release,
                    if categories {
                        shared_root.join("dcc")
                    } else {
                        shared_root.to_path_buf()
                    }
                );
            }
        }
    }

    #[test]
    fn test_flat_layout_keeps_additional_repository_search_paths() {
        let config = RezConfig {
            rez_install_categories: false,
            packages_path: vec!["/repo/int".into()],
            release_packages_path: "/repo/int".into(),
            release_bind_path: Some("/bindings".into()),
            release_build_path: Some("/builds".into()),
            release_pip_path: None,
            ..RezConfig::default()
        };
        assert_eq!(
            config.expanded_packages_path_os(),
            ["/bindings", "/builds", "/repo"].map(PathBuf::from)
        );
    }

    #[test]
    fn test_category_repository_roots_expand_for_package_search() {
        let root = std::env::temp_dir().join("rez-rs-category-search");
        let config = RezConfig {
            packages_path: vec![
                root.join("int").to_string_lossy().into_owned(),
                root.join("ext").to_string_lossy().into_owned(),
            ],
            local_packages_path: root.join("local").to_string_lossy().into_owned(),
            release_packages_path: root.join("release").to_string_lossy().into_owned(),
            release_bind_path: None,
            release_pip_path: None,
            release_build_path: None,
            ..RezConfig::default()
        };

        let expected = ["int", "ext", "dcc", "pip", "tool"].map(|category| root.join(category));
        assert_eq!(config.expanded_packages_path_os(), expected);
    }

    #[test]
    fn test_configuration_environment_names_do_not_collide() {
        let config = serde_json::to_value(RezConfig::default()).unwrap();
        let keys = config.as_object().unwrap().keys();
        let mut names = HashMap::new();
        for key in keys {
            let environment_name = config_environment_name(key);
            assert!(
                names.insert(environment_name.clone(), key).is_none(),
                "environment name collision for {environment_name}"
            );
        }
    }

    #[test]
    fn test_default_config_outputs_install_routing_settings() {
        let python_config = RezConfig::default_config_py();
        assert!(python_config.contains("rez_install_categories = True"));
        assert!(python_config.contains("rez_install_location = True"));
        let toml = RezConfig::default_config_toml().unwrap();
        assert!(toml.contains("rez_install_categories = true"));
        assert!(toml.contains("rez_install_location = true"));
    }

    #[test]
    fn test_valid_environment_overrides_compose_and_follow_rez_precedence() {
        let overrides = HashMap::from([
            ("REZ_RESOLVE_CACHING".into(), "no".into()),
            ("REZ_RESOLVE_CACHING_JSON".into(), "true".into()),
            ("REZ_BUILD_DIRECTORY".into(), "from_plain".into()),
            ("REZ_BUILD_DIRECTORY_JSON".into(), "\"from_json\"".into()),
            ("REZ_RESOURCE_CACHING_MAXSIZE".into(), "42".into()),
            ("REZ_CACHE_PACKAGE_FILES_JSON".into(), "false".into()),
        ]);
        let mut config = RezConfig::default();

        config.apply_env_overrides(Some(&overrides)).unwrap();

        assert!(!config.resolve_caching);
        assert_eq!(config.build_directory, "from_plain");
        assert_eq!(config.resource_caching_maxsize, 42);
        assert!(!config.cache_package_files);
    }

    #[test]
    fn test_implicit_packages() {
        let config = RezConfig::default();
        let resolved = config.resolved_implicit_packages();
        assert_eq!(resolved.len(), 3);
        assert!(resolved[0].starts_with("~platform=="));
        assert!(resolved[1].starts_with("~arch=="));
        assert!(resolved[2].starts_with("~os=="));
    }

    #[test]
    fn test_warn_debug_flags() {
        let mut config = RezConfig::default();
        assert!(!config.is_warn_enabled("shell_startup"));

        config.warn_all = true;
        assert!(config.is_warn_enabled("shell_startup"));

        config.warn_none = true;
        assert!(!config.is_warn_enabled("shell_startup"));
    }

    #[test]
    fn test_debug_flags() {
        let mut config = RezConfig::default();
        assert!(!config.is_debug_enabled("plugins"));

        config.debug_plugins = true;
        assert!(config.is_debug_enabled("plugins"));

        config.debug_none = true;
        assert!(!config.is_debug_enabled("plugins"));
    }

    #[test]
    fn test_color_defaults() {
        let config = RezConfig::default();
        assert_eq!(config.critical_color.fore, Some("red".into()));
        assert_eq!(config.warning_color.fore, Some("yellow".into()));
        assert_eq!(config.info_color.fore, Some("green".into()));
    }

    #[test]
    fn test_py_config_in_search_chain() {
        // Verify that rezconfig.py is found and merged via merge_config_dir
        let dir = std::env::temp_dir().join("rez_test_py_config_chain");
        let _ = std::fs::create_dir_all(&dir);
        let py_file = dir.join("rezconfig.py");
        std::fs::write(&py_file, "build_directory = 'custom_build_dir'\n").unwrap();

        let mut config = RezConfig::default();
        assert_eq!(config.build_directory, "build");
        config.merge_config_dir(&dir).unwrap();
        assert_eq!(config.build_directory, "custom_build_dir");

        // Cleanup
        let _ = std::fs::remove_file(&py_file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_py_config_takes_precedence_over_toml() {
        // When both rezconfig.py and rezconfig.toml exist, .py wins
        let dir = std::env::temp_dir().join("rez_test_py_vs_toml");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("rezconfig.py"), "build_directory = 'from_py'\n").unwrap();
        std::fs::write(
            dir.join("rezconfig.toml"),
            "build_directory = \"from_toml\"\n",
        )
        .unwrap();

        let mut config = RezConfig::default();
        config.merge_config_dir(&dir).unwrap();
        assert_eq!(config.build_directory, "from_py");

        // Cleanup
        let _ = std::fs::remove_file(dir.join("rezconfig.py"));
        let _ = std::fs::remove_file(dir.join("rezconfig.toml"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_load_rezconfig_py_content() {
        // Verify the Python VM execution path for rezconfig-style code
        let code = "\
packages_path = ['/opt/rez/packages', '/home/user/packages']\n\
local_packages_path = '/home/user/.rez/packages'\n\
resolve_caching = True\n\
package_filter = None\n\
";
        let preamble = "class ModifyList(list): pass\nclass DelayLoad:\n    def __init__(self, func): self.func = func\n";
        let full = format!("{}\n{}", preamble, code);
        let result = python_runtime::exec_py_globals(
            &full,
            "rezconfig.py",
            None,
            &["ModifyList", "DelayLoad"],
        )
        .unwrap();

        assert_eq!(
            result["local_packages_path"],
            serde_json::Value::String("/home/user/.rez/packages".into())
        );
        assert_eq!(result["resolve_caching"], serde_json::Value::Bool(true));
        assert_eq!(result["package_filter"], serde_json::Value::Null);
        let paths = result["packages_path"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_config_amqp_and_pip_remaps() {
        let config = RezConfig::default();
        assert_eq!(config.context_tracking_amqp.len(), 6);
        assert_eq!(
            config.context_tracking_amqp["exchange_routing_key"],
            "REZ.CONTEXT"
        );
        assert_eq!(config.pip_install_remaps.len(), 3);
        assert_eq!(
            config.pip_install_remaps[1]["record_path"],
            r"^{p}{s}{p}{s}lib{s}python{s}(.*)"
        );
        assert_eq!(
            config.pip_install_remaps[2]["record_path"],
            r"^(?:{p}{s})+(.+)"
        );
        assert_eq!(config.pip_install_remaps[2]["pip_install"], r"\1");
        assert_eq!(config.pip_install_remaps[2]["rez_install"], r"\1");
        assert_eq!(
            config.pip_install_remaps[0]["record_path"],
            r"^{p}{s}{p}{s}(bin{s}.*)"
        );
    }

    #[test]
    fn test_expanded_nonlocal_packages_path_removes_only_one_exact_local_entry() {
        let local = std::env::temp_dir().join("rez-rs-local");
        let remote = std::env::temp_dir().join("rez-rs-remote");
        let bind = std::env::temp_dir().join("rez-rs-bind");
        let local_text = local.to_string_lossy().into_owned();
        let remote_text = remote.to_string_lossy().into_owned();

        let mut config = RezConfig {
            local_packages_path: local_text.clone(),
            packages_path: vec![local_text.clone(), remote_text.clone(), local_text.clone()],
            release_packages_path: remote_text.clone(),
            release_bind_path: Some(bind.to_string_lossy().into_owned()),
            ..RezConfig::default()
        };

        assert_eq!(
            config.expanded_nonlocal_packages_path_os(),
            vec![remote.clone(), local.clone()]
        );

        config.local_packages_path = std::env::temp_dir()
            .join("rez-rs-other-local")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            config.expanded_nonlocal_packages_path_os(),
            vec![local.clone(), remote.clone(), local]
        );
    }

    #[test]
    fn test_package_definition_stems_uses_rez_nested_setting_order() {
        let mut config = RezConfig::default();
        config.plugins.insert(
            "package_repository".into(),
            serde_json::json!({
                "filesystem": {
                    "package_filenames": ["studio_package", "package"]
                }
            }),
        );

        assert_eq!(
            config.package_definition_stems().unwrap(),
            vec!["studio_package".to_string(), "package".to_string()]
        );
    }

    #[test]
    fn test_package_definition_stems_defaults_only_when_setting_is_missing() {
        let mut config = RezConfig::default();
        assert_eq!(config.package_definition_stems().unwrap(), ["package"]);

        config
            .plugins
            .insert("package_repository".into(), serde_json::json!({}));
        assert_eq!(config.package_definition_stems().unwrap(), ["package"]);

        config.plugins.insert(
            "package_repository".into(),
            serde_json::json!({"filesystem": {}}),
        );
        assert_eq!(config.package_definition_stems().unwrap(), ["package"]);
    }

    #[test]
    fn test_package_definition_stems_matches_rez_string_list_contract() {
        let mut config = RezConfig::default();
        config.plugins.insert(
            "package_repository".into(),
            serde_json::json!({"filesystem": {"package_filenames": ["studio_package", "package", "studio_package"]}}),
        );
        assert_eq!(
            config.package_definition_stems().unwrap(),
            ["studio_package", "package", "studio_package"]
        );

        config.plugins.insert(
            "package_repository".into(),
            serde_json::json!({"filesystem": {"package_filenames": []}}),
        );
        assert!(config.package_definition_stems().unwrap().is_empty());

        config.plugins.insert(
            "package_repository".into(),
            serde_json::json!({"filesystem": {"package_filenames": [""]}}),
        );
        assert_eq!(config.package_definition_stems().unwrap(), [""]);
    }

    #[test]
    fn test_package_definition_stems_rejects_malformed_containers_and_elements() {
        for plugins in [
            serde_json::json!({"package_repository": "filesystem"}),
            serde_json::json!({"package_repository": {"filesystem": []}}),
            serde_json::json!({"package_repository": {"filesystem": null}}),
            serde_json::json!({"package_repository": {"filesystem": {"package_filenames": "package"}}}),
            serde_json::json!({"package_repository": {"filesystem": {"package_filenames": 1}}}),
            serde_json::json!({"package_repository": {"filesystem": {"package_filenames": ["package", 2]}}}),
            serde_json::json!({"package_repository": {"filesystem": {"package_filenames": [null]}}}),
        ] {
            let config = RezConfig {
                plugins: plugins.as_object().unwrap().clone().into_iter().collect(),
                ..RezConfig::default()
            };
            assert!(
                config.package_definition_stems().is_err(),
                "accepted malformed plugins: {plugins}"
            );
        }
    }

    #[test]
    fn test_package_filename_type_errors_propagate_from_toml_and_python_sources() {
        let dir = std::env::temp_dir().join(format!(
            "rez_package_filename_source_errors_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let toml_path = dir.join("malformed.toml");
        std::fs::write(
            &toml_path,
            "[plugins.package_repository.filesystem]\npackage_filenames = 'package'\n",
        )
        .unwrap();
        let mut toml_config = RezConfig::default();
        toml_config.merge_toml_file(&toml_path).unwrap();
        let toml_error = toml_config
            .package_definition_stems()
            .unwrap_err()
            .to_string();
        assert!(toml_error.contains("package_filenames"));

        let py_path = dir.join("malformed.py");
        std::fs::write(
            &py_path,
            "plugins = {'package_repository': {'filesystem': {'package_filenames': ['package', 2]}}}\n",
        )
        .unwrap();
        let mut py_config = RezConfig::default();
        py_config.merge_py_file(&py_path).unwrap();
        let py_error = py_config
            .package_definition_stems()
            .unwrap_err()
            .to_string();
        assert!(py_error.contains("package_filenames[1]"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_plugin_config_layers_deep_merge_and_replace_package_filename_lists() {
        let mut config = RezConfig::default();
        config
            .merge_config_values(
                "base",
                HashMap::from([(
                    "plugins".into(),
                    serde_json::json!({
                        "package_repository": {
                            "filesystem": {
                                "package_filenames": ["base", "fallback"],
                                "other_setting": true
                            },
                            "other_plugin": {"enabled": true}
                        }
                    }),
                )]),
            )
            .unwrap();
        config
            .merge_config_values(
                "override",
                HashMap::from([(
                    "plugins".into(),
                    serde_json::json!({
                        "package_repository": {
                            "filesystem": {"package_filenames": ["studio", "package"]}
                        }
                    }),
                )]),
            )
            .unwrap();

        assert_eq!(
            config.package_definition_stems().unwrap(),
            ["studio", "package"]
        );
        assert_eq!(
            config.plugins["package_repository"]["filesystem"]["other_setting"],
            true
        );
        assert_eq!(
            config.plugins["package_repository"]["other_plugin"]["enabled"],
            true
        );
    }

    #[test]
    fn test_plugin_config_layer_validation_uses_effective_override_value() {
        let mut config = RezConfig::default();
        config
            .merge_config_values(
                "base",
                HashMap::from([(
                    "plugins".into(),
                    serde_json::json!({"package_repository": {"filesystem": {"package_filenames": "malformed"}}}),
                )]),
            )
            .unwrap();
        config
            .merge_config_values(
                "override",
                HashMap::from([(
                    "plugins".into(),
                    serde_json::json!({"package_repository": {"filesystem": {"package_filenames": ["package"]}}}),
                )]),
            )
            .unwrap();
        assert_eq!(config.package_definition_stems().unwrap(), ["package"]);

        config
            .merge_config_values(
                "invalid-final-override",
                HashMap::from([(
                    "plugins".into(),
                    serde_json::json!({"package_repository": {"filesystem": {"package_filenames": ["package", false]}}}),
                )]),
            )
            .unwrap();
        assert!(config.package_definition_stems().is_err());
    }

    #[test]
    fn test_expand_env_vars() {
        // Set test env vars
        std::env::set_var("TEST_VAR", "test_value");
        std::env::set_var("ANOTHER_VAR", "another_value");

        // Test ${VAR} syntax
        let result = RezConfig::expand_env_vars("path/${TEST_VAR}/subdir");
        assert_eq!(result, "path/test_value/subdir");

        // Test $VAR syntax
        let result = RezConfig::expand_env_vars("path/$TEST_VAR/subdir");
        assert_eq!(result, "path/test_value/subdir");

        // Test multiple vars
        let result = RezConfig::expand_env_vars("${TEST_VAR}/$ANOTHER_VAR");
        assert_eq!(result, "test_value/another_value");

        // Test non-existent var (should keep original)
        let result = RezConfig::expand_env_vars("${NONEXISTENT_VAR}");
        assert_eq!(result, "${NONEXISTENT_VAR}");

        // Cleanup
        std::env::remove_var("TEST_VAR");
        std::env::remove_var("ANOTHER_VAR");
    }

    #[test]
    fn test_platform_map_schema_and_configured_expansion() {
        let mut config = RezConfig::default();
        config.merge_config_values("platform mapping fixture", HashMap::from([
            ("platform_map".into(), serde_json::json!({"arch": {"^.*$": "mapped_arch"}, "os": {"^.*$": "mapped_os"}})),
            ("variant_select_mode".into(), serde_json::json!("intersection_priority")),
            ("packages_path".into(), serde_json::json!(["/packages/{system.arch}/{system.os}"])),
        ])).unwrap();
        config.expand_all_values().unwrap();
        assert_eq!(
            config.variant_select_mode,
            VariantSelectMode::IntersectionPriority
        );
        assert_eq!(config.packages_path, ["/packages/mapped_arch/mapped_os"]);
        assert!(config
            .resolved_implicit_packages()
            .contains(&"~arch==mapped_arch".into()));
        assert!(config
            .resolved_implicit_packages()
            .contains(&"~os==mapped_os".into()));
        assert!(serde_json::to_value(&config)
            .unwrap()
            .get("system_info")
            .is_none());
    }

    #[test]
    fn test_platform_map_python_source_order_and_config_expansion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rezconfig.py");
        std::fs::write(
            &path,
            r#"
platform_map = {
    "arch": {r"(.+)": r"first-\1", r".*": "incorrect"},
    "os": {
        r"windows-10\.0(\.1.*)": r"windows-10\1",
        r"windows-10\.0(\.2.*)": r"windows-11\1",
        r"windows": "incorrect",
    },
}
packages_path = ["/packages/{system.arch}/{system.os}"]
variant_select_mode = "intersection_priority"
"#,
        )
        .unwrap();
        let mut config = RezConfig::default();
        config.merge_py_file(&path).unwrap();
        assert_eq!(config.platform_map["arch"].0[0].0, "(.+)");
        assert_eq!(config.platform_map["arch"].0[1].0, ".*");
        assert_eq!(config.platform_map["os"].0[0].0, r"windows-10\.0(\.1.*)");
        config.expand_all_values().unwrap();
        let raw = &*crate::platform::RAW_SYSTEM;
        let expected_arch = format!("first-{}", raw.arch);
        let expected_os = raw
            .os
            .name()
            .strip_prefix("windows-10.0")
            .filter(|build| build.starts_with(".1") || build.starts_with(".2"))
            .map(|build| {
                format!(
                    "windows-{}{}",
                    if build.starts_with(".2") { "11" } else { "10" },
                    build
                )
            })
            .unwrap_or_else(|| raw.os.name().replacen("windows", "incorrect", 1));
        assert_eq!(
            config.system_info.as_ref().unwrap().arch.name(),
            expected_arch
        );
        assert_eq!(config.system_info.as_ref().unwrap().os.name(), expected_os);
        assert_eq!(
            config.packages_path,
            [format!("/packages/{expected_arch}/{expected_os}")]
        );
        assert_eq!(
            config.variant_select_mode,
            VariantSelectMode::IntersectionPriority
        );
    }

    #[test]
    fn test_platform_map_regex_is_not_template_expanded() {
        let mut config = RezConfig {
            platform_map: serde_json::from_str(
                r#"{"arch":{"^.*$":"mapped_arch"},"unused":{"^$HOME$":"\\g<0>"}}"#,
            )
            .unwrap(),
            ..RezConfig::default()
        };
        let before = serde_json::to_value(&config.platform_map).unwrap();
        config.expand_all_values().unwrap();
        assert_eq!(serde_json::to_value(&config.platform_map).unwrap(), before);
    }

    #[test]
    fn test_expand_config_value_system_vars() {
        use serde_json::Value;

        let sys = &*crate::platform::SYSTEM;

        // Test system.platform expansion
        let input = Value::String("/packages/{system.platform}".to_string());
        let result = RezConfig::expand_config_value(input, sys);
        assert_eq!(
            result,
            Value::String(format!("/packages/{}", sys.platform.name()))
        );

        // Test system.arch expansion
        let input = Value::String("/arch/{system.arch}".to_string());
        let result = RezConfig::expand_config_value(input, sys);
        assert_eq!(result, Value::String(format!("/arch/{}", sys.arch)));

        // Test system.os expansion
        let input = Value::String("/os/{system.os}".to_string());
        let result = RezConfig::expand_config_value(input, sys);
        assert_eq!(result, Value::String(format!("/os/{}", sys.os)));

        // Test array expansion
        let input = Value::Array(vec![
            Value::String("{system.platform}/path1".to_string()),
            Value::String("{system.arch}/path2".to_string()),
        ]);
        let result = RezConfig::expand_config_value(input, sys);
        if let Value::Array(arr) = result {
            assert_eq!(
                arr[0],
                Value::String(format!("{}/path1", sys.platform.name()))
            );
            assert_eq!(arr[1], Value::String(format!("{}/path2", sys.arch)));
        } else {
            panic!("Expected array");
        }
    }

    #[test]
    fn test_home_config_disabled() {
        // Test enabled (default)
        std::env::remove_var("REZ_DISABLE_HOME_CONFIG");
        assert!(!RezConfig::home_config_disabled());

        // Test disabled with "1"
        std::env::set_var("REZ_DISABLE_HOME_CONFIG", "1");
        assert!(RezConfig::home_config_disabled());

        // Test disabled with "true"
        std::env::set_var("REZ_DISABLE_HOME_CONFIG", "true");
        assert!(RezConfig::home_config_disabled());

        // Test disabled with "t"
        std::env::set_var("REZ_DISABLE_HOME_CONFIG", "t");
        assert!(RezConfig::home_config_disabled());

        // Test disabled with "yes"
        std::env::set_var("REZ_DISABLE_HOME_CONFIG", "yes");
        assert!(RezConfig::home_config_disabled());

        // Test case insensitive
        std::env::set_var("REZ_DISABLE_HOME_CONFIG", "TRUE");
        assert!(RezConfig::home_config_disabled());

        // Test invalid value (not disabled)
        std::env::set_var("REZ_DISABLE_HOME_CONFIG", "maybe");
        assert!(!RezConfig::home_config_disabled());

        // Cleanup
        std::env::remove_var("REZ_DISABLE_HOME_CONFIG");
    }
}
