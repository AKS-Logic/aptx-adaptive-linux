//! Host bridge to the vendor aptX Adaptive encoder running under QEMU.
//!
//! The vendor Hexagon module cannot run natively on x86, so this module
//! drives the reference helper (audio/aptx-adaptive/build/aptx-lossless-helper)
//! through qemu-hexagon.  It is the Rust equivalent of the research harness
//! audio/aptx-adaptive/harness/drive_r2.py.
//!
//! # Wire protocol (PR #12, protocol v2)
//!
//! The helper speaks a tiny binary protocol on stdin/stdout.
//!
//! * On startup it writes u32le status, u32le payload_size (both zero for a
//!   healthy helper).
//! * A control request is u32le 0xffffffff, u32le command, u32le length,
//!   followed by length bytes.  The helper answers with the same 8-byte
//!   reply header.
//! * An audio request is u32le byte_length, followed by interleaved stereo
//!   S32 little-endian PCM.  The reply is u32le status, u32le packet_size
//!   and, when status == 0, one OTA packet.  Status 11 (EAGAIN) means the
//!   helper is still buffering a frame.
//!
//! The default HelperConfig is the Bose QC Ultra (1st gen) configuration
//! captured from a working link: 48 kHz in/out, mode 2 (R2), profile 6,
//! MTU 995, 32-bit PCM, the 40-byte Bose CIE and stream descriptor
//! 1, 23, 0, 0, 15, 2, 3, 3, 3, 0, 170.  In that configuration every
//! successful reply is a 664-byte R2 OTA packet (version byte 0xae).
//!
//! # Typical use
//!
//! Use HelperBridge::spawn(HelperConfig::bose()), feed 256-frame chunks with
//! encode_interleaved_s32, and forward each returned packet.  The bridge is
//! the emulation path until the native encoder carries the transport.
//!
//! SPDX-License-Identifier: MIT

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Control-word prefix that starts a helper control request.
pub const CONTROL_WORD: u32 = 0xFFFF_FFFF;

/// Control command: (re)configure the encoder.
pub const CMD_CONFIG: u32 = 1;

/// Control command: set the IMCL bit-rate / quality level.
pub const CMD_SET_QUALITY_LEVEL: u32 = 3;

/// Protocol version sent in the configuration payload.
pub const PROTOCOL_VERSION: u32 = 2;

/// Size of the connection-info element (CIE) in the configuration payload.
pub const CIE_LEN: usize = 40;

/// Size of the R2 stream descriptor in the configuration payload.
pub const R2_STREAM_LEN: usize = 11;

/// Reply status meaning "input accepted, no packet ready yet".
pub const STATUS_EAGAIN: u32 = 11;

/// Reply status meaning success.
pub const STATUS_OK: u32 = 0;

/// Largest PCM chunk the helper accepts (PCM_BYTES_MAX in the helper).
pub const PCM_BYTES_MAX: usize = 64 * 1024;

/// Largest reply payload the helper can produce (MAX_PACKET_SIZE).
pub const MAX_REPLY_BYTES: usize = 4096;

/// Length of one R2 OTA packet produced by the default configuration.
pub const R2_OTA_LEN: usize = 664;

/// OTA version byte of an R2 packet (0xae).
pub const R2_OTA_TYPE: u8 = 0xae;

/// The 40-byte Bose QC Ultra connection-info element.
///
/// The meaningful prefix comes from a live Bose capture; the helper expects
/// a full 40-byte CIE, so the remaining bytes are zero padding.
pub const BOSE_CIE: [u8; CIE_LEN] = [
    0xd7, 0x00, 0x00, 0x00, 0xad, 0x00, 0x51, 0x0a, 0x37, 0x6a, 0xc8, 0x7d, 0x64, 0x7d, 0x00, 0x01,
    0x87, 0x00, 0x00, 0x00, 0x0f, 0x02, 0x03, 0x03, 0x03, 0x00, 0x00, 0xaa, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// The Bose CIE as the hex string used by the research harness.
pub const BOSE_CIE_HEX: &str = "d7000000ad00510a376ac87d647d0001870000000f020303030000aa";

/// The R2 stream descriptor captured from the Bose link.
pub const BOSE_R2_STREAM: [u8; R2_STREAM_LEN] = [1, 23, 0, 0, 15, 2, 3, 3, 3, 0, 170];

/// Encoder configuration sent to the helper.
///
/// The fields mirror build_config() in drive_r2.py; use HelperConfig::bose
/// (also the Default impl) for the working QC Ultra configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperConfig {
    /// Input sample rate in Hz (the plugin's rate; 48 000 here).
    pub source_rate: u32,
    /// Encoder sample rate in Hz (normally equal to source_rate).
    pub encoder_rate: u32,
    /// Codec mode: 0 = auto, 2 = R2, 3 = R3.
    pub mode: u32,
    /// Adaptive profile byte (6 for the Bose link).
    pub profile: u32,
    /// L2CAP MTU advertised to the encoder.
    pub mtu: u32,
    /// Whether adaptive bit-rate feedback is enabled (0/1).
    pub abr: u32,
    /// PCM bits per sample (32 for S32 chunks).
    pub bits: u32,
    /// Lossless mode selector (0 = off).
    pub lossless: u32,
    /// QHS link support flag (0 = no).
    pub qhs: u32,
    /// Connection-info element, exactly CIE_LEN bytes.
    pub cie: [u8; CIE_LEN],
    /// R2 stream descriptor, exactly R2_STREAM_LEN bytes.
    pub r2_stream: [u8; R2_STREAM_LEN],
}

impl HelperConfig {
    /// The Bose QC Ultra (1st gen) 48 kHz configuration.
    ///
    /// This is the configuration that produces Bose-valid frames from the
    /// vendor helper: mode 2 (R2), profile 6, MTU 995, 32-bit PCM, the Bose
    /// CIE and the Bose R2 stream descriptor.
    pub fn bose() -> Self {
        Self {
            source_rate: 48_000,
            encoder_rate: 48_000,
            mode: 2,
            profile: 6,
            mtu: 995,
            abr: 0,
            bits: 32,
            lossless: 0,
            qhs: 0,
            cie: BOSE_CIE,
            r2_stream: BOSE_R2_STREAM,
        }
    }

    /// Serialize the configuration into the helper's wire payload.
    ///
    /// The layout is ten little-endian u32s (protocol, source rate, encoder
    /// rate, mode, profile, MTU, ABR, bits, lossless, QHS), the CIE length,
    /// the CIE bytes and the stream descriptor bytes.
    pub fn to_wire_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(44 + CIE_LEN + R2_STREAM_LEN);
        for value in [
            PROTOCOL_VERSION,
            self.source_rate,
            self.encoder_rate,
            self.mode,
            self.profile,
            self.mtu,
            self.abr,
            self.bits,
            self.lossless,
            self.qhs,
            CIE_LEN as u32,
        ] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.extend_from_slice(&self.cie);
        payload.extend_from_slice(&self.r2_stream);
        payload
    }
}

impl Default for HelperConfig {
    fn default() -> Self {
        Self::bose()
    }
}

/// Resolve a 40-byte CIE from a possibly abbreviated hex string.
///
/// Whitespace, colon, dash, comma and underscore separators are ignored, as
/// is a 0x prefix.  A string shorter than 40 bytes is right-padded with
/// zeros, which is how the captured 28-byte Bose prefix is used; longer
/// strings are an error.
pub fn cie_from_hex(hex: &str) -> Result<[u8; CIE_LEN]> {
    let mut nibbles = Vec::with_capacity(hex.len());
    let mut chars = hex.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '0' && matches!(chars.peek(), Some('x') | Some('X')) {
            chars.next();
            continue;
        }
        if ch.is_ascii_whitespace() || matches!(ch, ':' | '-' | ',' | '_') {
            continue;
        }
        let value = ch
            .to_digit(16)
            .ok_or_else(|| invalid_arg(format!("invalid hex digit {ch:?} in CIE")))?;
        nibbles.push(value as u8);
    }
    if nibbles.len() % 2 != 0 {
        return Err(invalid_arg("CIE hex string has an odd number of digits"));
    }
    let byte_len = nibbles.len() / 2;
    if byte_len > CIE_LEN {
        return Err(invalid_arg(format!(
            "CIE hex string is {byte_len} bytes, at most {CIE_LEN} allowed"
        )));
    }
    let mut cie = [0u8; CIE_LEN];
    for (i, pair) in nibbles.chunks_exact(2).enumerate() {
        cie[i] = (pair[0] << 4) | pair[1];
    }
    Ok(cie)
}

/// Paths and QEMU arguments needed to start the helper.
///
/// HelperPaths::discover finds the checkout automatically; every field can
/// be overridden with an environment variable (APTX_HELPER, APTX_QEMU,
/// APTX_QEMU_CPU, APTX_HEXAGON_SYSROOT, APTX_ADAPTIVE_ROOT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperPaths {
    /// QEMU binary (default qemu-hexagon from PATH).
    pub qemu: PathBuf,
    /// CPU model passed to -cpu (default v73).
    pub cpu: String,
    /// Hexagon musl sysroot passed to -L.
    pub sysroot: PathBuf,
    /// The vendored helper binary.
    pub helper: PathBuf,
}

impl HelperPaths {
    /// Discover the helper, sysroot and QEMU binary for this checkout.
    ///
    /// The checkout root is taken from APTX_ADAPTIVE_ROOT (authoritative when
    /// set) or found by walking up from the current directory / executable,
    /// looking for audio/aptx-adaptive with a populated build/ and toolchain/.
    /// The QEMU binary is resolved against PATH and stored as an absolute
    /// path, so the command that is logged is exactly the command that runs.
    pub fn discover() -> Result<Self> {
        let root = Self::find_root()?;

        let helper = env_path("APTX_HELPER")
            .unwrap_or_else(|| root.join("build").join("aptx-lossless-helper"));
        Self::validate_helper(&helper, &root)?;

        let sysroot = match env_path("APTX_HEXAGON_SYSROOT") {
            Some(path) => path,
            None => Self::find_sysroot(&root)?,
        };
        Self::validate_sysroot(&sysroot)?;

        let qemu = Self::resolve_qemu()?;
        let cpu = std::env::var("APTX_QEMU_CPU")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "v73".to_string());

        let paths = Self {
            qemu,
            cpu,
            sysroot,
            helper,
        };
        paths.log_resolved();
        Ok(paths)
    }

    /// Build the Command used to launch the helper (without pipes).
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.qemu);
        command.args(self.arguments());
        command
    }

    /// The QEMU arguments after the program name, in execution order.
    pub fn arguments(&self) -> Vec<OsString> {
        vec![
            OsString::from("-cpu"),
            OsString::from(&self.cpu),
            OsString::from("-L"),
            self.sysroot.as_os_str().to_owned(),
            self.helper.as_os_str().to_owned(),
        ]
    }

    /// The exact command line used to start QEMU, quoted for logging.
    pub fn command_line(&self) -> String {
        let mut parts = vec![quote_os(self.qemu.as_os_str())];
        for argument in self.arguments() {
            parts.push(quote_os(&argument));
        }
        parts.join(" ")
    }

    /// Re-check the resolved paths immediately before an exec.
    ///
    /// Discovery already validated them, but a caller can construct
    /// HelperPaths directly and an installation can change between discovery
    /// and spawn.  Failures name the exact path and environment variable.
    pub fn validate_for_spawn(&self) -> Result<()> {
        if !is_executable_file(&self.qemu) {
            return Err(HelperError::Discovery(format!(
                "QEMU executable is missing or not executable: {} (set APTX_QEMU to an absolute path)",
                self.qemu.display()
            )));
        }
        if !self.helper.is_file() {
            return Err(HelperError::Discovery(format!(
                "helper ELF is missing: {} (set APTX_HELPER; build with audio/aptx-adaptive/harness/build.sh)",
                self.helper.display()
            )));
        }
        if !self.sysroot.is_dir() {
            return Err(HelperError::Discovery(format!(
                "Hexagon sysroot is missing: {} (set APTX_HEXAGON_SYSROOT)",
                self.sysroot.display()
            )));
        }
        Ok(())
    }

    fn log_resolved(&self) {
        eprintln!(
            "aptx-native: helper: resolved qemu={} cpu={} sysroot={} helper={} cwd={}",
            self.qemu.display(),
            self.cpu,
            self.sysroot.display(),
            self.helper.display(),
            cwd_display()
        );
    }

    fn find_root() -> Result<PathBuf> {
        if let Some(root) = env_path("APTX_ADAPTIVE_ROOT") {
            if root.is_dir() {
                return Ok(root);
            }
            return Err(HelperError::Discovery(format!(
                "APTX_ADAPTIVE_ROOT={} is not a directory",
                root.display()
            )));
        }

        let mut candidates: Vec<(PathBuf, &'static str)> = Vec::new();
        if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
            let manifest = PathBuf::from(manifest);
            if let Some(parent) = manifest.parent() {
                candidates.push((parent.join("aptx-adaptive"), "CARGO_MANIFEST_DIR"));
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            for ancestor in cwd.ancestors() {
                candidates.push((ancestor.join("audio").join("aptx-adaptive"), "cwd ancestor"));
                candidates.push((ancestor.join("aptx-adaptive"), "cwd ancestor"));
                if ancestor.file_name().and_then(|name| name.to_str()) == Some("aptx-adaptive") {
                    candidates.push((ancestor.to_path_buf(), "cwd is inside aptx-adaptive"));
                }
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            for ancestor in exe.ancestors().skip(1).take(8) {
                candidates.push((
                    ancestor.join("audio").join("aptx-adaptive"),
                    "executable ancestor",
                ));
            }
        }

        let found = candidates.iter().find(|(candidate, _)| {
            candidate
                .join("build")
                .join("aptx-lossless-helper")
                .is_file()
                || candidate.join("toolchain").is_dir()
        });
        if let Some((root, _source)) = found {
            return Ok(root.clone());
        }

        let mut checked = String::new();
        for (candidate, source) in candidates.iter().take(24) {
            checked.push_str(&format!("\n  {} ({source})", candidate.display()));
        }
        Err(HelperError::Discovery(format!(
            "cannot locate the aptx-adaptive checkout; set APTX_ADAPTIVE_ROOT. cwd={} exe={} checked:{checked}",
            cwd_display(),
            exe_display()
        )))
    }

    fn validate_helper(helper: &Path, root: &Path) -> Result<()> {
        let metadata = std::fs::metadata(helper).map_err(|source| {
            HelperError::Discovery(format!(
                "helper ELF is missing at {}: {source} (root={}; build with audio/aptx-adaptive/harness/build.sh, or set APTX_HELPER)",
                helper.display(),
                root.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(HelperError::Discovery(format!(
                "helper path is not a regular file: {}",
                helper.display()
            )));
        }
        let mut file = std::fs::File::open(helper).map_err(|source| {
            HelperError::Discovery(format!(
                "helper ELF is not readable at {}: {source}",
                helper.display()
            ))
        })?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(|source| {
            HelperError::Discovery(format!(
                "cannot read helper ELF header at {}: {source}",
                helper.display()
            ))
        })?;
        if &magic != b"\x7fELF" {
            return Err(HelperError::Discovery(format!(
                "helper is not an ELF binary: {} (magic {:02x?})",
                helper.display(),
                magic
            )));
        }
        Ok(())
    }

    fn validate_sysroot(sysroot: &Path) -> Result<()> {
        let metadata = std::fs::metadata(sysroot).map_err(|source| {
            HelperError::Discovery(format!(
                "Hexagon sysroot is missing at {}: {source} (set APTX_HEXAGON_SYSROOT)",
                sysroot.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(HelperError::Discovery(format!(
                "Hexagon sysroot is not a directory: {}",
                sysroot.display()
            )));
        }
        if Self::sysroot_loader(sysroot).is_none() {
            eprintln!(
                "aptx-native: helper: warning: {} has no lib/ld-musl-hexagon.so.1; QEMU may fail to load the helper",
                sysroot.display()
            );
        }
        Ok(())
    }

    fn sysroot_loader(sysroot: &Path) -> Option<PathBuf> {
        [
            "lib/ld-musl-hexagon.so.1",
            "lib/ld-musl-hexagon.so",
            "usr/lib/ld-musl-hexagon.so.1",
        ]
        .into_iter()
        .map(|relative| sysroot.join(relative))
        .find(|candidate| candidate.exists())
    }

    fn find_sysroot(root: &Path) -> Result<PathBuf> {
        let toolchain = root.join("toolchain");
        let entries = std::fs::read_dir(&toolchain).map_err(|source| {
            HelperError::Discovery(format!(
                "cannot read toolchain directory {}: {source} (set APTX_HEXAGON_SYSROOT)",
                toolchain.display()
            ))
        })?;
        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            let base = entry.path();
            candidates.push(
                base.join("x86_64-linux-gnu")
                    .join("target")
                    .join("hexagon-unknown-linux-musl")
                    .join("usr"),
            );
            candidates.push(
                base.join("target")
                    .join("hexagon-unknown-linux-musl")
                    .join("usr"),
            );
            candidates.push(base.join("usr"));
        }
        if let Some(found) = candidates.iter().find(|candidate| candidate.is_dir()) {
            return Ok(found.clone());
        }
        let checked = candidates
            .iter()
            .map(|candidate| format!("\n  {}", candidate.display()))
            .collect::<String>();
        Err(HelperError::Discovery(format!(
            "no hexagon-unknown-linux-musl sysroot under {}; set APTX_HEXAGON_SYSROOT. checked:{checked}",
            toolchain.display()
        )))
    }

    fn resolve_qemu() -> Result<PathBuf> {
        let requested = env_path("APTX_QEMU").unwrap_or_else(|| PathBuf::from("qemu-hexagon"));
        if let Some(found) = find_executable(&requested) {
            return Ok(found);
        }
        let searched = if requested.components().count() == 1 {
            " (searched PATH)"
        } else {
            ""
        };
        let path_value = std::env::var_os("PATH")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unset>".to_string());
        Err(HelperError::Discovery(format!(
            "QEMU executable not found: {}{search}; PATH={path}; cwd={} (set APTX_QEMU to an absolute path or install qemu-user)",
            requested.display(),
            cwd_display(),
            search = searched,
            path = path_value
        )))
    }
}

/// Display the current directory for diagnostics, without failing discovery.
fn cwd_display() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("<unavailable: {error}>"))
}

/// Display the current executable for diagnostics.
fn exe_display() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("<unavailable: {error}>"))
}

/// Quote one argument for the diagnostic command line.
fn quote_os(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    let safe = !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '-' | '_' | '.' | '/' | ':' | '+' | '=' | ',' | '@' | '%'
                )
        });
    if safe {
        text.into_owned()
    } else {
        format!("{text:?}")
    }
}

/// Whether a path is a regular file with at least one execute bit.
fn is_executable_file(path: &Path) -> bool {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Resolve an executable name through PATH, returning an absolute path.
fn find_executable(requested: &Path) -> Option<PathBuf> {
    let has_separator = requested.components().count() > 1;
    if has_separator || requested.is_absolute() {
        return is_executable_file(requested).then(|| absolute_or_original(requested));
    }
    let path_value = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path_value) {
        let candidate = directory.join(requested);
        if is_executable_file(&candidate) {
            return Some(absolute_or_original(&candidate));
        }
    }
    None
}

/// Prefer the canonical path for logs, but keep the requested path if the
/// filesystem refuses to resolve it.
fn absolute_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Errors from starting, configuring or driving the helper.
#[derive(Debug)]
pub enum HelperError {
    /// An I/O error on one of the helper pipes.
    Io(std::io::Error),
    /// qemu-hexagon could not be executed.
    Spawn {
        /// The binary that failed to start.
        qemu: PathBuf,
        /// The exact command line that was attempted.
        command: String,
        /// The working directory used for the spawn.
        cwd: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },
    /// QEMU started but the helper did not complete its startup handshake.
    Startup(StartupFailure),
    /// The helper, sysroot or checkout could not be located.
    Discovery(String),
    /// The helper violated the wire protocol or closed its pipes.
    Protocol(String),
    /// The helper refused to start.
    StartRejected(u32),
    /// The helper rejected the configuration payload.
    ConfigRejected(u32),
    /// The helper rejected the quality-level request.
    QualityRejected(u32),
    /// The helper rejected a PCM chunk with a status other than EAGAIN.
    AudioRejected(u32),
    /// A caller-supplied argument was invalid.
    InvalidArgument(String),
}

/// A failed helper startup handshake.
///
/// The startup reply is the first thing the helper writes; when QEMU aborts
/// before that (an executable-memory denial, a wrong sysroot, a missing
/// loader) the only evidence is QEMU's stderr and exit status.  This type
/// keeps that evidence together with the exact command and cwd so the
/// journal entry is self-contained.
#[derive(Debug)]
pub struct StartupFailure {
    /// Exact command line attempted.
    pub command: String,
    /// Working directory used for the spawn.
    pub cwd: PathBuf,
    /// The handshake error (for example "helper closed after 0/8 bytes").
    pub cause: String,
    /// Exit code or signal, when the child had already exited.
    pub exit: Option<String>,
    /// Tail of QEMU/helper stderr (bounded to 8 KiB by the drain thread).
    pub stderr: String,
    /// Actionable remediation when the stderr matches a known failure.
    pub hint: Option<String>,
}

impl fmt::Display for StartupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "helper startup failed: {}; command={}; cwd={}; exit={}; stderr={:?}",
            self.cause,
            self.command,
            self.cwd.display(),
            self.exit.as_deref().unwrap_or("still running"),
            self.stderr.trim()
        )?;
        if let Some(hint) = &self.hint {
            write!(f, "; hint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for StartupFailure {}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "helper I/O error: {error}"),
            Self::Spawn {
                qemu,
                command,
                cwd,
                source,
            } => write!(
                f,
                "failed to start {}: {source}; command={command}; cwd={}",
                qemu.display(),
                cwd.display()
            ),
            Self::Startup(failure) => write!(f, "{failure}"),
            Self::Discovery(message) => write!(f, "{message}"),
            Self::Protocol(message) => write!(f, "helper protocol error: {message}"),
            Self::StartRejected(status) => {
                write!(f, "helper refused to start (status {status})")
            }
            Self::ConfigRejected(status) => {
                write!(f, "helper rejected the configuration (status {status})")
            }
            Self::QualityRejected(status) => {
                write!(f, "helper rejected the quality level (status {status})")
            }
            Self::AudioRejected(status) => {
                write!(f, "helper rejected the PCM chunk (status {status})")
            }
            Self::InvalidArgument(message) => write!(f, "invalid argument: {message}"),
        }
    }
}

impl std::error::Error for HelperError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Spawn { source, .. } => Some(source),
            Self::Startup(failure) => Some(failure),
            _ => None,
        }
    }
}

impl From<std::io::Error> for HelperError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Convenience result type for this module.
pub type Result<T> = std::result::Result<T, HelperError>;

/// A live encoder helper process.
///
/// Construction spawns QEMU, waits for the startup reply and sends the
/// configuration.  Feed interleaved stereo S32 chunks with
/// HelperBridge::encode_interleaved_s32 (or raw little-endian bytes with
/// HelperBridge::encode_pcm); each call returns Some(packet) when the helper
/// completed a frame, or None while it is buffering.
pub struct HelperBridge {
    child: Child,
    paths: HelperPaths,
    config: HelperConfig,
    startup_payload_size: u32,
    packets: u64,
    stderr: StderrDrain,
    closed: bool,
    /// Exit status captured while shutting the child down, for diagnostics.
    exit_status: Option<ExitStatus>,
}

impl HelperBridge {
    /// Spawn the helper with automatic path discovery and the given config.
    pub fn spawn(config: HelperConfig) -> Result<Self> {
        let paths = HelperPaths::discover()?;
        Self::spawn_with_paths(&paths, config)
    }

    /// Spawn the helper at explicit paths and configure it.
    ///
    /// The exact command line, cwd and relevant APTX_* variables are logged
    /// before exec; QEMU's stderr is drained into a bounded buffer and is
    /// included in the error if the startup handshake fails.  On failure the
    /// child is killed and reaped before the error is returned.
    pub fn spawn_with_paths(paths: &HelperPaths, config: HelperConfig) -> Result<Self> {
        paths.validate_for_spawn()?;
        let command_line = paths.command_line();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("?"));
        log_spawn_context(paths, &command_line, &cwd);

        let mut child = paths
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| {
                eprintln!(
                    "aptx-native: helper: spawn failed: {source}; command={command_line}; cwd={}",
                    cwd.display()
                );
                HelperError::Spawn {
                    qemu: paths.qemu.clone(),
                    command: command_line.clone(),
                    cwd: cwd.clone(),
                    source,
                }
            })?;

        eprintln!(
            "aptx-native: helper: spawned pid={} from process {}",
            child.id(),
            std::process::id()
        );
        let stderr = StderrDrain::new(child.stderr.take());
        let mut bridge = Self {
            child,
            paths: paths.clone(),
            config,
            startup_payload_size: 0,
            packets: 0,
            stderr,
            closed: false,
            exit_status: None,
        };

        match bridge.initialize() {
            Ok(()) => {
                eprintln!(
                    "aptx-native: helper: ready pid={} rate={} mode={} profile={} mtu={} startup_payload={}",
                    bridge.pid(),
                    bridge.config.source_rate,
                    bridge.config.mode,
                    bridge.config.profile,
                    bridge.config.mtu,
                    bridge.startup_payload_size
                );
                Ok(bridge)
            }
            Err(error) => {
                let cause = error.to_string();
                let _ = bridge.shutdown();
                let stderr = bridge.stderr_tail();
                let exit = bridge.exit_description();
                let hint = startup_hint(&stderr).map(str::to_string);
                let failure = StartupFailure {
                    command: command_line.clone(),
                    cwd: cwd.clone(),
                    cause,
                    exit,
                    stderr,
                    hint,
                };
                eprintln!("aptx-native: helper: {failure}");
                Err(HelperError::Startup(failure))
            }
        }
    }

    /// The configuration this bridge was started with.
    pub fn config(&self) -> &HelperConfig {
        &self.config
    }

    /// The resolved helper paths.
    pub fn paths(&self) -> &HelperPaths {
        &self.paths
    }

    /// PID of the QEMU process wrapping the helper.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Payload size reported in the startup reply (zero on current helpers).
    pub fn startup_payload_size(&self) -> u32 {
        self.startup_payload_size
    }

    /// Number of OTA packets returned so far.
    pub fn packets_emitted(&self) -> u64 {
        self.packets
    }

    /// Recent helper stderr, for diagnostics (bounded to the last 8 KiB).
    pub fn stderr_tail(&self) -> String {
        self.stderr.tail()
    }

    /// Whether the helper process is still alive.
    pub fn is_running(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    /// Send the IMCL bit-rate/quality-level feedback (1..15).
    pub fn set_quality_level(&mut self, level: u32) -> Result<()> {
        let reply = self.request(CMD_SET_QUALITY_LEVEL, &level.to_le_bytes())?;
        if reply.status != STATUS_OK {
            return Err(HelperError::QualityRejected(reply.status));
        }
        Ok(())
    }

    /// Feed one chunk of interleaved stereo S32 little-endian PCM.
    ///
    /// The chunk must be a non-empty multiple of 8 bytes (one stereo frame)
    /// and at most PCM_BYTES_MAX.  Returns Some(packet) when a frame
    /// completed, None for the helper's EAGAIN reply.
    pub fn encode_pcm(&mut self, pcm: &[u8]) -> Result<Option<Vec<u8>>> {
        if self.closed {
            return Err(HelperError::Protocol("bridge is closed".to_string()));
        }
        if pcm.is_empty() {
            return Err(invalid_arg("PCM chunk is empty"));
        }
        if pcm.len() % 8 != 0 {
            return Err(invalid_arg(format!(
                "PCM chunk length {} is not a multiple of 8 bytes (stereo S32)",
                pcm.len()
            )));
        }
        if pcm.len() > PCM_BYTES_MAX {
            return Err(invalid_arg(format!(
                "PCM chunk length {} exceeds the helper limit of {PCM_BYTES_MAX}",
                pcm.len()
            )));
        }

        self.write_all(&(pcm.len() as u32).to_le_bytes())?;
        self.write_all(pcm)?;

        let reply = self.read_reply()?;
        match reply.status {
            STATUS_OK => {
                self.packets += 1;
                Ok(Some(reply.payload))
            }
            STATUS_EAGAIN => Ok(None),
            status => Err(HelperError::AudioRejected(status)),
        }
    }

    /// Feed one chunk of interleaved stereo i32 samples.
    ///
    /// samples must contain a whole number of frames (an even number of
    /// values, left then right); values are sent little-endian.
    pub fn encode_interleaved_s32(&mut self, samples: &[i32]) -> Result<Option<Vec<u8>>> {
        if samples.len() % 2 != 0 {
            return Err(invalid_arg(format!(
                "PCM sample count {} is odd (need stereo pairs)",
                samples.len()
            )));
        }
        let mut pcm = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        self.encode_pcm(&pcm)
    }

    /// Close the helper's stdin, wait briefly for it to exit and reap it.
    ///
    /// Calling this more than once is harmless; Drop also calls it.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        // Closing stdin is the helper's EOF signal.
        drop(self.child.stdin.take());

        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.exit_status = Some(status);
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        if let Ok(status) = self.child.wait() {
                            self.exit_status = Some(status);
                        }
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    let _ = self.child.kill();
                    if let Ok(status) = self.child.wait() {
                        self.exit_status = Some(status);
                    }
                    self.stderr.finish();
                    return Err(HelperError::Io(error));
                }
            }
        }
        self.stderr.finish();
        Ok(())
    }

    /// Exit code or signal of the reaped child, formatted for a log line.
    fn exit_description(&self) -> Option<String> {
        self.exit_status.as_ref().map(describe_exit)
    }

    fn initialize(&mut self) -> Result<()> {
        let startup = self.read_reply()?;
        if startup.status != STATUS_OK {
            return Err(HelperError::StartRejected(startup.status));
        }
        self.startup_payload_size = startup.payload.len() as u32;

        let payload = self.config.to_wire_payload();
        let reply = self.request(CMD_CONFIG, &payload)?;
        if reply.status != STATUS_OK {
            return Err(HelperError::ConfigRejected(reply.status));
        }
        Ok(())
    }

    fn request(&mut self, command: u32, payload: &[u8]) -> Result<Reply> {
        let mut message = Vec::with_capacity(12 + payload.len());
        message.extend_from_slice(&CONTROL_WORD.to_le_bytes());
        message.extend_from_slice(&command.to_le_bytes());
        message.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        message.extend_from_slice(payload);
        self.write_all(&message)?;
        self.read_reply()
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| HelperError::Protocol("helper stdin is closed".to_string()))?;
        stdin.write_all(bytes)?;
        stdin.flush()?;
        Ok(())
    }

    fn read_reply(&mut self) -> Result<Reply> {
        let header = self.read_exact(8)?;
        let status = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if size > MAX_REPLY_BYTES {
            return Err(HelperError::Protocol(format!(
                "reply payload of {size} bytes exceeds the {MAX_REPLY_BYTES}-byte limit"
            )));
        }
        let payload = if size == 0 {
            Vec::new()
        } else {
            self.read_exact(size)?
        };
        Ok(Reply { status, payload })
    }

    fn read_exact(&mut self, size: usize) -> Result<Vec<u8>> {
        let stdout = self
            .child
            .stdout
            .as_mut()
            .ok_or_else(|| HelperError::Protocol("helper stdout is closed".to_string()))?;
        let mut buffer = vec![0u8; size];
        let mut received = 0;
        while received < size {
            match stdout.read(&mut buffer[received..]) {
                Ok(0) => {
                    return Err(HelperError::Protocol(format!(
                        "helper closed after {received}/{size} bytes"
                    )))
                }
                Ok(count) => received += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(HelperError::Io(error)),
            }
        }
        Ok(buffer)
    }
}

impl Drop for HelperBridge {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Reply {
    status: u32,
    payload: Vec<u8>,
}

/// Render an exit status as "exit code N" or "killed by signal N".
fn describe_exit(status: &ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    format!("{status:?}")
}

/// Turn a known QEMU startup failure into an actionable hint.
///
/// The dominant production failure is the systemd user-service sandbox:
/// pipewire.service and wireplumber.service both set
/// MemoryDenyWriteExecute=yes, and QEMU's TCG code buffer needs a
/// writable+executable mapping, so qemu-hexagon aborts before writing the
/// startup reply:
///
/// ```text
/// qemu-hexagon: qemu_mprotect__osdep: mprotect failed: Permission denied
/// ```
///
/// The hint is diagnostic only; deciding whether to fall back is the caller's
/// job (see APTX_NATIVE_HELPER=auto in the C ABI).
fn startup_hint(stderr: &str) -> Option<&'static str> {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("mprotect failed") {
        return Some(
            "qemu-hexagon needs writable+executable memory, which the user-service sandbox blocks; add MemoryDenyWriteExecute=no to pipewire.service.d/zz-aptx-native.conf and wireplumber.service.d/zz-aptx-native.conf, or leave APTX_NATIVE_HELPER=auto so the native encoder runs",
        );
    }
    if lower.contains("operation not permitted") || lower.contains("seccomp") {
        return Some(
            "a service sandbox syscall filter blocked QEMU; check SystemCallFilter in the pipewire/wireplumber user service",
        );
    }
    if lower.contains("ld-musl")
        || lower.contains("interpreter")
        || lower.contains("could not open")
    {
        return Some(
            "check APTX_HEXAGON_SYSROOT and APTX_HELPER: QEMU could not load the helper ELF or its musl loader",
        );
    }
    if lower.contains("permission denied") {
        return Some(
            "check read/execute permission on APTX_HELPER, APTX_HEXAGON_SYSROOT and their parent directories",
        );
    }
    None
}

/// Log the exact spawn context and the APTX_* environment once per stream.
fn log_spawn_context(paths: &HelperPaths, command_line: &str, cwd: &Path) {
    eprintln!(
        "aptx-native: helper: spawn command={command_line} cwd={} process={}",
        cwd.display(),
        std::process::id()
    );
    eprintln!(
        "aptx-native: helper: spawn paths helper={} sysroot={} cpu={}",
        paths.helper.display(),
        paths.sysroot.display(),
        paths.cpu
    );
    for name in [
        "APTX_ADAPTIVE_ROOT",
        "APTX_HELPER",
        "APTX_QEMU",
        "APTX_QEMU_CPU",
        "APTX_HEXAGON_SYSROOT",
        "APTX_NATIVE_HELPER",
        "APTX_NATIVE_LL",
        "APTX_NATIVE_REPLAY",
    ] {
        let value = std::env::var_os(name)
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "<unset>".to_string());
        eprintln!("aptx-native: helper: spawn env {name}={value}");
    }
}

fn invalid_arg(message: impl Into<String>) -> HelperError {
    HelperError::InvalidArgument(message.into())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// Drains the helper's stderr on a background thread so a chatty QEMU can
/// never fill the pipe and stall the encoder.
struct StderrDrain {
    text: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl StderrDrain {
    const MAX_BYTES: usize = 8 * 1024;

    fn new(stderr: Option<std::process::ChildStderr>) -> Self {
        let text = Arc::new(Mutex::new(String::new()));
        let handle = stderr.map(|mut pipe| {
            let text = Arc::clone(&text);
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(count) => {
                            let mut text = text.lock().unwrap_or_else(|poison| poison.into_inner());
                            text.push_str(&String::from_utf8_lossy(&chunk[..count]));
                            if text.len() > Self::MAX_BYTES {
                                let excess = text.len() - Self::MAX_BYTES;
                                let cut = (0..=excess)
                                    .rev()
                                    .find(|&index| text.is_char_boundary(index))
                                    .unwrap_or(0);
                                text.drain(..cut);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
        });
        Self { text, handle }
    }

    fn tail(&self) -> String {
        self.text
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default()
    }

    fn finish(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Serializes tests that mutate the process environment (APTX_* variables).
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn bose_config_payload_layout() {
        let config = HelperConfig::bose();
        let payload = config.to_wire_payload();

        assert_eq!(payload.len(), 44 + CIE_LEN + R2_STREAM_LEN);
        assert_eq!(u32_at(&payload, 0), PROTOCOL_VERSION);
        assert_eq!(u32_at(&payload, 4), 48_000);
        assert_eq!(u32_at(&payload, 8), 48_000);
        assert_eq!(u32_at(&payload, 12), 2);
        assert_eq!(u32_at(&payload, 16), 6);
        assert_eq!(u32_at(&payload, 20), 995);
        assert_eq!(u32_at(&payload, 24), 0);
        assert_eq!(u32_at(&payload, 28), 32);
        assert_eq!(u32_at(&payload, 32), 0);
        assert_eq!(u32_at(&payload, 36), 0);
        assert_eq!(u32_at(&payload, 40), CIE_LEN as u32);
        assert_eq!(&payload[44..44 + CIE_LEN], &BOSE_CIE[..]);
        assert_eq!(
            &payload[44 + CIE_LEN..],
            &BOSE_R2_STREAM[..],
            "stream descriptor"
        );
    }

    #[test]
    fn bose_cie_hex_round_trip() {
        let cie = cie_from_hex(BOSE_CIE_HEX).expect("captured CIE parses");
        assert_eq!(cie, BOSE_CIE);
        assert_eq!(&cie[..28], &BOSE_CIE[..28]);
        assert_eq!(&cie[28..], &[0u8; 12]);

        assert!(cie_from_hex(&"11".repeat(CIE_LEN + 1)).is_err());
        assert!(cie_from_hex("abc").is_err());
        assert!(cie_from_hex("zz").is_err());
    }

    #[test]
    fn tone_produces_r2_packets() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let paths = match HelperPaths::discover() {
            Ok(paths) => paths,
            Err(error) => {
                eprintln!("skipping helper bridge test: {error}");
                return;
            }
        };
        let mut bridge = match HelperBridge::spawn_with_paths(&paths, HelperConfig::bose()) {
            Ok(bridge) => bridge,
            Err(HelperError::Spawn { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                eprintln!("skipping helper bridge test: qemu-hexagon not installed");
                return;
            }
            Err(error) => panic!("helper failed to start: {error}"),
        };

        assert_eq!(bridge.startup_payload_size(), 0);

        // 0.25 s of a 440 Hz tone at the native rig level, fed in the same
        // 256-frame chunks the research harness uses.
        const RATE: u32 = 48_000;
        const CHUNK: usize = 256;
        let total_frames = RATE as usize / 4;
        let step = 2.0 * std::f64::consts::PI * 440.0 / RATE as f64;
        let mut phase = 0.0f64;
        let mut packets = 0usize;
        let mut frames_sent = 0usize;

        while frames_sent < total_frames {
            let frames = CHUNK.min(total_frames - frames_sent);
            let mut samples = Vec::with_capacity(frames * 2);
            for _ in 0..frames {
                let value = (0.5 * f64::from(1u32 << 30) * phase.sin()) as i32;
                samples.push(value);
                samples.push(value);
                phase += step;
            }
            match bridge
                .encode_interleaved_s32(&samples)
                .expect("helper accepted the PCM chunk")
            {
                Some(packet) => {
                    assert_eq!(packet.len(), R2_OTA_LEN, "R2 OTA packet length");
                    assert_eq!(packet[7], R2_OTA_TYPE, "R2 OTA version byte");
                    let ttp = u16::from_le_bytes([packet[0], packet[1]]);
                    assert_ne!(ttp, 0, "TTP is populated");
                    packets += 1;
                }
                None => {}
            }
            frames_sent += frames;
        }

        let stderr = bridge.stderr_tail();
        let emitted = bridge.packets_emitted();
        bridge.shutdown().expect("clean helper shutdown");
        assert_eq!(emitted, packets as u64);
        assert!(
            packets > 0,
            "helper produced no packets for a 0.25 s tone; stderr: {stderr}"
        );
        eprintln!(
            "helper bridge tone test: {packets} R2 packets of {R2_OTA_LEN} bytes from {frames_sent} frames"
        );
    }

    #[test]
    fn command_arguments_are_exact_and_quoted() {
        let paths = HelperPaths {
            qemu: PathBuf::from("/usr/bin/qemu-hexagon"),
            cpu: "v73".to_string(),
            sysroot: PathBuf::from("/tmp/sys root"),
            helper: PathBuf::from("/tmp/helper elf"),
        };
        assert_eq!(
            paths.arguments(),
            vec![
                OsString::from("-cpu"),
                OsString::from("v73"),
                OsString::from("-L"),
                OsString::from("/tmp/sys root"),
                OsString::from("/tmp/helper elf"),
            ]
        );
        let line = paths.command_line();
        assert!(
            line.starts_with("/usr/bin/qemu-hexagon -cpu v73 -L "),
            "{line}"
        );
        assert!(line.contains("\"/tmp/sys root\""), "{line}");
        assert!(line.ends_with("\"/tmp/helper elf\""), "{line}");
    }

    #[test]
    fn startup_hint_names_the_service_sandbox() {
        let hint =
            startup_hint("qemu-hexagon: qemu_mprotect__osdep: mprotect failed: Permission denied")
                .expect("mprotect failure has a hint");
        assert!(hint.contains("MemoryDenyWriteExecute=no"), "{hint}");
        assert!(hint.contains("APTX_NATIVE_HELPER=auto"), "{hint}");
        assert!(startup_hint("").is_none());
    }

    #[test]
    fn startup_failure_carries_command_exit_and_stderr() {
        let real = match HelperPaths::discover() {
            Ok(paths) => paths,
            Err(error) => {
                eprintln!("skipping startup-failure test: {error}");
                return;
            }
        };
        let paths = HelperPaths {
            qemu: PathBuf::from("/bin/false"),
            cpu: "v73".to_string(),
            sysroot: real.sysroot,
            helper: real.helper,
        };
        let error = match HelperBridge::spawn_with_paths(&paths, HelperConfig::bose()) {
            Ok(_) => panic!("/bin/false unexpectedly started a helper"),
            Err(error) => error,
        };
        match error {
            HelperError::Startup(failure) => {
                assert!(
                    failure.command.contains("/bin/false"),
                    "{}",
                    failure.command
                );
                assert!(failure.command.contains("-cpu v73"), "{}", failure.command);
                assert_eq!(failure.cwd, std::env::current_dir().expect("test cwd"));
                assert!(
                    failure.exit.is_some(),
                    "exit status was not captured: {failure}"
                );
                assert!(failure.to_string().contains("command=/bin/false"));
            }
            other => panic!("expected HelperError::Startup, got {other:?}"),
        }
    }
}
