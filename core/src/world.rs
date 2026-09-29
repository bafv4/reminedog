//! Working out which world or server the player is in.
//!
//! Multiplayer servers come from the `Connecting to <host>, <port>` line in `logs/latest.log`.
//! For singleplayer the log only says that the integrated server started, so the world folder
//! is the one whose `session.lock` was touched most recently.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 25565;

/// Identifies the world a waypoint file belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorldId {
    /// A world folder under `saves/`.
    Singleplayer {
        folder: String,
    },
    Multiplayer {
        host: String,
        port: u16,
    },
}

impl WorldId {
    /// A file name stem that is safe on Windows and Linux: `sp-<folder>` or
    /// `mp-<host>-<port>`, with the host lowercased.
    pub fn file_stem(&self) -> String {
        match self {
            WorldId::Singleplayer { folder } => format!("sp-{}", sanitize_file_component(folder)),
            WorldId::Multiplayer { host, port } => {
                format!(
                    "mp-{}-{port}",
                    sanitize_file_component(&host.to_lowercase())
                )
            }
        }
    }

    /// A name for the UI: the folder name, or `host[:port]` (IPv6 in brackets).
    pub fn display_name(&self) -> String {
        match self {
            WorldId::Singleplayer { folder } => folder.clone(),
            WorldId::Multiplayer { host, port } => {
                let host = if host.contains(':') {
                    format!("[{host}]")
                } else {
                    host.clone()
                };
                if *port == DEFAULT_PORT {
                    host
                } else {
                    format!("{host}:{port}")
                }
            }
        }
    }
}

const MAX_COMPONENT_LEN: usize = 80;

/// Makes `s` usable as a single file name component on Windows and Linux.
///
/// Replaces `< > : " / \ | ? *` and control characters with `_`, trims trailing dots and
/// spaces, prefixes reserved device names (`CON`, `com1.txt`, ...) with `_`, never returns an
/// empty string and caps the
/// result at 80 bytes (on a char boundary). If anything had to change, `-` and 8 hex digits of
/// a stable hash of `s` are appended so that different inputs stay distinct.
pub fn sanitize_file_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    out.truncate(out.trim_end_matches(['.', ' ']).len());
    if is_reserved_name(&out) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push('_');
    }
    if out == s && out.len() <= MAX_COMPONENT_LEN {
        return out;
    }
    let suffix = format!("-{:08x}", fold_to_u32(fnv1a64(s.as_bytes())));
    truncate_on_char_boundary(&mut out, MAX_COMPONENT_LEN - suffix.len());
    out.push_str(&suffix);
    out
}

/// Whether Windows treats `name` as a device (`CON`, `NUL.txt`, `com1`, `LPT\u{b9}`, ...).
/// `COM0` and `LPT0` are included because Microsoft's naming rules list them as reserved.
fn is_reserved_name(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let base = base.to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let Some(number) = base
        .strip_prefix("COM")
        .or_else(|| base.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut chars = number.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

fn truncate_on_char_boundary(s: &mut String, max_len: usize) {
    if s.len() > max_len {
        let mut end = max_len;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

/// 64-bit FNV-1a. Stable across builds and platforms, unlike `DefaultHasher`.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(PRIME)
    })
}

fn fold_to_u32(hash: u64) -> u32 {
    (hash ^ (hash >> 32)) as u32
}

/// World-relevant events from `latest.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    ConnectingToServer { host: String, port: u16 },
    IntegratedServerStarting,
    IntegratedServerStopping,
}

/// `[time] [thread/LEVEL]` followed by exactly one of the vanilla `: `, Forge's
/// ` [logger/marker]: ` or Fabric's ` (category) `, then the message. Keeping the separators
/// strict stops a message that starts with a tag (`[CHAT] ...`) from passing as a logger name.
static LOG_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\[[^\]]+\] \[(?P<thread>[^\]]+)/(?P<level>[A-Z]+)\](?:: | \[[^\]]*\]: | \([^)]*\) )(?P<msg>.*)$",
    )
    .expect("valid regex")
});

/// Recognizes world-related lines of Minecraft's `latest.log`.
///
/// Understands the vanilla layout (`[12:34:56] [Render thread/INFO]: msg`), Forge/NeoForge
/// (`[29Sep2026 12:34:56.789] [Render thread/INFO] [logger/]: msg`) and Fabric
/// (`[12:34:56] [Render thread/INFO] (Minecraft) msg`). The event text must start the message,
/// so chat lines (`[CHAT] <bob> Connecting to ...`) never match.
pub fn parse_log_line(line: &str) -> Option<LogEvent> {
    let line = line.trim_start_matches('\u{feff}').trim_end();
    let caps = LOG_HEADER.captures(line)?;
    if &caps["level"] != "INFO" {
        return None;
    }
    let msg = caps.name("msg")?.as_str();
    if let Some(rest) = msg.strip_prefix("Connecting to ") {
        let (host, port) = rest.rsplit_once(", ")?;
        if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let port = port.parse().ok()?;
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return None;
        }
        return Some(LogEvent::ConnectingToServer {
            host: host.to_owned(),
            port,
        });
    }
    if msg.starts_with("Starting integrated minecraft server version") {
        return Some(LogEvent::IntegratedServerStarting);
    }
    if msg == "Stopping server" && &caps["thread"] == "Server thread" {
        return Some(LogEvent::IntegratedServerStopping);
    }
    None
}

const FINGERPRINT_LEN: usize = 256;
const MAX_READ_PER_POLL: u64 = 8 * 1024 * 1024;
const MAX_LINE_LEN: usize = 1024 * 1024;

/// Incrementally reads lines appended to a log file.
///
/// The file is opened and closed on every [`poll`](Self::poll) so that the game can rotate it
/// (on Windows an open handle could block that). Replacement, rotation and truncation are
/// detected by the file shrinking or by its first bytes changing, and reading restarts at 0.
#[derive(Debug)]
pub struct LogTail {
    path: PathBuf,
    offset: u64,
    /// The first `min(offset, FINGERPRINT_LEN)` bytes consumed so far.
    fingerprint: Vec<u8>,
    /// Bytes of an incomplete trailing line.
    partial: Vec<u8>,
    /// Inside a line longer than `MAX_LINE_LEN`; dropped up to the next newline.
    discarding: bool,
    restarts: u64,
    max_read: u64,
}

impl LogTail {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            fingerprint: Vec::new(),
            partial: Vec::new(),
            discarding: false,
            restarts: 0,
            max_read: MAX_READ_PER_POLL,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many times reading restarted because the file was replaced, truncated or removed.
    /// Callers can compare it between polls to reset state derived from the old file.
    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// Returns the complete lines appended since the last poll, without line terminators.
    /// A missing file yields no lines. At most 8 MiB are read per call.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.restart();
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len < self.offset || !self.fingerprint_matches(&mut file)? {
            self.restart();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut data = Vec::new();
        // Consumes and thereby closes the file.
        file.take(self.max_read).read_to_end(&mut data)?;

        if self.fingerprint.len() < FINGERPRINT_LEN {
            // The fingerprint is only short while it still ends at `offset`.
            let take = (FINGERPRINT_LEN - self.fingerprint.len()).min(data.len());
            self.fingerprint.extend_from_slice(&data[..take]);
        }
        self.offset += data.len() as u64;
        Ok(self.split_lines(&data))
    }

    fn fingerprint_matches(&self, file: &mut File) -> io::Result<bool> {
        if self.fingerprint.is_empty() {
            return Ok(true);
        }
        let mut head = vec![0; self.fingerprint.len()];
        file.seek(SeekFrom::Start(0))?;
        match file.read_exact(&mut head) {
            Ok(()) => Ok(head == self.fingerprint),
            // Shrank since the length check.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn restart(&mut self) {
        if self.offset > 0 || !self.partial.is_empty() {
            self.restarts += 1;
        }
        self.offset = 0;
        self.fingerprint.clear();
        self.partial.clear();
        self.discarding = false;
    }

    fn split_lines(&mut self, data: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut rest = data;
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            let segment = &rest[..pos];
            if !self.discarding && self.partial.len() + segment.len() <= MAX_LINE_LEN {
                self.partial.extend_from_slice(segment);
                let line = self.partial.strip_suffix(b"\r").unwrap_or(&self.partial);
                lines.push(String::from_utf8_lossy(line).into_owned());
            }
            self.partial.clear();
            self.discarding = false;
            rest = &rest[pos + 1..];
        }
        if !self.discarding {
            if self.partial.len() + rest.len() <= MAX_LINE_LEN {
                self.partial.extend_from_slice(rest);
            } else {
                self.partial.clear();
                self.discarding = true;
            }
        }
        lines
    }
}

/// Follows [`LogEvent`]s to know which world the player is in.
#[derive(Debug, Clone, Default)]
pub struct WorldTracker {
    last: Option<LogEvent>,
}

impl WorldTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_log_event(&mut self, ev: &LogEvent) {
        self.last = Some(ev.clone());
    }

    /// Parses `line` and applies the event, if any, returning it.
    pub fn on_log_line(&mut self, line: &str) -> Option<LogEvent> {
        let ev = parse_log_line(line)?;
        self.on_log_event(&ev);
        Some(ev)
    }

    /// Forgets all events, e.g. after the log file was replaced.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// The current world according to the last event: a server after `ConnectingToServer`,
    /// the most recently locked world folder after `IntegratedServerStarting`, and `None`
    /// after `IntegratedServerStopping` or before any event.
    pub fn resolve(&self, saves_dir: &Path) -> Option<WorldId> {
        match self.last.as_ref()? {
            LogEvent::ConnectingToServer { host, port } => Some(WorldId::Multiplayer {
                host: host.to_lowercase(),
                port: *port,
            }),
            LogEvent::IntegratedServerStarting => match detect_singleplayer_world(saves_dir) {
                Ok(folder) => folder.map(|folder| WorldId::Singleplayer { folder }),
                Err(e) => {
                    log::warn!("cannot scan {}: {e}", saves_dir.display());
                    None
                }
            },
            LogEvent::IntegratedServerStopping => None,
        }
    }
}

/// Returns the world folder directly under `saves_dir` whose `session.lock` was modified most
/// recently (the game writes it when a world is loaded), or `None` if there is none.
/// Non-directories and entries that cannot be read are skipped.
pub fn detect_singleplayer_world(saves_dir: &Path) -> io::Result<Option<String>> {
    let entries = match fs::read_dir(saves_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut newest: Option<(SystemTime, String)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        // Follows symlinks, so linked world folders count.
        if !fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Ok(modified) = fs::metadata(path.join("session.lock")).and_then(|m| m.modified())
        else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        // Ties are broken by name so the result does not depend on directory order.
        if newest
            .as_ref()
            .is_none_or(|(t, n)| (modified, &name) > (*t, n))
        {
            newest = Some((modified, name));
        }
    }
    Ok(newest.map(|(_, name)| name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::time::{Duration, UNIX_EPOCH};

    fn sp(folder: &str) -> WorldId {
        WorldId::Singleplayer {
            folder: folder.into(),
        }
    }

    fn mp(host: &str, port: u16) -> WorldId {
        WorldId::Multiplayer {
            host: host.into(),
            port,
        }
    }

    #[test]
    fn fnv1a64_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn sanitize_keeps_safe_names() {
        for name in [
            "New World",
            "example.com",
            "My World (2)",
            "新しいワールド",
            "a.b-c_d",
            "COM10",
            "CONSOLE",
            ".hidden",
        ] {
            assert_eq!(sanitize_file_component(name), name);
        }
        let exactly_80 = "x".repeat(80);
        assert_eq!(sanitize_file_component(&exactly_80), exactly_80);
    }

    #[test]
    fn sanitize_replaces_forbidden_characters() {
        assert_eq!(sanitize_file_component("a<b"), "a_b-e2c22faf");
        assert_eq!(sanitize_file_component("a/b"), "a_b-e2480c78");
        assert_eq!(sanitize_file_component("a\\b"), "a_b-e2530c8f");
        assert_eq!(sanitize_file_component("a:b"), "a_b-e2c18079");
        assert_eq!(sanitize_file_component("x>\"|?*y"), "x_____y-1ce0ff51");
        assert_eq!(sanitize_file_component("tab\there\n"), "tab_here_-ce7e7c02");
        assert_eq!(sanitize_file_component("del\u{7f}"), "del_-e79589e8");
    }

    #[test]
    fn sanitize_trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_file_component("World."), "World-926bc6f6");
        assert_eq!(sanitize_file_component("World . . "), "World-13abfd42");
        assert_eq!(sanitize_file_component(" World"), " World");
        assert_eq!(sanitize_file_component("..."), "_-1b922c0e");
        assert_eq!(sanitize_file_component(".."), "_-b37a252a");
        assert_eq!(sanitize_file_component(""), "_-4fd0bfc1");
        assert_eq!(sanitize_file_component("   "), "_-0d026cc0");
    }

    #[test]
    fn sanitize_avoids_windows_reserved_names() {
        assert_eq!(sanitize_file_component("CON"), "_CON-a1fd9952");
        assert_eq!(sanitize_file_component("con"), "_con-fb059532");
        assert_eq!(sanitize_file_component("con.txt"), "_con.txt-0c631f8b");
        assert_eq!(
            sanitize_file_component("Nul.tar.gz"),
            "_Nul.tar.gz-96dc5ec5"
        );
        for name in [
            "PRN",
            "aux",
            "NUL",
            "COM1",
            "com9",
            "LPT1",
            "lpt9",
            "COM\u{b9}",
            "lpt\u{b3}",
            "CONIN$",
            "conout$",
            "AUX.json",
            "NUL .txt",
            "CON.",
            // Listed as reserved in Microsoft's file naming documentation.
            "COM0",
            "lpt0.log",
        ] {
            let out = sanitize_file_component(name);
            assert!(out.starts_with('_'), "{name} -> {out}");
            assert!(!is_reserved_name(&out), "{name} -> {out}");
        }
        assert!(!is_reserved_name("COM"));
        assert!(!is_reserved_name("COM00"));
        assert!(!is_reserved_name("LPT"));
        assert!(!is_reserved_name("icon"));
    }

    #[test]
    fn sanitize_caps_length_on_char_boundary() {
        let long = "a".repeat(200);
        let out = sanitize_file_component(&long);
        assert_eq!(out.len(), 80);
        assert_eq!(out, format!("{}-d95e07ec", "a".repeat(71)));
        // Multi-byte characters are never split.
        let wide = "あ".repeat(40);
        let out = sanitize_file_component(&wide);
        assert!(out.len() <= 80, "{}", out.len());
        assert_eq!(out, format!("{}-7d56f0a5", "あ".repeat(23)));
        // 81 bytes is one too many.
        let out = sanitize_file_component(&"b".repeat(81));
        assert_eq!(out.len(), 80);
        assert!(out.ends_with("-f7713920"), "{out}");
    }

    #[test]
    fn sanitize_distinguishes_inputs_that_map_to_the_same_text() {
        let variants = ["a:b", "a/b", "a\\b", "a<b", "a_b", "a?b"];
        let outputs: HashSet<String> = variants
            .iter()
            .map(|v| sanitize_file_component(v))
            .collect();
        assert_eq!(outputs.len(), variants.len(), "{outputs:?}");
        let long_a = format!("{}1", "x".repeat(100));
        let long_b = format!("{}2", "x".repeat(100));
        assert_ne!(
            sanitize_file_component(&long_a),
            sanitize_file_component(&long_b)
        );
    }

    #[test]
    fn file_stems() {
        assert_eq!(sp("New World").file_stem(), "sp-New World");
        assert_eq!(sp("CON").file_stem(), "sp-_CON-a1fd9952");
        assert_eq!(sp("a/b").file_stem(), "sp-a_b-e2480c78");
        assert_eq!(mp("example.com", 25565).file_stem(), "mp-example.com-25565");
        assert_eq!(mp("Example.COM", 25565).file_stem(), "mp-example.com-25565");
        assert_eq!(mp("192.168.1.5", 54321).file_stem(), "mp-192.168.1.5-54321");
        assert_eq!(
            mp("2001:db8::1", 25565).file_stem(),
            "mp-2001_db8__1-eb334f17-25565"
        );
        assert_eq!(mp("::1", 25566).file_stem(), "mp-__1-eb56b0b4-25566");
        // Folder names keep their case.
        assert_ne!(sp("World").file_stem(), sp("world").file_stem());
    }

    #[test]
    fn display_names() {
        assert_eq!(sp("New World").display_name(), "New World");
        assert_eq!(mp("example.com", 25565).display_name(), "example.com");
        assert_eq!(mp("example.com", 25566).display_name(), "example.com:25566");
        assert_eq!(mp("::1", 25565).display_name(), "[::1]");
        assert_eq!(mp("2001:db8::1", 1234).display_name(), "[2001:db8::1]:1234");
    }

    #[test]
    fn world_id_serde() {
        let id = mp("example.com", 25565);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<WorldId>(&json).unwrap(), id);
    }

    fn connecting(host: &str, port: u16) -> Option<LogEvent> {
        Some(LogEvent::ConnectingToServer {
            host: host.into(),
            port,
        })
    }

    #[test]
    fn parses_connecting_lines() {
        let cases = [
            (
                "[12:34:56] [Render thread/INFO]: Connecting to example.com, 25565",
                "example.com",
                25565,
            ),
            (
                "[12:34:56] [Server Connector #1/INFO]: Connecting to mc.example.org, 25566\r\n",
                "mc.example.org",
                25566,
            ),
            (
                "[29Sep2026 12:34:56.789] [Render thread/INFO] [net.minecraft.client.gui.screens.ConnectScreen/]: Connecting to example.com, 25565",
                "example.com",
                25565,
            ),
            (
                "[29Sep2026 12:34:56.789] [Server Connector #2/INFO] [net.minecraft.client.gui.screen.ConnectingScreen/]: Connecting to 10.0.0.1, 1",
                "10.0.0.1",
                1,
            ),
            (
                "[12:34:56] [Render thread/INFO] (Minecraft) Connecting to play.example.net, 25565",
                "play.example.net",
                25565,
            ),
            (
                "[12:34:56] [Render thread/INFO]: Connecting to 192.168.0.10, 65535  ",
                "192.168.0.10",
                65535,
            ),
            (
                "[12:34:56] [Render thread/INFO]: Connecting to 2001:db8::1, 25565",
                "2001:db8::1",
                25565,
            ),
            (
                "[12:34:56] [Render thread/INFO]: Connecting to [::1], 25565",
                "::1",
                25565,
            ),
            (
                "[12:34:56] [Render thread/INFO]: Connecting to Example.COM, 25565",
                "Example.COM",
                25565,
            ),
        ];
        for (line, host, port) in cases {
            assert_eq!(parse_log_line(line), connecting(host, port), "{line}");
        }
    }

    #[test]
    fn parses_integrated_server_lines() {
        for line in [
            "[12:34:56] [Server thread/INFO]: Starting integrated minecraft server version 1.20.1",
            "[12:34:56] [Server thread/INFO]: Starting integrated minecraft server version 1.13.2\r",
            "[29Sep2026 12:34:56.789] [Server thread/INFO] [net.minecraft.client.server.IntegratedServer/]: Starting integrated minecraft server version 1.21.1",
            "[12:34:56] [Server thread/INFO] (Minecraft) Starting integrated minecraft server version 1.20.4",
        ] {
            assert_eq!(
                parse_log_line(line),
                Some(LogEvent::IntegratedServerStarting),
                "{line}"
            );
        }
        for line in [
            "[12:34:56] [Server thread/INFO]: Stopping server",
            "[29Sep2026 12:34:56.789] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: Stopping server\r\n",
            "[12:34:56] [Server thread/INFO] (Minecraft) Stopping server",
        ] {
            assert_eq!(
                parse_log_line(line),
                Some(LogEvent::IntegratedServerStopping),
                "{line}"
            );
        }
    }

    #[test]
    fn ignores_unrelated_and_spoofed_lines() {
        for line in [
            "",
            "Connecting to example.com, 25565",
            "[12:34:56] [Render thread/INFO]: [System] [CHAT] <bob> Connecting to evil.com, 1",
            "[12:34:56] [Render thread/INFO]: [CHAT] Connecting to evil.com, 1",
            "[29Sep2026 12:34:56.789] [Render thread/INFO] [net.minecraft.client.gui.components.ChatComponent/]: [CHAT] <bob> Connecting to evil.com, 1",
            "[12:34:56] [Render thread/INFO]: [CHAT] <bob> Stopping server",
            "[12:34:56] [Render thread/INFO]: [CHAT] Starting integrated minecraft server version 1",
            "[12:34:56] [Render thread/INFO]: Stopping server",
            "[12:34:56] [Server thread/WARN]: Stopping server",
            "[12:34:56] [Render thread/WARN]: Connecting to example.com, 25565",
            "[12:34:56] [Server thread/INFO]: Stopping server now",
            "[12:34:56] [Render thread/INFO]: Connecting to example.com",
            "[12:34:56] [Render thread/INFO]: Connecting to example.com, 65536",
            "[12:34:56] [Render thread/INFO]: Connecting to example.com, 123456",
            "[12:34:56] [Render thread/INFO]: Connecting to example.com, -1",
            "[12:34:56] [Render thread/INFO]: Connecting to example.com, 25565x",
            "[12:34:56] [Render thread/INFO]: Connecting to , 25565",
            "[12:34:56] [Render thread/INFO]: Connecting to two words, 25565",
            "[12:34:56] [Render thread/INFO]: Connecting to example.com,25565",
            "[12:34:56] [Render thread/INFO]: Setting user: Steve",
            "[12:34:56] [Render thread/INFO]:  Connecting to example.com, 25565",
            "\tat net.minecraft.client.Minecraft.run(Minecraft.java:1)",
            // No layout puts a bracketed tag after the level without a colon, so a chat tag
            // there must not be taken for Forge's logger name.
            "[12:34:56] [Render thread/INFO] [CHAT] Connecting to evil.com, 1",
            "[12:34:56] [Render thread/INFO] [CHAT] Stopping server",
            "[12:34:56] [Server thread/INFO] [CHAT] Stopping server",
            "[12:34:56] [Render thread/INFO] Connecting to evil.com, 1",
            "[12:34:56] [Render thread/INFO](Minecraft) Connecting to evil.com, 1",
        ] {
            assert_eq!(parse_log_line(line), None, "{line:?}");
        }
    }

    fn append(path: &Path, data: &[u8]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(data).unwrap();
    }

    #[test]
    fn tail_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        assert_eq!(tail.path(), path);
        assert!(tail.poll().unwrap().is_empty());
        assert!(tail.poll().unwrap().is_empty());
        append(&path, b"first\n");
        assert_eq!(tail.poll().unwrap(), ["first"]);
        assert_eq!(tail.restarts(), 0);
    }

    #[test]
    fn tail_growth_and_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        append(&path, b"one\r\ntwo\nthr");
        assert_eq!(tail.poll().unwrap(), ["one", "two"]);
        assert!(tail.poll().unwrap().is_empty());
        append(&path, b"ee");
        assert!(tail.poll().unwrap().is_empty());
        append(&path, b"\r");
        assert!(tail.poll().unwrap().is_empty());
        append(&path, b"\n\nfour\n");
        assert_eq!(tail.poll().unwrap(), ["three", "", "four"]);
        assert!(tail.poll().unwrap().is_empty());
        assert_eq!(tail.restarts(), 0);
    }

    #[test]
    fn tail_decodes_utf8_split_across_polls_and_invalid_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        let text = "ワールド\n".as_bytes();
        append(&path, &text[..4]);
        assert!(tail.poll().unwrap().is_empty());
        append(&path, &text[4..]);
        assert_eq!(tail.poll().unwrap(), ["ワールド"]);
        append(&path, b"bad \xff byte\n");
        assert_eq!(tail.poll().unwrap(), ["bad \u{fffd} byte"]);
    }

    #[test]
    fn tail_detects_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        fs::write(&path, "old line one\nold line two\npartial").unwrap();
        assert_eq!(tail.poll().unwrap(), ["old line one", "old line two"]);
        fs::write(&path, "new\n").unwrap();
        assert_eq!(tail.poll().unwrap(), ["new"]);
        assert_eq!(tail.restarts(), 1);
    }

    #[test]
    fn tail_detects_replacement_with_a_longer_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        fs::write(&path, "[10:00:00] [main/INFO]: old session\n").unwrap();
        assert_eq!(
            tail.poll().unwrap(),
            ["[10:00:00] [main/INFO]: old session"]
        );
        // Rotate the way log4j does: move the old file away and start a new one.
        fs::rename(&path, dir.path().join("2026-09-29-1.log")).unwrap();
        fs::write(
            &path,
            "[11:00:00] [main/INFO]: new session\n[11:00:01] [main/INFO]: second line\n",
        )
        .unwrap();
        assert_eq!(
            tail.poll().unwrap(),
            [
                "[11:00:00] [main/INFO]: new session",
                "[11:00:01] [main/INFO]: second line"
            ]
        );
        assert_eq!(tail.restarts(), 1);
        append(&path, b"third\n");
        assert_eq!(tail.poll().unwrap(), ["third"]);
        assert_eq!(tail.restarts(), 1);
    }

    #[test]
    fn tail_drops_old_partial_line_on_rotation_to_a_longer_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        fs::write(
            &path,
            "[10:00:00] [main/INFO]: old\n[10:00:01] [Render thread/INFO]: Conn",
        )
        .unwrap();
        assert_eq!(tail.poll().unwrap(), ["[10:00:00] [main/INFO]: old"]);
        // The new session is already longer than everything read from the old one.
        let new = format!("[11:00:00] [main/INFO]: {}\nsecond\n", "n".repeat(100));
        fs::write(&path, &new).unwrap();
        let lines = tail.poll().unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("[11:00:00]"), "{lines:?}");
        assert_eq!(tail.restarts(), 1);
    }

    #[test]
    fn tail_lone_cr_does_not_end_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        // Only `\n` (optionally after `\r`) ends a line; text smuggled after a bare `\r` stays
        // inside the line it was logged in, so it cannot pose as a line of its own.
        append(
            &path,
            b"[10:00:00] [Render thread/INFO]: x\r[10:00:00] [Render thread/INFO]: Connecting to evil.com, 1\r",
        );
        assert!(tail.poll().unwrap().is_empty());
        append(&path, b"\n");
        let lines = tail.poll().unwrap();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(parse_log_line(&lines[0]), None);
        // CR-only content is buffered like any unfinished line and stays bounded.
        tail.max_read = 64 * 1024;
        append(&path, "a\r".repeat(MAX_LINE_LEN).as_bytes());
        while tail.offset < fs::metadata(&path).unwrap().len() {
            assert!(tail.poll().unwrap().is_empty());
            assert!(tail.partial.len() <= MAX_LINE_LEN);
        }
        append(&path, b"\nnext\n");
        assert_eq!(tail.poll().unwrap(), ["next"]);
    }

    #[test]
    fn tail_utf8_split_at_the_read_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        tail.max_read = 5;
        // Every 3-byte character straddles some 5-byte read boundary.
        append(&path, "ワールドの名前\r\n".as_bytes());
        let mut lines = Vec::new();
        for _ in 0..10 {
            lines.extend(tail.poll().unwrap());
        }
        assert_eq!(lines, ["ワールドの名前"]);
    }

    #[test]
    fn tail_fingerprint_grows_with_short_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        fs::write(&path, "a\n").unwrap();
        assert_eq!(tail.poll().unwrap(), ["a"]);
        // The first bytes are unchanged, so this is growth, not replacement.
        let long_line = "x".repeat(600);
        append(&path, format!("{long_line}\n").as_bytes());
        assert_eq!(tail.poll().unwrap(), [long_line.as_str()]);
        assert_eq!(tail.fingerprint.len(), FINGERPRINT_LEN);
        append(&path, b"b\n");
        assert_eq!(tail.poll().unwrap(), ["b"]);
        // Replacing with same-length-or-longer content that differs early is noticed.
        let mut replaced = fs::read(&path).unwrap();
        replaced[0] = b'Z';
        replaced.extend_from_slice(b"c\n");
        fs::write(&path, &replaced).unwrap();
        let lines = tail.poll().unwrap();
        assert_eq!(lines.first().map(String::as_str), Some("Z"));
        assert_eq!(lines.len(), 4);
        assert_eq!(tail.restarts(), 1);
    }

    #[test]
    fn tail_removed_then_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(tail.poll().unwrap().len(), 2);
        fs::remove_file(&path).unwrap();
        assert!(tail.poll().unwrap().is_empty());
        assert_eq!(tail.restarts(), 1);
        assert!(tail.poll().unwrap().is_empty());
        assert_eq!(tail.restarts(), 1);
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        assert_eq!(tail.poll().unwrap(), ["one", "two", "three"]);
    }

    #[test]
    fn tail_reads_in_bounded_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        tail.max_read = 10;
        append(&path, b"line-0001\nline-0002\nline-0003\n");
        let mut lines = Vec::new();
        for _ in 0..4 {
            lines.extend(tail.poll().unwrap());
        }
        assert_eq!(lines, ["line-0001", "line-0002", "line-0003"]);
        assert_eq!(tail.restarts(), 0);
    }

    #[test]
    fn tail_drops_overlong_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        let huge = vec![b'x'; MAX_LINE_LEN + 1];
        append(&path, b"before\n");
        append(&path, &huge[..MAX_LINE_LEN / 2]);
        assert_eq!(tail.poll().unwrap(), ["before"]);
        append(&path, &huge[MAX_LINE_LEN / 2..]);
        assert!(tail.poll().unwrap().is_empty());
        append(&path, b"xxxx\nafter\n");
        assert_eq!(tail.poll().unwrap(), ["after"]);
        // A complete overlong line within one read is dropped too.
        append(&path, &huge);
        append(&path, b"\nlast\n");
        assert_eq!(tail.poll().unwrap(), ["last"]);
    }

    #[test]
    fn tail_does_not_keep_the_file_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        let mut tail = LogTail::new(&path);
        fs::write(&path, "a\n").unwrap();
        tail.poll().unwrap();
        // Renaming and deleting would fail on Windows while a handle without sharing is open.
        fs::rename(&path, dir.path().join("old.log")).unwrap();
        fs::remove_file(dir.path().join("old.log")).unwrap();
    }

    fn set_lock_time(world: &Path, secs: u64) {
        fs::create_dir_all(world).unwrap();
        let lock = File::create(world.join("session.lock")).unwrap();
        lock.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn detects_most_recently_locked_world() {
        let dir = tempfile::tempdir().unwrap();
        let saves = dir.path().join("saves");
        set_lock_time(&saves.join("Old World"), 1_600_000_000);
        set_lock_time(&saves.join("Current"), 1_700_000_000);
        set_lock_time(&saves.join("Older"), 1_500_000_000);
        fs::create_dir_all(saves.join("No lock")).unwrap();
        // A newer plain file named like a world must be ignored.
        let stray = File::create(saves.join("session.lock")).unwrap();
        stray
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_800_000_000))
            .unwrap();
        assert_eq!(
            detect_singleplayer_world(&saves).unwrap().as_deref(),
            Some("Current")
        );

        set_lock_time(&saves.join("Old World"), 1_750_000_000);
        assert_eq!(
            detect_singleplayer_world(&saves).unwrap().as_deref(),
            Some("Old World")
        );
    }

    #[test]
    fn detect_without_worlds() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_singleplayer_world(&dir.path().join("missing")).unwrap(),
            None
        );
        assert_eq!(detect_singleplayer_world(dir.path()).unwrap(), None);
        fs::create_dir(dir.path().join("empty world")).unwrap();
        assert_eq!(detect_singleplayer_world(dir.path()).unwrap(), None);
    }

    #[test]
    fn detect_breaks_ties_by_name() {
        let dir = tempfile::tempdir().unwrap();
        set_lock_time(&dir.path().join("b"), 1_700_000_000);
        set_lock_time(&dir.path().join("a"), 1_700_000_000);
        set_lock_time(&dir.path().join("c"), 1_700_000_000);
        assert_eq!(
            detect_singleplayer_world(dir.path()).unwrap().as_deref(),
            Some("c")
        );
    }

    #[test]
    fn tracker_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let saves = dir.path().join("saves");
        set_lock_time(&saves.join("Survival"), 1_700_000_000);
        let mut tracker = WorldTracker::new();
        assert_eq!(tracker.resolve(&saves), None);

        tracker.on_log_event(&LogEvent::IntegratedServerStarting);
        assert_eq!(tracker.resolve(&saves), Some(sp("Survival")));
        tracker.on_log_event(&LogEvent::IntegratedServerStopping);
        assert_eq!(tracker.resolve(&saves), None);

        tracker.on_log_event(&LogEvent::ConnectingToServer {
            host: "Play.Example.com".into(),
            port: 25565,
        });
        assert_eq!(tracker.resolve(&saves), Some(mp("play.example.com", 25565)));

        // Joining a singleplayer world after a server switches back.
        tracker.on_log_event(&LogEvent::IntegratedServerStarting);
        assert_eq!(tracker.resolve(&saves), Some(sp("Survival")));
        // Resolution follows the lock file at the time of the call.
        set_lock_time(&saves.join("Creative"), 1_800_000_000);
        assert_eq!(tracker.resolve(&saves), Some(sp("Creative")));

        tracker.on_log_event(&LogEvent::ConnectingToServer {
            host: "10.0.0.2".into(),
            port: 25570,
        });
        assert_eq!(tracker.resolve(&saves), Some(mp("10.0.0.2", 25570)));
        tracker.reset();
        assert_eq!(tracker.resolve(&saves), None);
    }

    #[test]
    fn tracker_singleplayer_without_saves() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = WorldTracker::new();
        tracker.on_log_event(&LogEvent::IntegratedServerStarting);
        assert_eq!(tracker.resolve(&dir.path().join("saves")), None);
    }

    #[test]
    fn tracker_from_log_lines() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = WorldTracker::new();
        assert_eq!(
            tracker.on_log_line("[12:00:00] [Render thread/INFO]: Setting user: Steve"),
            None
        );
        assert_eq!(
            tracker
                .on_log_line("[12:00:01] [Render thread/INFO]: Connecting to example.com, 25565"),
            connecting("example.com", 25565)
        );
        assert_eq!(
            tracker.on_log_line("[12:00:02] [Render thread/INFO]: [CHAT] Stopping server"),
            None
        );
        assert_eq!(tracker.resolve(dir.path()), Some(mp("example.com", 25565)));
    }
}
