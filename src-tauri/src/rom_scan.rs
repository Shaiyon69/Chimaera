//! ROM folder scanning, disc sniffing and disc SKU extraction. Port of
//! `services/emulators/scan-rom-folder.ts`, `sniff-disc-platform.ts`,
//! `extract-disc-sku.ts`, `sku-scan.ts`, `sku-normalize.ts`, `param-sfo.ts`,
//! `dolphin-disc-reader.ts`, `cso-*.ts` and `verified-ps3-iso-title-id.ts`.
//! CHD images go through the `chd` crate instead of a port of `chd-reader.ts`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use regex::bytes::Regex;

const MAX_COLLECTED_FILES: usize = 50_000;
const DIR_SIZE_ENTRY_CAP: usize = 100_000;

fn directory_markers(system: &str) -> &'static [&'static str] {
    match system {
        "ps3" => &["PS3_GAME", "ps3_game"],
        "psp" => &["PSP_GAME", "psp_game"],
        _ => &[],
    }
}

struct Candidate {
    full_path: PathBuf,
    name: String,
    is_marker_dir: bool,
}

pub struct ScannedGame {
    pub primary_path: PathBuf,
    pub name: String,
    pub size_bytes: u64,
    pub wrong_platform: bool,
}

pub struct ScanResult {
    pub file_count: u64,
    pub size_bytes: u64,
    pub games: Vec<ScannedGame>,
}

/// `.cue` for `game.cue`; empty for dotfiles and names without a dot, like Electron.
fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[dot..].to_lowercase(),
        _ => String::new(),
    }
}

fn base_no_ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

fn compute_dir_size(root: &Path, cancel: &AtomicBool) -> u64 {
    let (mut total, mut visited) = (0u64, 0usize);
    let mut queue = VecDeque::from([root.to_path_buf()]);
    let mut seen = HashSet::new();
    while let Some(dir) = queue.pop_front() {
        if cancelled(cancel) {
            break;
        }
        let Ok(real) = std::fs::canonicalize(&dir) else { continue };
        if !seen.insert(real) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if cancelled(cancel) {
                return total;
            }
            visited += 1;
            if visited > DIR_SIZE_ENTRY_CAP + 1 {
                return total;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                queue.push_back(entry.path());
            } else if kind.is_file() {
                total += file_size(&entry.path()).unwrap_or(0);
            }
        }
    }
    total
}

fn collect_candidates(root: &Path, system: &str, extensions: &[&str], scan_subfolders: bool, cancel: &AtomicBool) -> Vec<Candidate> {
    let markers = directory_markers(system);
    let mut candidates = Vec::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    let mut seen = HashSet::new();
    while let Some(dir) = queue.pop_front() {
        if cancelled(cancel) {
            break;
        }
        if candidates.len() >= MAX_COLLECTED_FILES {
            eprintln!("[rom-scan] stopped at the {MAX_COLLECTED_FILES} file cap in {}", root.display());
            break;
        }
        let Ok(real) = std::fs::canonicalize(&dir) else { continue };
        if !seen.insert(real) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if cancelled(cancel) {
                break;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let full = entry.path();
            if kind.is_dir() {
                if markers.contains(&name.as_str()) {
                    // PSP launches the folder that holds PSP_GAME, PS3 the PS3_GAME folder itself.
                    let launch = if system == "psp" { dir.clone() } else { full };
                    let name = launch.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    candidates.push(Candidate { full_path: launch, name, is_marker_dir: true });
                } else if scan_subfolders {
                    queue.push_back(full);
                }
            } else if kind.is_file() {
                let lower = name.to_lowercase();
                if extensions.iter().any(|ext| lower.ends_with(ext)) {
                    candidates.push(Candidate { full_path: full, name, is_marker_dir: false });
                }
            }
        }
    }
    candidates
}

struct GameGroup {
    primary: usize,
    sidecars: Vec<usize>,
}

const PS1_PRIMARY: &[&str] = &[".cue", ".ccd", ".mds", ".chd", ".pbp", ".iso", ".ecm"];
const PS1_PAIRS: &[(&str, &[&str])] = &[(".cue", &[".bin"]), (".ccd", &[".img", ".sub"]), (".mds", &[".mdf"])];
const PS2_PRIMARY: &[&str] = &[".iso", ".chd", ".cso", ".zso", ".gz", ".nrg", ".cue", ".mds"];
const PS2_PAIRS: &[(&str, &[&str])] = &[(".cue", &[".bin"]), (".mds", &[".mdf"])];
const PS3_LAUNCHABLE: &[&str] = &[".iso", ".pkg", ".elf", ".self"];

fn pair_rule(pairs: &[(&str, &'static [&'static str])], ext: &str) -> Option<&'static [&'static str]> {
    pairs.iter().find(|(primary, _)| *primary == ext).map(|(_, sidecars)| *sidecars)
}

/// applyPairedRules: an .m3u playlist wins; otherwise each .cue/.ccd/.mds
/// swallows same-named sidecars, and orphan sidecars stand alone.
fn paired_groups(all: &[Candidate], group: &[usize], primary_exts: &[&str], pairs: &[(&str, &'static [&'static str])]) -> Vec<GameGroup> {
    let playlists: Vec<usize> = group.iter().copied().filter(|&i| ext_of(&all[i].name) == ".m3u").collect();
    if !playlists.is_empty() {
        return playlists.into_iter().map(|primary| GameGroup { primary, sidecars: vec![] }).collect();
    }
    let mut sidecar_of: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut skipped = HashSet::new();
    for &f in group {
        let Some(sidecar_exts) = pair_rule(pairs, &ext_of(&all[f].name)) else { continue };
        let base = base_no_ext(&all[f].name);
        let matched: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&o| o != f && base_no_ext(&all[o].name) == base && sidecar_exts.contains(&ext_of(&all[o].name).as_str()))
            .collect();
        skipped.extend(matched.iter().copied());
        sidecar_of.insert(f, matched);
    }
    let all_sidecar_exts: HashSet<&str> = pairs.iter().flat_map(|(_, s)| s.iter().copied()).collect();
    group
        .iter()
        .copied()
        .filter(|f| !skipped.contains(f))
        .filter_map(|f| {
            let ext = ext_of(&all[f].name);
            if primary_exts.contains(&ext.as_str()) {
                Some(GameGroup { primary: f, sidecars: sidecar_of.remove(&f).unwrap_or_default() })
            } else if all_sidecar_exts.contains(ext.as_str()) {
                Some(GameGroup { primary: f, sidecars: vec![] })
            } else {
                None
            }
        })
        .collect()
}

fn dedup_games(system: &str, all: &[Candidate]) -> Vec<GameGroup> {
    let mut games: Vec<GameGroup> = (0..all.len()).filter(|&i| all[i].is_marker_dir).map(|primary| GameGroup { primary, sidecars: vec![] }).collect();
    // Grouped by parent folder in first-seen order, like Electron's Map.
    let mut order: Vec<PathBuf> = Vec::new();
    let mut by_dir: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, c) in all.iter().enumerate().filter(|(_, c)| !c.is_marker_dir) {
        let parent = c.full_path.parent().map(Path::to_path_buf).unwrap_or_default();
        by_dir.entry(parent.clone()).or_insert_with(|| { order.push(parent); Vec::new() }).push(i);
    }
    for dir in order {
        let group = &by_dir[&dir];
        match system {
            "ps1" => games.extend(paired_groups(all, group, PS1_PRIMARY, PS1_PAIRS)),
            "ps2" => games.extend(paired_groups(all, group, PS2_PRIMARY, PS2_PAIRS)),
            "ps3" => games.extend(group.iter().copied().filter(|&i| PS3_LAUNCHABLE.contains(&ext_of(&all[i].name).as_str())).map(|primary| GameGroup { primary, sidecars: vec![] })),
            _ => games.extend(group.iter().map(|&primary| GameGroup { primary, sidecars: vec![] })),
        }
    }
    games
}

#[derive(PartialEq)]
enum Classification {
    Ok,
    WrongPlatform,
    Skip,
}

const SNIFFABLE: &[&str] = &[".cue", ".iso", ".img", ".mds", ".ccd", ".bin", ".mdf"];
const PS3_INTERNAL_FILES: &[&str] = &["eboot.bin", "param.sfo", "ps3_disc.sfb"];

fn sniffed(path: &Path) -> Option<Platform> {
    resolve_sniff_target(path).map(|target| sniff_disc_image(&target))
}

fn classify(c: &Candidate, system: &str) -> Classification {
    use Classification::*;
    if c.is_marker_dir {
        return Ok;
    }
    let ext = ext_of(&c.name);
    let verdict = |accept: &dyn Fn(Platform) -> bool| match sniffed(&c.full_path) {
        None | Some(Platform::Unknown) => Ok,
        Some(p) if accept(p) => Ok,
        Some(_) => WrongPlatform,
    };
    match system {
        "ps3" => {
            if PS3_INTERNAL_FILES.contains(&c.name.to_lowercase().as_str()) {
                Skip
            } else if ext == ".iso" {
                verdict(&|p| p == Platform::Ps3)
            } else if [".pkg", ".elf", ".self"].contains(&ext.as_str()) {
                Ok
            } else {
                Skip
            }
        }
        "psp" => Ok,
        "dolphin" if ext == ".wad" || ext == ".wbfs" => Ok,
        "dolphin" => verdict(&|p| p == Platform::GameCube || p == Platform::Wii),
        _ if !SNIFFABLE.contains(&ext.as_str()) => Ok,
        _ => verdict(&|p| p.system() == Some(system)),
    }
}

fn size_game(all: &[Candidate], game: &GameGroup, cancel: &AtomicBool) -> (u64, u64) {
    let primary = &all[game.primary];
    let (mut counted, mut size) = (0, 0);
    if primary.is_marker_dir {
        size += compute_dir_size(&primary.full_path, cancel);
        counted = 1;
    } else if let Some(s) = file_size(&primary.full_path) {
        counted = 1;
        size += s;
    }
    for &sidecar in &game.sidecars {
        if cancelled(cancel) {
            break;
        }
        size += file_size(&all[sidecar].full_path).unwrap_or(0);
    }
    (counted, size)
}

/// scanRomFolder: `progress(processed, total, current file, kept)`.
pub fn scan_rom_folder(root: &Path, system: &str, scan_subfolders: bool, cancel: &AtomicBool, progress: &mut dyn FnMut(usize, usize, Option<&str>, usize)) -> Result<ScanResult, String> {
    let k = crate::emulators::known(system)?;
    let all = collect_candidates(root, system, k.rom_extensions, scan_subfolders, cancel);
    let games = dedup_games(system, &all);
    let total = games.len();
    let mut result = ScanResult { file_count: 0, size_bytes: 0, games: Vec::new() };
    progress(0, total, None, 0);
    for (i, game) in games.iter().enumerate() {
        if cancelled(cancel) {
            break;
        }
        let primary = &all[game.primary];
        let class = classify(primary, system);
        if class != Classification::Skip {
            let (counted, size) = size_game(&all, game, cancel);
            if class == Classification::Ok {
                result.file_count += counted;
                result.size_bytes += size;
            }
            result.games.push(ScannedGame { primary_path: primary.full_path.clone(), name: primary.name.clone(), size_bytes: size, wrong_platform: class == Classification::WrongPlatform });
        }
        progress(i + 1, total, Some(&primary.name), result.games.len());
    }
    Ok(result)
}

pub fn count_rom_groups(root: &Path, system: &str, scan_subfolders: bool, cancel: &AtomicBool) -> Result<usize, String> {
    let k = crate::emulators::known(system)?;
    Ok(dedup_games(system, &collect_candidates(root, system, k.rom_extensions, scan_subfolders, cancel)).len())
}

// ---- Disc sniffing ----------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Platform {
    Ps1,
    Ps2,
    Ps3,
    GameCube,
    Wii,
    Unknown,
}

impl Platform {
    fn system(self) -> Option<&'static str> {
        match self {
            Platform::Ps1 => Some("ps1"),
            Platform::Ps2 => Some("ps2"),
            Platform::Ps3 => Some("ps3"),
            Platform::GameCube | Platform::Wii => Some("dolphin"),
            Platform::Unknown => None,
        }
    }
}

const SNIFF_BYTES: usize = 16 * 1024 * 1024;

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Reads up to `len` bytes at `offset`; short at end of file.
fn read_up_to(file: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity(len.min(64 * 1024 * 1024));
    file.by_ref().take(len as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn read_exact_at(file: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    read_up_to(file, offset, len).filter(|b| b.len() == len)
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

static BOOT2_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)BOOT2\s*=").unwrap());
static BOOT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)BOOT\s*=").unwrap());

fn classify_playstation(data: &[u8]) -> Platform {
    let ps3_hits = ["PS3_GAME", "PS3_DISC.SFB", "PARAM.SFO", "EBOOT.BIN"].iter().filter(|m| contains(data, m.as_bytes())).count();
    if ps3_hits >= 2 {
        Platform::Ps3
    } else if BOOT2_RE.is_match(data) {
        Platform::Ps2
    } else if BOOT_RE.is_match(data) {
        Platform::Ps1
    } else if ps3_hits == 1 {
        Platform::Ps3
    } else {
        Platform::Unknown
    }
}

pub fn sniff_disc_image(path: &Path) -> Platform {
    if let Some(header) = read_dolphin_disc_header(path) {
        if be32(&header, 0x18) == 0x5d1c_9ea3 {
            return Platform::Wii;
        }
        if be32(&header, 0x1c) == 0xc233_9f3d {
            return Platform::GameCube;
        }
    }
    let Ok(mut file) = File::open(path) else { return Platform::Unknown };
    read_up_to(&mut file, 0, SNIFF_BYTES).map(|data| classify_playstation(&data)).unwrap_or(Platform::Unknown)
}

/// A file in `dir` named `base`, matched case-insensitively when the exact name is missing.
fn find_in_dir(dir: &Path, base: &str) -> Option<PathBuf> {
    let direct = dir.join(base);
    if direct.exists() {
        return Some(direct);
    }
    let wanted = base.to_lowercase();
    std::fs::read_dir(dir).ok()?.flatten().find(|e| e.file_name().to_string_lossy().to_lowercase() == wanted).map(|e| e.path())
}

static CUE_FILE_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?i)FILE\s+"(.+?)"\s+\w+"#).unwrap());

pub fn parse_cue_referenced_files(cue: &Path) -> Vec<PathBuf> {
    let Ok(content) = std::fs::read_to_string(cue) else { return vec![] };
    let dir = cue.parent().unwrap_or(Path::new(""));
    CUE_FILE_RE
        .captures_iter(&content)
        .filter_map(|c| {
            let reference = c[1].replace('\\', "/");
            let base = reference.rsplit('/').next().unwrap_or_default();
            find_in_dir(dir, base)
        })
        .collect()
}

pub(crate) fn sidecar_with_ext(path: &Path, ext: &str) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    find_in_dir(path.parent()?, &format!("{stem}{ext}"))
}

/// resolveSniffTarget: the file that holds the disc data, or None for formats we cannot read raw.
pub fn resolve_sniff_target(path: &Path) -> Option<PathBuf> {
    let ext = ext_of(&path.file_name()?.to_string_lossy());
    match ext.as_str() {
        ".cue" => parse_cue_referenced_files(path).into_iter().next(),
        ".mds" => sidecar_with_ext(path, ".mdf"),
        ".ccd" => sidecar_with_ext(path, ".img"),
        ".iso" | ".img" | ".bin" | ".mdf" | ".gcm" | ".rvz" | ".wia" | ".tgc" => Some(path.to_path_buf()),
        _ => None,
    }
}

// ---- GameCube / Wii headers -------------------------------------------------

const DISC_HEADER_SIZE: usize = 0x20;
const MAX_DOLPHIN_BLOCK_SIZE: u32 = 16 * 1024 * 1024;

fn valid_block_size(size: u32) -> bool {
    size >= DISC_HEADER_SIZE as u32 && size <= MAX_DOLPHIN_BLOCK_SIZE
}

fn gcz_header(file: &mut File) -> Option<Vec<u8>> {
    let header = read_exact_at(file, 0, 0x20)?;
    if le32(&header, 0) != 0xb10b_c001 {
        return None;
    }
    let compressed_size = le64(&header, 0x08);
    let (block_size, block_count) = (le32(&header, 0x18), le32(&header, 0x1c));
    if !valid_block_size(block_size) || block_count == 0 {
        return None;
    }
    const UNCOMPRESSED: u64 = 1 << 63;
    let pointer = |file: &mut File, i: u64| read_exact_at(file, 0x20 + i * 8, 8).map(|b| le64(&b, 0));
    let first = pointer(file, 0)?;
    let next = if block_count == 1 { compressed_size } else { pointer(file, 1).unwrap_or(0) & !UNCOMPRESSED };
    let first_offset = first & !UNCOMPRESSED;
    let encoded_size = next.checked_sub(first_offset).filter(|&s| s > 0 && s <= u64::from(block_size) + 64)?;
    let data_offset = 0x20 + u64::from(block_count) * 12;
    let encoded = read_exact_at(file, data_offset + first_offset, encoded_size as usize)?;
    let decoded = if first & UNCOMPRESSED != 0 {
        encoded
    } else {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(encoded.as_slice()).take(u64::from(block_size)).read_to_end(&mut out).ok()?;
        out
    };
    (decoded.len() == block_size as usize).then(|| decoded[..DISC_HEADER_SIZE].to_vec())
}

/// readDolphinDiscHeader: the 0x20-byte disc header behind each container format.
pub fn read_dolphin_disc_header(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let ext = ext_of(&path.file_name()?.to_string_lossy());
    let raw = |file: &mut File, offset: u64| read_exact_at(file, offset, DISC_HEADER_SIZE);
    match ext.as_str() {
        ".wia" | ".rvz" => raw(&mut file, 0x58),
        ".tgc" => {
            let header = read_exact_at(&mut file, 0, 0x0c)?;
            raw(&mut file, u64::from(be32(&header, 0x08)))
        }
        ".wbfs" => {
            let header = read_exact_at(&mut file, 0, 0x0d)?;
            let (shift, first_sector) = (header[0x09], header[0x0c]);
            if &header[..4] != b"WBFS" || !(9..=31).contains(&shift) || first_sector == 0 {
                return None;
            }
            raw(&mut file, u64::from(first_sector) << shift)
        }
        ".ciso" => {
            // Dolphin CISO is sparse, not compressed: block zero sits right after the 0x8000 map.
            let header = read_exact_at(&mut file, 0, 9)?;
            if &header[..4] != b"CISO" || !valid_block_size(le32(&header, 4)) || header[8] != 1 {
                return None;
            }
            raw(&mut file, 0x8000)
        }
        ".gcz" => gcz_header(&mut file),
        _ => raw(&mut file, 0),
    }
}

fn dolphin_game_id(path: &Path) -> Option<String> {
    if ext_of(&path.file_name()?.to_string_lossy()) == ".wad" {
        return None;
    }
    let header = read_dolphin_disc_header(path)?;
    let id = &header[..6];
    id.iter().all(u8::is_ascii_alphanumeric).then(|| String::from_utf8_lossy(id).to_uppercase())
}

// ---- SKU extraction ---------------------------------------------------------

/// sku-normalize `normalize`: `SLUS_213.76` -> `SLUS-21376`.
pub fn normalize(raw: &str) -> String {
    let stripped: String = raw.to_uppercase().chars().filter(char::is_ascii_alphanumeric).collect();
    let letters = stripped.get(..4).unwrap_or_default();
    let digits = stripped.get(4..).unwrap_or_default();
    if letters.len() == 4 && letters.chars().all(|c| c.is_ascii_uppercase()) && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        format!("{letters}-{digits}")
    } else {
        stripped
    }
}

/// launchbox-shop-details `normalizeSku`: the lookup key, no dash.
pub fn lookup_key(raw: &str) -> String {
    raw.to_uppercase().chars().filter(char::is_ascii_alphanumeric).collect()
}

static BOOT_SKU_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)BOOT2?\s*=\s*cdrom0?:\\?([A-Z]{4}[_\-.\s]?\d{2,3}[_\-.\s]?\d{2,3})").unwrap());
static ISO_FILENAME_SKU_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)([A-Z]{4})_(\d{2,3})\.(\d{2,3});1").unwrap());
static PSP_DISC_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)[A-Z]{4}[-_ ]?\d{5}").unwrap());
static TITLE_ID_GUESS_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[A-Z]{4}\d{5}").unwrap());

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// scanBuffersForRawSku over a stream of chunks, keeping a 128-byte tail across chunk edges.
struct SkuScanner {
    tail: Vec<u8>,
    iso_fallback: Option<String>,
}

impl SkuScanner {
    fn new() -> Self {
        Self { tail: Vec::new(), iso_fallback: None }
    }

    /// The SYSTEM.CNF BOOT line, normalized, as soon as it is seen.
    fn feed(&mut self, chunk: &[u8]) -> Option<String> {
        let mut text = std::mem::take(&mut self.tail);
        text.extend_from_slice(chunk);
        if let Some(c) = BOOT_SKU_RE.captures(&text) {
            return Some(normalize(&latin1(&c[1])));
        }
        if self.iso_fallback.is_none() {
            if let Some(c) = ISO_FILENAME_SKU_RE.captures(&text) {
                self.iso_fallback = Some(normalize(&format!("{}_{}.{}", latin1(&c[1]), latin1(&c[2]), latin1(&c[3]))));
            }
        }
        self.tail = text[text.len().saturating_sub(128)..].to_vec();
        None
    }

    fn scan(chunks: impl IntoIterator<Item = Vec<u8>>) -> Option<String> {
        let mut scanner = Self::new();
        for chunk in chunks {
            if let Some(sku) = scanner.feed(&chunk) {
                return Some(sku);
            }
        }
        scanner.iso_fallback
    }
}

const LEADING_LIMIT: usize = 16 * 1024 * 1024;
const MAX_BLOCKS_SCANNED: u32 = 24_000;

fn inflate_cso_block(src: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if flate2::read::DeflateDecoder::new(src).read_to_end(&mut out).is_ok() {
        return Some(out);
    }
    out.clear();
    flate2::read::ZlibDecoder::new(src).read_to_end(&mut out).ok().map(|_| out)
}

/// readCsoLeadingData: the first 16 MiB of a CSO v0/v1 image, block by block.
fn cso_leading_chunks(path: &Path) -> Option<Vec<Vec<u8>>> {
    let mut file = File::open(path).ok()?;
    let header = read_exact_at(&mut file, 0, 24)?;
    let (total, block_size, version, shift) = (le64(&header, 8), le32(&header, 16), header[20], header[21]);
    if &header[..4] != b"CISO" || block_size == 0 || total == 0 || version > 1 {
        return None;
    }
    let blocks = total.div_ceil(u64::from(block_size)).min(u64::from(MAX_BLOCKS_SCANNED)) as usize;
    let index = read_exact_at(&mut file, 24, (blocks + 1) * 4)?;
    let (mut chunks, mut decoded) = (Vec::new(), 0);
    for b in 0..blocks {
        if decoded >= LEADING_LIMIT {
            break;
        }
        let (entry, next) = (le32(&index, b * 4), le32(&index, (b + 1) * 4));
        let start = u64::from(entry & 0x7fff_ffff) << shift;
        let end = u64::from(next & 0x7fff_ffff) << shift;
        if end <= start {
            continue;
        }
        let Some(raw) = read_up_to(&mut file, start, (end - start) as usize) else { continue };
        let chunk = if entry & 0x8000_0000 != 0 { Some(raw[..raw.len().min(block_size as usize)].to_vec()) } else { inflate_cso_block(&raw) };
        if let Some(chunk) = chunk.filter(|c| !c.is_empty()) {
            decoded += chunk.len();
            chunks.push(chunk);
        }
    }
    (!chunks.is_empty()).then_some(chunks)
}

/// readChdLeadingData: the first 16 MiB of hunks. Hunks that fail to decode
/// (FLAC audio tracks, for one) are skipped, not fatal.
fn chd_leading_chunks(path: &Path) -> Option<Vec<Vec<u8>>> {
    let mut chd = chd::Chd::open(BufReader::new(File::open(path).ok()?), None).ok()?;
    let count = chd.header().hunk_count().min(MAX_BLOCKS_SCANNED);
    let (mut compressed, mut chunks, mut decoded) = (Vec::new(), Vec::new(), 0);
    for h in 0..count {
        if decoded >= LEADING_LIMIT {
            break;
        }
        let mut out = chd.get_hunksized_buffer();
        let ok = chd.hunk(h).and_then(|mut hunk| hunk.read_hunk_in(&mut compressed, &mut out)).is_ok();
        if ok {
            decoded += out.len();
            chunks.push(out);
        }
    }
    (!chunks.is_empty()).then_some(chunks)
}

const SKU_CHUNK: usize = 1024 * 1024;
const SKU_SCAN_LIMIT: u64 = 64 * 1024 * 1024;

fn scan_file_for_sku(target: &Path) -> Option<String> {
    let mut file = File::open(target).ok()?;
    let mut scanner = SkuScanner::new();
    let mut offset = 0u64;
    while offset < SKU_SCAN_LIMIT {
        let chunk = read_up_to(&mut file, offset, SKU_CHUNK)?;
        if chunk.is_empty() {
            break;
        }
        if let Some(sku) = scanner.feed(&chunk) {
            return Some(sku);
        }
        offset += chunk.len() as u64;
    }
    scanner.iso_fallback
}

fn ps12_sku(path: &Path) -> Option<String> {
    let ext = ext_of(&path.file_name()?.to_string_lossy());
    match ext.as_str() {
        ".chd" => SkuScanner::scan(chd_leading_chunks(path)?),
        ".cso" => SkuScanner::scan(cso_leading_chunks(path)?),
        _ => resolve_sniff_target(path).filter(|t| t.exists()).and_then(|t| scan_file_for_sku(&t)),
    }
}

/// parseParamSfoValue: a string entry of a PARAM.SFO, normalized.
pub fn param_sfo_value(data: &[u8], wanted: &str) -> Option<String> {
    if data.len() < 20 || le32(data, 0) != 0x4653_5000 {
        return None;
    }
    let (key_table, data_table, entries) = (le32(data, 8) as usize, le32(data, 12) as usize, le32(data, 16) as usize);
    for i in 0..entries {
        let off = 20 + i * 16;
        if off + 16 > data.len() {
            break;
        }
        let key_offset = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
        let (used, value_offset) = (le32(data, off + 4) as usize, le32(data, off + 12) as usize);
        let key_start = (key_table + key_offset).min(data.len());
        let key_end = data[key_start..].iter().position(|&b| b == 0).map_or(data.len(), |p| key_start + p);
        if &data[key_start..key_end] != wanted.as_bytes() {
            continue;
        }
        let start = data_table + value_offset;
        let value = data.get(start..start + used)?;
        let value = &value[..value.iter().position(|&b| b == 0).unwrap_or(value.len())];
        let value = latin1(value);
        let value = value.trim();
        return (!value.is_empty()).then(|| normalize(value));
    }
    None
}

const ISO_SECTOR: u64 = 2048;

struct IsoEntry {
    name: String,
    lba: u32,
    size: u32,
    is_dir: bool,
}

fn iso_dir_records(buf: &[u8]) -> Vec<IsoEntry> {
    let (mut entries, mut pos) = (Vec::new(), 0usize);
    while pos < buf.len() {
        let len = buf[pos] as usize;
        if len == 0 {
            // Records never cross a sector edge; zero padding means "next sector".
            pos = (pos / ISO_SECTOR as usize + 1) * ISO_SECTOR as usize;
            continue;
        }
        if pos + len > buf.len() || pos + 33 > buf.len() {
            break;
        }
        let name_len = buf[pos + 32] as usize;
        let raw = &buf[pos + 33..(pos + 33 + name_len).min(buf.len())];
        let name = match raw {
            [0] => ".".to_string(),
            [1] => "..".to_string(),
            _ => latin1(raw).split(';').next().unwrap_or_default().to_string(),
        };
        entries.push(IsoEntry { name, lba: le32(buf, pos + 2), size: le32(buf, pos + 10), is_dir: buf[pos + 25] & 0x02 != 0 });
        pos += len;
    }
    entries
}

fn find_iso_entry<'a>(entries: &'a [IsoEntry], name: &str) -> Option<&'a IsoEntry> {
    entries.iter().find(|e| e.name != "." && e.name != ".." && e.name.eq_ignore_ascii_case(name))
}

/// extractTitleIdFromIso: PS3_GAME/PARAM.SFO through the ISO9660 tree.
pub fn ps3_iso_title_id(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let pvd = read_up_to(&mut file, 16 * ISO_SECTOR, ISO_SECTOR as usize)?;
    if pvd.len() < 190 || pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return None;
    }
    let mut extent = |lba: u32, size: u32, cap: u32| read_up_to(&mut file, u64::from(lba) * ISO_SECTOR, size.min(cap) as usize);
    let root = extent(le32(&pvd, 158), le32(&pvd, 166), 256 * 1024)?;
    let root_entries = iso_dir_records(&root);
    let game = find_iso_entry(&root_entries, "PS3_GAME").filter(|e| e.is_dir)?;
    let game_dir = extent(game.lba, game.size, 256 * 1024)?;
    let game_entries = iso_dir_records(&game_dir);
    let sfo = find_iso_entry(&game_entries, "PARAM.SFO").filter(|e| !e.is_dir)?;
    param_sfo_value(&extent(sfo.lba, sfo.size, 1024 * 1024)?, "TITLE_ID")
}

pub(crate) fn ps3_pkg_title_id(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let head = read_up_to(&mut file, 0, 0x80)?;
    if head.len() < 0x54 || be32(&head, 0) != 0x7f50_4b47 {
        return None;
    }
    let content_id = latin1(&head[0x30..0x30 + 36]);
    let wider = latin1(&head);
    TITLE_ID_GUESS_RE.find(&content_id).or_else(|| TITLE_ID_GUESS_RE.find(&wider)).map(|m| normalize(m.as_str()))
}

fn guess_from_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    TITLE_ID_GUESS_RE.find(&name).map(|m| normalize(m.as_str()))
}

fn ps3_title_id(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() {
        return [path.join("PARAM.SFO"), path.join("PS3_GAME").join("PARAM.SFO")]
            .iter()
            .find_map(|sfo| std::fs::read(sfo).ok().and_then(|d| param_sfo_value(&d, "TITLE_ID")))
            .or_else(|| guess_from_name(path));
    }
    let ext = ext_of(&path.file_name()?.to_string_lossy());
    let from_media = match ext.as_str() {
        ".iso" => ps3_iso_title_id(path),
        ".pkg" => ps3_pkg_title_id(path),
        _ => None,
    };
    from_media.or_else(|| guess_from_name(path)).or_else(|| path.parent().and_then(guess_from_name))
}

fn psp_disc_id_in(chunks: &[Vec<u8>]) -> Option<String> {
    let mut tail: Vec<u8> = Vec::new();
    for chunk in chunks {
        let mut text = std::mem::take(&mut tail);
        text.extend_from_slice(chunk);
        if let Some(m) = PSP_DISC_ID_RE.find(&text) {
            return Some(normalize(&latin1(m.as_bytes())));
        }
        tail = text[text.len().saturating_sub(16)..].to_vec();
    }
    None
}

fn pbp_disc_id(path: &Path, file_size: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let header = read_exact_at(&mut file, 0, 0x28)?;
    if header[..4] != [0, b'P', b'B', b'P'] {
        return None;
    }
    let start = u64::from(le32(&header, 0x08));
    let end = u64::from(le32(&header, 0x0c)).min(start + 1024 * 1024);
    if start >= end || end > file_size {
        return None;
    }
    param_sfo_value(&read_up_to(&mut file, start, (end - start) as usize)?, "DISC_ID")
}

fn psp_disc_id(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() {
        return [path.join("PSP_GAME").join("PARAM.SFO"), path.join("PARAM.SFO")]
            .iter()
            .find_map(|sfo| std::fs::read(sfo).ok().and_then(|d| param_sfo_value(&d, "DISC_ID")));
    }
    let ext = ext_of(&path.file_name()?.to_string_lossy());
    let id = match ext.as_str() {
        ".pbp" => pbp_disc_id(path, meta.len()),
        ".cso" => cso_leading_chunks(path).and_then(|c| psp_disc_id_in(&c)),
        ".chd" => chd_leading_chunks(path).and_then(|c| psp_disc_id_in(&c)),
        _ => File::open(path).ok().and_then(|mut f| read_up_to(&mut f, 0, SNIFF_BYTES)).and_then(|d| PSP_DISC_ID_RE.find(&d).map(|m| normalize(&latin1(m.as_bytes())))),
    };
    id.or_else(|| {
        let name = path.file_name()?.to_string_lossy();
        PSP_DISC_ID_RE.find(name.as_bytes()).map(|m| normalize(&latin1(m.as_bytes())))
    })
}

/// extractDiscSku: the serial the LaunchBox catalogue knows the disc by.
pub fn extract_disc_sku(path: &Path, system: &str) -> Option<String> {
    match system {
        "ps1" | "ps2" => ps12_sku(path),
        "ps3" => ps3_title_id(path),
        "psp" => psp_disc_id(path),
        "dolphin" => dolphin_game_id(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-romscan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn normalizes_skus_like_electron() {
        assert_eq!(normalize("SLUS_213.76"), "SLUS-21376");
        assert_eq!(normalize("sces-50361"), "SCES-50361");
        // Dolphin ids skip normalize; it would dash them too.
        assert_eq!(normalize("GALE01"), "GALE-01");
        assert_eq!(normalize("not a sku"), "NOTASKU");
        assert_eq!(lookup_key("SLUS-21376"), "SLUS21376");
    }

    #[test]
    fn finds_the_boot_line_across_chunk_edges() {
        let line = b"BOOT2 = cdrom0:\\SLUS_213.76;1\r\n";
        let (a, b) = line.split_at(12);
        assert_eq!(SkuScanner::scan([a.to_vec(), b.to_vec()]), Some("SLUS-21376".into()));
        assert_eq!(SkuScanner::scan([b"xx SCUS_944.55;1 yy".to_vec()]), Some("SCUS-94455".into()));
        assert_eq!(SkuScanner::scan([b"nothing here".to_vec()]), None);
    }

    #[test]
    fn classifies_playstation_discs() {
        assert_eq!(classify_playstation(b"BOOT2 = cdrom0:\\SLUS_200.00;1"), Platform::Ps2);
        assert_eq!(classify_playstation(b"BOOT = cdrom:\\SCUS_944.55;1"), Platform::Ps1);
        assert_eq!(classify_playstation(b"PS3_GAME ... PARAM.SFO"), Platform::Ps3);
        assert_eq!(classify_playstation(b"random"), Platform::Unknown);
    }

    #[test]
    fn reads_param_sfo_entries() {
        // One entry: key "TITLE_ID", value "BLUS30443".
        let mut sfo = vec![0u8; 20 + 16];
        sfo[..4].copy_from_slice(&0x4653_5000u32.to_le_bytes());
        let key_table = sfo.len() as u32;
        sfo.extend_from_slice(b"TITLE_ID\0\0\0\0");
        let data_table = sfo.len() as u32;
        sfo.extend_from_slice(b"BLUS30443\0\0\0\0\0\0\0");
        sfo[8..12].copy_from_slice(&key_table.to_le_bytes());
        sfo[12..16].copy_from_slice(&data_table.to_le_bytes());
        sfo[16..20].copy_from_slice(&1u32.to_le_bytes());
        sfo[24..28].copy_from_slice(&10u32.to_le_bytes());
        assert_eq!(param_sfo_value(&sfo, "TITLE_ID"), Some("BLUS-30443".into()));
        assert_eq!(param_sfo_value(&sfo, "DISC_ID"), None);
    }

    #[test]
    fn groups_cue_sheets_and_playlists() {
        let dir = temp_dir();
        for name in ["Game (Track 1).bin", "Game.cue", "Game.bin", "Other.chd", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::write(dir.join("Game.cue"), "FILE \"Game.bin\" BINARY\n").unwrap();
        let none = AtomicBool::new(false);
        let mut names: Vec<String> = scan_rom_folder(&dir, "ps1", false, &none, &mut |_, _, _, _| {}).unwrap().games.into_iter().map(|g| g.name).collect();
        names.sort();
        // "Game (Track 1).bin" has no same-named cue, so it stands alone.
        assert_eq!(names, ["Game (Track 1).bin", "Game.cue", "Other.chd"]);
        std::fs::write(dir.join("Game.m3u"), "Game.cue").unwrap();
        let names: Vec<String> = scan_rom_folder(&dir, "ps1", false, &none, &mut |_, _, _, _| {}).unwrap().games.into_iter().map(|g| g.name).collect();
        assert_eq!(names, ["Game.m3u"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_discs_of_the_wrong_platform_and_reads_their_sku() {
        let dir = temp_dir();
        let mut iso = vec![0u8; 4096];
        iso[100..129].copy_from_slice(b"BOOT2 = cdrom0:\\SLES_123.45;1");
        std::fs::write(dir.join("ps2game.iso"), &iso).unwrap();
        let none = AtomicBool::new(false);
        let ps1 = scan_rom_folder(&dir, "ps1", false, &none, &mut |_, _, _, _| {}).unwrap();
        assert!(ps1.games[0].wrong_platform);
        assert_eq!(ps1.file_count, 0);
        let ps2 = scan_rom_folder(&dir, "ps2", false, &none, &mut |_, _, _, _| {}).unwrap();
        assert!(!ps2.games[0].wrong_platform);
        assert_eq!(ps2.size_bytes, 4096);
        assert_eq!(extract_disc_sku(&dir.join("ps2game.iso"), "ps2"), Some("SLES-12345".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_gamecube_ids_and_ps3_folders() {
        let dir = temp_dir();
        let mut gcm = vec![0u8; 0x40];
        gcm[..6].copy_from_slice(b"GALE01");
        gcm[0x1c..0x20].copy_from_slice(&0xc233_9f3du32.to_be_bytes());
        std::fs::write(dir.join("melee.iso"), &gcm).unwrap();
        assert_eq!(sniff_disc_image(&dir.join("melee.iso")), Platform::GameCube);
        assert_eq!(extract_disc_sku(&dir.join("melee.iso"), "dolphin"), Some("GALE01".into()));
        let game = dir.join("Demon's Souls [BLUS30443]").join("PS3_GAME");
        std::fs::create_dir_all(&game).unwrap();
        let none = AtomicBool::new(false);
        let found = scan_rom_folder(&dir, "ps3", true, &none, &mut |_, _, _, _| {}).unwrap();
        // melee.iso sniffs as GameCube: listed, but as the wrong platform.
        assert_eq!(found.games.len(), 2);
        assert_eq!(found.games.iter().filter(|g| !g.wrong_platform).map(|g| g.name.as_str()).collect::<Vec<_>>(), ["PS3_GAME"]);
        // No PARAM.SFO: the folder name is the fallback.
        assert_eq!(extract_disc_sku(game.parent().unwrap(), "ps3"), Some("BLUS-30443".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
