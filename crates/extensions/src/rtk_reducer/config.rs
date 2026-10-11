use crabber::ExtensionError;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
    time::Duration,
};

/// Release whose four pipe filters are supported.
pub const RTK_VERSION: &str = "0.51.0";
/// Conventional ordinary-phase transform order.
pub const DEFAULT_ORDER: i32 = 1000;

/// Fixed text filters. Hosts assert the input format; no auto-detection occurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Filter {
    /// Cargo test output; an addition beyond the Go reference.
    CargoTest,
    /// Go test output.
    GoTest,
    /// Git diff output.
    GitDiff,
    /// Repetitive logs.
    Log,
}
impl Filter {
    /// Argument passed after `rtk pipe --filter`.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::CargoTest => "cargo-test",
            Self::GoTest => "go-test",
            Self::GitDiff => "git-diff",
            Self::Log => "log",
        }
    }
}
/// One selected top-level string member.
#[derive(Clone, Debug, Serialize)]
pub struct Field {
    /// Nonempty member name without NUL.
    pub name: String,
    /// Host-asserted text format.
    pub filter: Filter,
}
/// Optional exact comparison against a normalized input string member.
#[derive(Clone, Debug, Serialize)]
pub struct InputMatch {
    /// Nonempty top-level input member name without NUL.
    pub field: String,
    /// Distinct allowed values, compared byte for byte.
    pub equals: Vec<String>,
}
/// Selected output representation; nested paths are unsupported.
#[derive(Clone, Debug, Serialize)]
pub enum Selection {
    /// A result that is one JSON string.
    Text(Filter),
    /// Named top-level string members, processed in canonical name order.
    JsonFields(Vec<Field>),
}
/// One binding per tool name.
#[derive(Clone, Debug, Serialize)]
pub struct Binding {
    /// Exact tool name.
    pub tool_name: String,
    /// Optional exact normalized-input condition.
    pub match_input: Option<InputMatch>,
    /// Output selection.
    pub selection: Selection,
}
/// Positive resource bounds, all included in the configuration fingerprint.
#[derive(Clone, Debug, Serialize)]
pub struct Limits {
    /// Bindings per instance; 1–256.
    pub max_bindings: usize,
    /// Members per JSON selection; 1–256.
    pub max_fields_per_binding: usize,
    /// Allowed values per input match; 1–256.
    pub max_input_matches: usize,
    /// Sum of binding text bytes; 1–1 MiB.
    pub max_config_bytes: usize,
    /// Matched input string UTF-8 bytes; 1–1 MiB.
    pub max_input_bytes: usize,
    /// Selected string UTF-8 bytes; 1–1 MiB.
    pub max_field_bytes: usize,
    /// Concurrent transform leases; 1–64. Capacity never queues.
    pub max_concurrent: usize,
    /// Child stdout bytes; 1–1 MiB, at most max_field_bytes.
    pub max_stdout_bytes: usize,
    /// One deadline across every field in a transform; positive, at most 60 s.
    pub timeout: Duration,
    /// Reaping bound after KILL; positive, at most 60 s.
    pub kill_wait: Duration,
    /// Close bound; positive, at most 120 s, at least timeout + kill_wait.
    pub shutdown_grace: Duration,
}
/// Host-owned executable and temporary root. Construction never executes RTK.
#[derive(Clone)]
pub struct Options {
    /// Explicit ordinary-phase order.
    pub order: i32,
    /// Absolute normalized path, canonicalized before execution. The host must
    /// keep its target immutable throughout this mount's lifetime.
    pub executable_path: PathBuf,
    /// Exactly 64 lowercase SHA-256 hex characters. Verified at construction
    /// only; this proves identity, not provenance or execution-time integrity.
    pub executable_sha256: String,
    /// Existing absolute private root, owned by the host euid and without
    /// group/other write permission. Ancestors are not inspected.
    pub temp_root: PathBuf,
    /// Nonempty exact tool bindings.
    pub bindings: Vec<Binding>,
    /// Resource bounds.
    pub limits: Limits,
}
impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("order", &self.order)
            .field("bindings", &self.bindings.len())
            .field("limits", &self.limits)
            .finish()
    }
}
pub(super) const PRIVATE_KEYS: [(&str, &str); 7] = [
    ("HOME", "home"),
    ("XDG_CONFIG_HOME", "config"),
    ("XDG_DATA_HOME", "data"),
    ("XDG_STATE_HOME", "state"),
    ("XDG_CACHE_HOME", "cache"),
    ("CLAUDE_CONFIG_DIR", "claude"),
    ("TMPDIR", "tmp"),
];
pub(super) const FIXED_ENV: [(&str, &str); 5] = [
    ("RTK_TELEMETRY_DISABLED", "1"),
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C"),
    ("LC_ALL", "C"),
];
#[derive(Clone, Serialize)]
pub(super) struct CanonicalBinding {
    pub(super) match_input: Option<(String, BTreeSet<String>)>,
    pub(super) selection: Selection,
}
#[derive(Clone)]
pub(super) struct Configuration {
    pub(super) executable: PathBuf,
    pub(super) temp_root: PathBuf,
    pub(super) bindings: BTreeMap<String, CanonicalBinding>,
    pub(super) limits: Limits,
    pub(super) order: i32,
}
pub(super) fn validate(options: Options) -> Result<(Configuration, String), ExtensionError> {
    #[cfg(not(unix))]
    {
        let _ = options;
        Err(crate::config_error("unsupported-platform"))
    }
    #[cfg(unix)]
    {
        validate_unix(options)
    }
}
#[cfg(unix)]
fn normalized(path: &std::path::Path) -> bool {
    use std::{os::unix::ffi::OsStrExt, path::Component};
    path.is_absolute()
        && path.as_os_str().len() <= 4096
        && !path.as_os_str().as_bytes().contains(&0)
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
        && !path.components().any(|c| c == Component::ParentDir)
}
#[cfg(unix)]
fn validate_unix(options: Options) -> Result<(Configuration, String), ExtensionError> {
    use sha2::{Digest, Sha256};
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };
    let l = &options.limits;
    for (value, cap) in [
        (l.max_bindings, 256),
        (l.max_fields_per_binding, 256),
        (l.max_input_matches, 256),
        (l.max_config_bytes, 1024 * 1024),
        (l.max_input_bytes, 1024 * 1024),
        (l.max_field_bytes, 1024 * 1024),
        (l.max_concurrent, 64),
        (l.max_stdout_bytes, 1024 * 1024),
    ] {
        if value == 0 || value > cap {
            return Err(crate::config_error("limits"));
        }
    }
    if l.max_stdout_bytes > l.max_field_bytes {
        return Err(crate::config_error("limit-order"));
    }
    if [l.timeout, l.kill_wait]
        .iter()
        .any(|d| d.is_zero() || *d > Duration::from_secs(60))
    {
        return Err(crate::config_error("timeouts"));
    }
    if l.shutdown_grace.is_zero()
        || l.shutdown_grace > Duration::from_secs(120)
        || l.shutdown_grace < l.timeout + l.kill_wait
    {
        return Err(crate::config_error("shutdown-grace"));
    }
    if options.executable_sha256.len() != 64
        || !options
            .executable_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(crate::config_error("executable-digest"));
    }
    if options.bindings.is_empty() {
        return Err(crate::config_error("bindings-required"));
    }
    if options.bindings.len() > l.max_bindings {
        return Err(crate::config_error("binding-count"));
    }
    let mut bytes = 0usize;
    for b in &options.bindings {
        bytes = bytes.saturating_add(b.tool_name.len());
        match &b.selection {
            Selection::Text(f) => bytes = bytes.saturating_add(f.cli_name().len()),
            Selection::JsonFields(fields) => {
                for f in fields {
                    bytes = bytes
                        .saturating_add(f.name.len())
                        .saturating_add(f.filter.cli_name().len());
                }
            }
        }
        if let Some(m) = &b.match_input {
            bytes = bytes.saturating_add(m.field.len());
            for v in &m.equals {
                bytes = bytes.saturating_add(v.len());
            }
        }
    }
    if bytes > l.max_config_bytes {
        return Err(crate::config_error("config-bytes"));
    }
    let mut bindings = BTreeMap::new();
    for b in options.bindings {
        if !crate::valid_identity(&b.tool_name)
            || b.tool_name.chars().any(char::is_whitespace)
            || b.tool_name.contains('/')
        {
            return Err(crate::config_error("tool-name"));
        }
        if bindings.contains_key(&b.tool_name) {
            return Err(crate::config_error("duplicate-tool"));
        }
        let mut selection = b.selection;
        if let Selection::JsonFields(fields) = &mut selection {
            if fields.is_empty() {
                return Err(crate::config_error("json-fields"));
            }
            if fields.len() > l.max_fields_per_binding {
                return Err(crate::config_error("field-count"));
            }
            let mut names = BTreeSet::new();
            for f in fields.iter() {
                if f.name.is_empty() || f.name.contains('\0') {
                    return Err(crate::config_error("field-name"));
                }
                if !names.insert(&f.name) {
                    return Err(crate::config_error("duplicate-field"));
                }
            }
            fields.sort_by(|a, b| (&a.name, a.filter).cmp(&(&b.name, b.filter)));
        }
        let match_input = if let Some(m) = b.match_input {
            if m.field.is_empty() || m.field.contains('\0') {
                return Err(crate::config_error("match-field"));
            }
            if m.equals.is_empty() || m.equals.len() > l.max_input_matches {
                return Err(crate::config_error("input-match-count"));
            }
            let mut values = BTreeSet::new();
            for v in m.equals {
                if v.contains('\0') || v.len() > l.max_input_bytes {
                    return Err(crate::config_error("input-match-value"));
                }
                if !values.insert(v) {
                    return Err(crate::config_error("duplicate-input-match"));
                }
            }
            Some((m.field, values))
        } else {
            None
        };
        bindings.insert(
            b.tool_name,
            CanonicalBinding {
                match_input,
                selection,
            },
        );
    }
    if !normalized(&options.executable_path) {
        return Err(crate::config_error("executable-path"));
    }
    let executable = options
        .executable_path
        .canonicalize()
        .map_err(|_| crate::config_error("executable-path"))?;
    let metadata = executable
        .metadata()
        .map_err(|_| crate::config_error("executable-file"))?;
    const MAX_EXECUTABLE: u64 = 128 * 1024 * 1024;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.len() > MAX_EXECUTABLE
    {
        return Err(crate::config_error("executable-file"));
    }
    let mut file =
        std::fs::File::open(&executable).map_err(|_| crate::config_error("executable-file"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 32 * 1024];
    let mut total = 0u64;
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| crate::config_error("executable-file"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_EXECUTABLE {
            return Err(crate::config_error("executable-file"));
        }
        hasher.update(&buffer[..n]);
    }
    if format!("{:x}", hasher.finalize()) != options.executable_sha256 {
        return Err(crate::config_error("executable-digest-mismatch"));
    }
    if !normalized(&options.temp_root) {
        return Err(crate::config_error("temp-root"));
    }
    let temp_root = options
        .temp_root
        .canonicalize()
        .map_err(|_| crate::config_error("temp-root"))?;
    let metadata = temp_root
        .metadata()
        .map_err(|_| crate::config_error("temp-root"))?;
    if !metadata.is_dir() {
        return Err(crate::config_error("temp-root"));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(crate::config_error("temp-root-mode"));
    }
    validate_owner(metadata.uid(), rustix::process::geteuid().as_raw())?;
    let canonical: Vec<_> = bindings
        .iter()
        .map(|(name, b)| (name, &b.match_input, &b.selection))
        .collect();
    let hash = crate::config_hash(&(
        "rtk-reducer-v1",
        RTK_VERSION,
        "private-env-v1",
        "marker-v1",
        PRIVATE_KEYS,
        FIXED_ENV,
        options.executable_sha256,
        options.order,
        canonical,
        l,
    ));
    Ok((
        Configuration {
            executable,
            temp_root,
            bindings,
            limits: options.limits,
            order: options.order,
        },
        hash,
    ))
}
#[cfg(unix)]
pub(super) fn validate_owner(uid: u32, expected: u32) -> Result<(), ExtensionError> {
    if uid != expected {
        return Err(crate::config_error("temp-root-owner"));
    }
    Ok(())
}

#[cfg(all(test, not(unix)))]
mod unsupported_tests {
    use super::*;
    #[test]
    fn unsupported_platform_rejects_first() {
        let o = Options {
            order: 0,
            executable_path: PathBuf::new(),
            executable_sha256: String::new(),
            temp_root: PathBuf::new(),
            bindings: vec![],
            limits: Limits {
                max_bindings: 0,
                max_fields_per_binding: 0,
                max_input_matches: 0,
                max_config_bytes: 0,
                max_input_bytes: 0,
                max_field_bytes: 0,
                max_concurrent: 0,
                max_stdout_bytes: 0,
                timeout: Duration::ZERO,
                kill_wait: Duration::ZERO,
                shutdown_grace: Duration::ZERO,
            },
        };
        assert!(
            validate(o)
                .err()
                .unwrap()
                .to_string()
                .ends_with("unsupported-platform")
        );
    }
}
