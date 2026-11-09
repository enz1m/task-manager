//! Desktop entry names and icons.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct DesktopApp {
    pub(crate) name: String,
    pub(crate) desktop_id: String,
    pub(crate) exec_names: Vec<String>,
    pub(crate) icon: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessKind {
    App,
    Service,
    System,
}

impl ProcessKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Service => "Service",
            Self::System => "System",
        }
    }
}

pub(crate) fn classify_process(comm: &str, command: &str, app: Option<&DesktopApp>) -> String {
    let lower = format!("{comm} {command}").to_ascii_lowercase();
    let kind = if is_obvious_helper_process(&lower) {
        ProcessKind::System
    } else if app.is_some() || is_known_gui_process(comm, command) {
        ProcessKind::App
    } else if lower.contains("systemd") || comm.to_ascii_lowercase().ends_with('d') {
        ProcessKind::Service
    } else {
        ProcessKind::System
    };
    kind.as_str().to_owned()
}

pub(crate) fn is_obvious_helper_process(lower: &str) -> bool {
    [
        "-contentproc",
        "--type=",
        "isolated web co",
        "isolated servic",
        "web content",
        "gpu process",
        "utility process",
        "extension process",
        "zygote",
        "bwrap",
        "bubblewrap",
        "flatpak-session-helper",
        "xdg-dbus-proxy",
        "gvfsd-metadata",
        "glycin",
        "kworker",
        "dbus-daemon",
        "pipewire",
        "wireplumber",
        "xdg-desktop-portal",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) fn is_known_gui_process(comm: &str, command: &str) -> bool {
    const KNOWN: [&str; 32] = [
        "chrome",
        "chromium",
        "firefox",
        "zen",
        "brave",
        "code",
        "codium",
        "enzimcoder",
        "nautilus",
        "dolphin",
        "konsole",
        "gnome-terminal",
        "alacritty",
        "kitty",
        "foot",
        "wezterm",
        "slack",
        "discord",
        "vesktop",
        "signal",
        "telegram",
        "thunderbird",
        "spotify",
        "obsidian",
        "steam",
        "vlc",
        "gimp",
        "inkscape",
        "blender",
        "libreoffice",
        "soffice.bin",
        "zathura",
    ];
    let executable = command.split_whitespace().next().unwrap_or_default();
    [
        normalize_process_name(comm),
        normalize_process_name(executable),
    ]
    .iter()
    .any(|name| {
        KNOWN.iter().any(|known| {
            name == known
                || name
                    .strip_prefix(known)
                    .is_some_and(|rest| rest.starts_with('-'))
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameSource<'a> {
    KernelComm(&'a str),
    ExecutablePath(&'a str),
    Missing,
}

pub(crate) fn classify_name_source(command: &str) -> NameSource<'_> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return NameSource::Missing;
    }
    // Bracketed kernel comm names can contain spaces and slashes.
    if let Some(comm) = bracketed_comm(trimmed) {
        return NameSource::KernelComm(comm);
    }
    match trimmed.split_whitespace().next() {
        Some(argv0) if !argv0.is_empty() => NameSource::ExecutablePath(argv0),
        _ => NameSource::Missing,
    }
}

pub(crate) fn bracketed_comm(value: &str) -> Option<&str> {
    if !value.starts_with('[') {
        return None;
    }
    value.find(']').and_then(|end| value.get(..=end))
}

pub(crate) fn display_process_name(comm: &str, command: &str) -> String {
    match classify_name_source(command) {
        // A kernel comm is a name, so path basename extraction would lose part of it.
        NameSource::KernelComm(name) => name.to_owned(),
        NameSource::ExecutablePath(argv0) => Path::new(argv0)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or(comm)
            .to_owned(),
        NameSource::Missing => comm.to_owned(),
    }
}

#[derive(Debug, Default)]
pub(crate) struct DesktopIndex {
    apps: Vec<DesktopApp>,
    by_exec: std::collections::HashMap<String, usize>,
    by_id: std::collections::HashMap<String, usize>,
    by_name: std::collections::HashMap<String, usize>,
}

impl DesktopIndex {
    pub(crate) fn icon_for_name(&self, name: &str) -> Option<&str> {
        let app = &self.apps[*self.by_name.get(name)?];
        app.icon.as_deref()
    }
}

impl From<Vec<DesktopApp>> for DesktopIndex {
    fn from(apps: Vec<DesktopApp>) -> Self {
        let mut by_exec = std::collections::HashMap::new();
        let mut by_id = std::collections::HashMap::new();
        let mut by_name = std::collections::HashMap::new();
        for (at, app) in apps.iter().enumerate() {
            by_name.entry(app.name.clone()).or_insert(at);
            for exec in &app.exec_names {
                by_exec.entry(exec.clone()).or_insert(at);
            }
            by_id.entry(app.desktop_id.clone()).or_insert(at);
        }
        Self {
            apps,
            by_exec,
            by_id,
            by_name,
        }
    }
}

pub(crate) fn desktop_app_index() -> &'static DesktopIndex {
    // The desktop index remains unchanged for this process lifetime.
    static DESKTOP_APPS: OnceLock<DesktopIndex> = OnceLock::new();
    DESKTOP_APPS.get_or_init(|| DesktopIndex::from(load_desktop_apps()))
}

pub(crate) fn load_desktop_apps() -> Vec<DesktopApp> {
    let mut apps = Vec::new();
    for dir in desktop_app_dirs() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(app) = parse_desktop_file(&path) {
                apps.push(app);
            }
        }
    }
    apps
}

pub(crate) fn desktop_app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share/applications"));
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs
}

pub(crate) fn parse_desktop_file(path: &Path) -> Option<DesktopApp> {
    let source = fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut hidden = false;
    for line in source.lines() {
        let line = line.trim();
        if line == "NoDisplay=true" || line == "Hidden=true" {
            hidden = true;
        } else if let Some(value) = line.strip_prefix("Name=") {
            name.get_or_insert_with(|| value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("Exec=") {
            exec.get_or_insert_with(|| value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("Icon=") {
            icon.get_or_insert_with(|| value.trim().to_owned());
        }
    }
    if hidden {
        return None;
    }
    let name = name?;
    let exec_names = exec
        .map(|exec| desktop_exec_names(&exec))
        .filter(|names| !names.is_empty())
        .unwrap_or_default();
    let desktop_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(DesktopApp {
        name,
        desktop_id,
        exec_names,
        icon: icon.filter(|icon| !icon.is_empty()),
    })
}

pub(crate) fn desktop_exec_names(exec: &str) -> Vec<String> {
    let mut names = Vec::new();
    for part in exec.split_whitespace() {
        if part.starts_with('%') || part.contains('=') {
            if let Some(command) = part.strip_prefix("--command=") {
                let normalized = normalize_process_name(command);
                if !normalized.is_empty() {
                    names.push(normalized);
                }
            }
            continue;
        }
        let value = part.trim_matches('"').trim_matches('\'');
        if value.is_empty() {
            continue;
        }
        let Some(file_name) = Path::new(value).file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let normalized = normalize_process_name(file_name);
        if is_desktop_exec_candidate(&normalized) {
            names.push(normalized);
        }
        if names.len() >= 2 {
            break;
        }
    }
    names.sort();
    names.dedup();
    names
}

pub(crate) fn is_desktop_exec_candidate(name: &str) -> bool {
    !name.is_empty()
        && name.chars().any(char::is_alphanumeric)
        && !name.contains('.')
        && !matches!(
            name,
            "env"
                | "sh"
                | "bash"
                | "flatpak"
                | "run"
                | "gtk-launch"
                | "dbus-run-session"
                | "bwrap"
                | "bubblewrap"
                | "@@"
                | "--file-forwarding"
        )
        && !name.starts_with("--")
}

pub(crate) fn match_desktop_app<'a>(
    index: &'a DesktopIndex,
    comm: &str,
    command: &str,
) -> Option<&'a DesktopApp> {
    let comm = normalize_process_name(comm);
    let command_tokens = normalized_command_tokens(command);
    let by_exec = std::iter::once(&comm)
        .chain(&command_tokens)
        .filter_map(|name| index.by_exec.get(name));
    let by_id = command_tokens
        .iter()
        .filter_map(|token| index.by_id.get(token));
    by_exec.chain(by_id).min().map(|&at| &index.apps[at])
}

pub(crate) fn normalized_command_tokens(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches('"')
                .trim_matches('\'')
                .trim_start_matches("--command=")
        })
        .map(normalize_process_name)
        .filter(|token| !token.is_empty())
        .collect()
}

pub(crate) fn normalize_process_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(name)
        .trim_start_matches("lt-")
        .trim_end_matches("-bin")
        .to_ascii_lowercase()
}
