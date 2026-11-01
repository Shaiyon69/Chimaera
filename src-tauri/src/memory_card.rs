//! PS1 and PS2 memory card readers (port of `ps1-memory-card/` and `ps2-memory-card/`:
//! the parsers, `extract-save-sku.ts`, and the `.mcs`/`.psu` export builders).
//! Read-only: nothing here writes to a card.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// One save as the scan stores it. PS1 has no timestamps (0).
#[derive(Debug, Clone, PartialEq)]
pub struct Save {
    pub folder: String,
    pub sku: Option<String>,
    pub file_count: u64,
    pub size: u64,
    pub created_ms: i64,
    pub modified_ms: i64,
}

#[derive(Debug, PartialEq)]
pub enum FormatState {
    Formatted,
    Unformatted,
    Unreadable,
}

impl FormatState {
    pub fn as_str(&self) -> &'static str {
        match self {
            FormatState::Formatted => "formatted",
            FormatState::Unformatted => "unformatted",
            FormatState::Unreadable => "unreadable",
        }
    }
}

/// Reads `len` bytes at `offset`; past the end of the file is zeros (like Node's fh.read into Buffer.alloc).
fn read_at(file: &mut File, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    file.seek(SeekFrom::Start(offset))?;
    let mut filled = 0;
    while filled < len {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(buf)
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn latin1(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end].iter().map(|&b| b as char).collect()
}

// ---- save-folder SKU ---------------------------------------------------------

const KNOWN_PREFIXES: &[&str] = &[
    "SLUS", "SLES", "SLPS", "SLPM", "SCUS", "SCES", "SCPS", "SCAJ", "SLAJ", "SLKA", "SCKA", "TCPS", "PBPX", "CPCS", "PAPX", "TLES", "TLUS",
    "TCES", "ALCH", "HAKU", "SLED", "SCED", "SIPS", "ESPM",
];

/// isSystemSaveFolder: BIOS, system, network and PocketStation folders.
pub fn is_system_folder(name: &str) -> bool {
    let u = name.trim().to_uppercase();
    u.is_empty()
        || u == "."
        || u == ".."
        || u.ends_with("-SYSTEM")
        || u.starts_with("BWNETCNF")
        || u == "BIOS"
        || u.starts_with("BOOT")
        || ["BADATA", "BEDATA", "BIDATA", "BJDATA", "POCKETSTN"].iter().any(|p| u.starts_with(p))
}

/// extractSkuFromSaveFolder: `BASLUS-20294USER` -> `SLUS-20294`.
pub fn sku_from_folder(name: &str) -> Option<String> {
    let u = name.trim().to_uppercase();
    if is_system_folder(&u) {
        return None;
    }
    let serial = |s: &str| -> Option<(String, String)> {
        let b = s.as_bytes();
        if b.len() < 9 || !b[..4].iter().all(u8::is_ascii_uppercase) {
            return None;
        }
        let digits = if b[4].is_ascii_digit() { 4 } else if matches!(b[4], b'-' | b'_' | b' ' | b'.') { 5 } else { return None };
        let d = b.get(digits..digits + 5)?;
        d.iter().all(u8::is_ascii_digit).then(|| (s[..4].to_string(), s[digits..digits + 5].to_string()))
    };
    // Drop a leading B<region> only when a serial follows it.
    let body = if u.len() > 2 && u.as_bytes()[0] == b'B' && u.as_bytes()[1].is_ascii_uppercase() && serial(&u[2..]).is_some() { &u[2..] } else { &u[..] };
    let (prefix, digits) = serial(body)?;
    KNOWN_PREFIXES.contains(&prefix.as_str()).then(|| format!("{prefix}-{digits}"))
}

// ---- PS1 ---------------------------------------------------------------------

const PS1_FRAME: usize = 128;
const PS1_BLOCK: usize = 8192;
const PS1_DATA_BLOCKS: usize = 15;
const PS1_HEADER_OFFSETS: [usize; 4] = [0, 64, 128, 3904];

fn ps1_data_offset(probe: &[u8], size: u64) -> Option<usize> {
    PS1_HEADER_OFFSETS.into_iter().find(|&off| (off + PS1_BLOCK) as u64 <= size && probe.get(off..off + 2) == Some(b"MC"))
}

struct Ps1Card {
    file: File,
    offset: usize,
    directory: Vec<u8>,
}

impl Ps1Card {
    fn open(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        if size < PS1_BLOCK as u64 {
            return None;
        }
        let probe = read_at(&mut file, 0, (size as usize).min(PS1_HEADER_OFFSETS[3] + PS1_BLOCK)).ok()?;
        let offset = ps1_data_offset(&probe, size)?;
        let directory = read_at(&mut file, offset as u64, PS1_BLOCK).ok()?;
        Some(Self { file, offset, directory })
    }

    fn frame(&self, block: usize) -> &[u8] {
        &self.directory[block * PS1_FRAME..(block + 1) * PS1_FRAME]
    }

    /// The first blocks of saves in use, with their identifiers.
    fn firsts(&self) -> impl Iterator<Item = (usize, String)> + '_ {
        (1..=PS1_DATA_BLOCKS).filter_map(|b| {
            let frame = self.frame(b);
            let name = latin1(&frame[0x0a..0x0a + 20]).trim().to_string();
            (u32_at(frame, 0) & 0xff == 0x51 && !name.is_empty()).then_some((b, name))
        })
    }

    /// The link chain from a first block, or the contiguous layout when it is corrupt.
    fn chain(&self, first: usize) -> Vec<usize> {
        let size = u32_at(self.frame(first), 4) as usize;
        let expected = size.div_ceil(PS1_BLOCK).clamp(1, PS1_DATA_BLOCKS);
        let mut chain = vec![first];
        let mut cur = first;
        for _ in 0..PS1_DATA_BLOCKS {
            let link = u16_at(self.frame(cur), 8);
            if link == 0xffff {
                break;
            }
            let next = link as usize + 1;
            if !(1..=PS1_DATA_BLOCKS).contains(&next) || chain.contains(&next) {
                break;
            }
            chain.push(next);
            cur = next;
        }
        if chain.len() != expected {
            return (first..=PS1_DATA_BLOCKS).take(expected).collect();
        }
        chain
    }
}

pub fn inspect_ps1(path: &Path) -> FormatState {
    let Ok(mut file) = File::open(path) else { return FormatState::Unreadable };
    let Ok(size) = file.metadata().map(|m| m.len()) else { return FormatState::Unreadable };
    if size < PS1_BLOCK as u64 {
        return FormatState::Unformatted;
    }
    match read_at(&mut file, 0, (size as usize).min(PS1_HEADER_OFFSETS[3] + PS1_BLOCK)) {
        Ok(probe) if ps1_data_offset(&probe, size).is_some() => FormatState::Formatted,
        Ok(_) => FormatState::Unformatted,
        Err(_) => FormatState::Unreadable,
    }
}

/// listPs1Saves; None when the file is not a PS1 card.
pub fn list_ps1(path: &Path) -> Option<Vec<Save>> {
    let card = Ps1Card::open(path)?;
    Some(
        card.firsts()
            .map(|(block, name)| Save {
                sku: sku_from_folder(&name),
                file_count: card.chain(block).len() as u64,
                size: u32_at(card.frame(block), 4) as u64,
                folder: name,
                created_ms: 0,
                modified_ms: 0,
            })
            .collect(),
    )
}

/// buildMcsBuffer over readPs1SaveContents: the directory frame, then the blocks in chain order.
pub fn ps1_mcs(path: &Path, identifier: &str) -> Option<Vec<u8>> {
    let mut card = Ps1Card::open(path)?;
    let (first, _) = card.firsts().find(|(_, name)| name == identifier)?;
    let mut out = card.frame(first).to_vec();
    for block in card.chain(first) {
        let offset = (card.offset + block * PS1_BLOCK) as u64;
        out.extend(read_at(&mut card.file, offset, PS1_BLOCK).ok()?);
    }
    Some(out)
}

// ---- PS2 ---------------------------------------------------------------------

const PS2_MAGIC: &[u8] = b"Sony PS2 Memory Card Format ";
const PS2_SUPERBLOCK: usize = 0x154;
const DIR_ENTRY: usize = 512;
const FAT_ALLOCATED: u32 = 0x8000_0000;
const FAT_MASK: u32 = 0x7fff_ffff;
const MAX_CHAIN_STEPS: usize = 8192;
const MAX_DIR_ENTRIES: usize = 8192;
const MAX_SAVE_FILE: usize = 16 * 1024 * 1024;
const DF_FILE: u16 = 0x0010;
const DF_DIR: u16 = 0x0020;
const DF_EXISTS: u16 = 0x8000;

struct Superblock {
    page_len: usize,
    pages_per_cluster: usize,
    clusters_per_card: u32,
    alloc_offset: u32,
    root: u32,
    ifc: [u32; 32],
}

impl Superblock {
    fn cluster_size(&self) -> usize {
        self.page_len * self.pages_per_cluster
    }
}

fn read_superblock(file: &mut File) -> Option<Superblock> {
    let b = read_at(file, 0, PS2_SUPERBLOCK).ok()?;
    if file.metadata().ok()?.len() < PS2_SUPERBLOCK as u64 || !b.starts_with(PS2_MAGIC) {
        return None;
    }
    let (page_len, pages_per_cluster) = (u16_at(&b, 0x28) as usize, u16_at(&b, 0x2a) as usize);
    if page_len == 0 || pages_per_cluster == 0 {
        return None;
    }
    let mut ifc = [0u32; 32];
    for (i, slot) in ifc.iter_mut().enumerate() {
        *slot = u32_at(&b, 0x50 + i * 4);
    }
    Some(Superblock { page_len, pages_per_cluster, clusters_per_card: u32_at(&b, 0x30), alloc_offset: u32_at(&b, 0x34), root: u32_at(&b, 0x3c), ifc })
}

/// detectEcc: the on-disk page stride, with or without the ECC spare area.
fn raw_page_size(size: u64, sb: &Superblock) -> Option<u64> {
    let page = sb.page_len as u64;
    let ecc = page + page / 128 * 4;
    let pages = sb.clusters_per_card as u64 * sb.pages_per_cluster as u64;
    if pages > 0 && size == pages * ecc {
        return Some(ecc);
    }
    if pages > 0 && size == pages * page {
        return Some(page);
    }
    if size % ecc == 0 && size % page != 0 {
        return Some(ecc);
    }
    (size % page == 0).then_some(page)
}

/// ToD `[pad, sec, min, hour, day, month, year u16]` to epoch seconds; 0 when out of range.
fn parse_tod(raw: &[u8]) -> i64 {
    let (sec, min, hour, day, month) = (raw[1] as i64, raw[2] as i64, raw[3] as i64, raw[4] as i64, raw[5] as i64);
    let year = u16_at(raw, 6) as i64;
    if !(1980..=3000).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return 0;
    }
    crate::http_download::days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec
}

struct DirEntry {
    mode: u16,
    length: u32,
    cluster: u32,
    name: String,
    created: [u8; 8],
    modified: [u8; 8],
}

impl DirEntry {
    fn parse(b: &[u8]) -> Self {
        DirEntry {
            mode: u16_at(b, 0),
            length: u32_at(b, 4),
            cluster: u32_at(b, 0x10),
            name: latin1(&b[0x40..0x60]),
            created: b[0x08..0x10].try_into().unwrap(),
            modified: b[0x18..0x20].try_into().unwrap(),
        }
    }
    fn exists(&self) -> bool {
        self.mode & DF_EXISTS != 0
    }
    fn is_dir(&self) -> bool {
        self.mode & DF_DIR != 0
    }
    fn is_save_file(&self) -> bool {
        self.exists() && self.mode & DF_FILE != 0 && self.name != "." && self.name != ".."
    }
}

struct Ps2Card {
    file: File,
    sb: Superblock,
    raw_page: u64,
    fat_cache: HashMap<u32, Vec<u8>>,
}

impl Ps2Card {
    fn open(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let sb = read_superblock(&mut file)?;
        let raw_page = raw_page_size(file.metadata().ok()?.len(), &sb)?;
        Some(Self { file, sb, raw_page, fat_cache: HashMap::new() })
    }

    fn cluster(&mut self, absolute: u32) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(self.sb.cluster_size());
        for p in 0..self.sb.pages_per_cluster as u64 {
            let page = absolute as u64 * self.sb.pages_per_cluster as u64 + p;
            out.extend(read_at(&mut self.file, page * self.raw_page, self.sb.page_len)?);
        }
        Ok(out)
    }

    fn cached(&mut self, absolute: u32) -> std::io::Result<&[u8]> {
        if !self.fat_cache.contains_key(&absolute) {
            let c = self.cluster(absolute)?;
            self.fat_cache.insert(absolute, c);
        }
        Ok(&self.fat_cache[&absolute])
    }

    /// The FAT entry of a relative cluster, through the indirect FAT.
    fn fat_entry(&mut self, relative: u32) -> std::io::Result<u32> {
        let per = (self.sb.cluster_size() / 4) as u32;
        let (slot, selector) = (relative % per, relative / per);
        let Some(&indirect) = self.sb.ifc.get((selector / per) as usize) else { return Ok(0) };
        let fat = u32_at(self.cached(indirect)?, (selector % per) as usize * 4);
        Ok(u32_at(self.cached(fat)?, slot as usize * 4))
    }

    fn chain(&mut self, start: u32, max: usize) -> std::io::Result<Vec<u32>> {
        let (mut out, mut cur) = (Vec::new(), start);
        for _ in 0..MAX_CHAIN_STEPS {
            if out.len() >= max || cur >= self.sb.clusters_per_card {
                break;
            }
            let entry = self.fat_entry(cur)?;
            if entry & FAT_ALLOCATED == 0 {
                break;
            }
            out.push(cur);
            let next = entry & FAT_MASK;
            if next == FAT_MASK || next == cur {
                break;
            }
            cur = next;
        }
        Ok(out)
    }

    fn chain_bytes(&mut self, start: u32, length: usize) -> std::io::Result<Vec<u8>> {
        let effective = length.min(MAX_SAVE_FILE);
        if effective == 0 {
            return Ok(Vec::new());
        }
        let size = self.sb.cluster_size();
        let mut out = Vec::with_capacity(effective);
        for rel in self.chain(start, effective.div_ceil(size) + 1)? {
            if out.len() >= effective {
                break;
            }
            let cluster = self.cluster(self.sb.alloc_offset + rel)?;
            out.extend_from_slice(&cluster[..size.min(effective - out.len())]);
        }
        Ok(out)
    }

    fn directory(&mut self, start: u32, count: u32) -> std::io::Result<Vec<DirEntry>> {
        let per = self.sb.cluster_size() / DIR_ENTRY;
        if per == 0 {
            return Ok(Vec::new());
        }
        let want = (count as usize).min(MAX_DIR_ENTRIES);
        let mut entries = Vec::new();
        for rel in self.chain(start, want.div_ceil(per))? {
            let cluster = self.cluster(self.sb.alloc_offset + rel)?;
            for chunk in cluster.chunks_exact(DIR_ENTRY) {
                if entries.len() >= want {
                    break;
                }
                entries.push(DirEntry::parse(chunk));
            }
            if entries.len() >= want {
                break;
            }
        }
        Ok(entries)
    }

    /// The root's entry count lives in its own "." entry.
    fn root(&mut self) -> std::io::Result<Vec<DirEntry>> {
        let first = self.cluster(self.sb.alloc_offset + self.sb.root)?;
        let dot = DirEntry::parse(&first[..DIR_ENTRY]);
        self.directory(self.sb.root, dot.length)
    }
}

pub fn inspect_ps2(path: &Path) -> FormatState {
    match File::open(path) {
        Ok(mut file) => if read_superblock(&mut file).is_some() { FormatState::Formatted } else { FormatState::Unformatted },
        Err(_) => FormatState::Unreadable,
    }
}

/// listSaves; None when the file is not a readable PS2 card.
pub fn list_ps2(path: &Path) -> Option<Vec<Save>> {
    let mut card = Ps2Card::open(path)?;
    let mut saves = Vec::new();
    for entry in card.root().ok()? {
        if !entry.exists() || !entry.is_dir() || is_system_folder(&entry.name) {
            continue;
        }
        let files: Vec<DirEntry> = card.directory(entry.cluster, entry.length).ok()?.into_iter().filter(DirEntry::is_save_file).collect();
        saves.push(Save {
            sku: sku_from_folder(&entry.name),
            file_count: files.len() as u64,
            size: files.iter().map(|f| f.length as u64).sum(),
            created_ms: parse_tod(&entry.created) * 1000,
            modified_ms: parse_tod(&entry.modified) * 1000,
            folder: entry.name,
        });
    }
    Some(saves)
}

fn psu_record(mode: u16, length: u32, name: &str, created: &[u8; 8], modified: &[u8; 8]) -> Vec<u8> {
    let mut rec = vec![0u8; DIR_ENTRY];
    rec[0..2].copy_from_slice(&mode.to_le_bytes());
    rec[4..8].copy_from_slice(&length.to_le_bytes());
    rec[0x08..0x10].copy_from_slice(created);
    rec[0x18..0x20].copy_from_slice(modified);
    let name: Vec<u8> = name.chars().map(|c| c as u32 as u8).take(32).collect();
    rec[0x40..0x40 + name.len()].copy_from_slice(&name);
    rec
}

/// buildPsuBuffer over readSaveContents: folder, ".", "..", then each file padded to 1024.
pub fn ps2_psu(path: &Path, folder_name: &str) -> Option<Vec<u8>> {
    let mut card = Ps2Card::open(path)?;
    let folder = card.root().ok()?.into_iter().find(|e| e.exists() && e.is_dir() && e.name == folder_name)?;
    let files: Vec<DirEntry> = card.directory(folder.cluster, folder.length).ok()?.into_iter().filter(DirEntry::is_save_file).collect();
    let dir_mode = folder.mode | DF_DIR | DF_EXISTS;
    let mut out = psu_record(dir_mode, files.len() as u32 + 2, &folder.name, &folder.created, &folder.modified);
    out.extend(psu_record(dir_mode, 0, ".", &folder.created, &folder.modified));
    out.extend(psu_record(dir_mode, 0, "..", &folder.created, &folder.modified));
    for file in files {
        let data = card.chain_bytes(file.cluster, file.length as usize).ok()?;
        out.extend(psu_record(file.mode | DF_FILE | DF_EXISTS, file.length, &file.name, &file.created, &file.modified));
        out.extend_from_slice(&data);
        out.resize(out.len() + (1024 - data.len() % 1024) % 1024, 0);
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-memcard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn extracts_skus_like_electron() {
        assert_eq!(sku_from_folder("BASLUS-20294USER").as_deref(), Some("SLUS-20294"));
        assert_eq!(sku_from_folder("BESLES-52988XXX").as_deref(), Some("SLES-52988"));
        assert_eq!(sku_from_folder("BASCUS-94163DRAKAN").as_deref(), Some("SCUS-94163"));
        assert_eq!(sku_from_folder("SLPM_12345").as_deref(), Some("SLPM-12345"));
        assert_eq!(sku_from_folder("BIEXEC-SYSTEM"), None);
        assert_eq!(sku_from_folder("BAXXXX-12345"), None);
        assert_eq!(sku_from_folder("BADATA-SYSTEM"), None);
    }

    /// A 128 KB PS1 card with a two-block save in blocks 2 and 3.
    pub fn ps1_card(header: usize) -> Vec<u8> {
        let mut card = vec![0u8; header + 16 * PS1_BLOCK];
        let dir = header;
        card[dir..dir + 2].copy_from_slice(b"MC");
        for b in 1..=15 {
            card[dir + b * PS1_FRAME..dir + b * PS1_FRAME + 4].copy_from_slice(&0xa0u32.to_le_bytes());
            card[dir + b * PS1_FRAME + 8..dir + b * PS1_FRAME + 10].copy_from_slice(&0xffffu16.to_le_bytes());
        }
        let f2 = dir + 2 * PS1_FRAME;
        card[f2..f2 + 4].copy_from_slice(&0x51u32.to_le_bytes());
        card[f2 + 4..f2 + 8].copy_from_slice(&(2 * PS1_BLOCK as u32).to_le_bytes());
        card[f2 + 8..f2 + 10].copy_from_slice(&2u16.to_le_bytes()); // next block 3
        card[f2 + 0x0a..f2 + 0x0a + 18].copy_from_slice(b"BASCUS-94163DRAKAN");
        let f3 = dir + 3 * PS1_FRAME;
        card[f3..f3 + 4].copy_from_slice(&0x53u32.to_le_bytes());
        card[header + 2 * PS1_BLOCK] = 0xaa;
        card[header + 3 * PS1_BLOCK] = 0xbb;
        card
    }

    #[test]
    fn reads_ps1_cards_and_builds_mcs() {
        for header in [0, 64, 3904] {
            let path = temp_file("card.mcd", &ps1_card(header));
            assert_eq!(inspect_ps1(&path), FormatState::Formatted);
            let saves = list_ps1(&path).unwrap();
            assert_eq!(saves.len(), 1);
            assert_eq!((saves[0].folder.as_str(), saves[0].sku.as_deref(), saves[0].file_count, saves[0].size), ("BASCUS-94163DRAKAN", Some("SCUS-94163"), 2, 16384));
            let mcs = ps1_mcs(&path, "BASCUS-94163DRAKAN").unwrap();
            assert_eq!(mcs.len(), 128 + 2 * PS1_BLOCK);
            assert_eq!((mcs[128], mcs[128 + PS1_BLOCK]), (0xaa, 0xbb));
            assert!(ps1_mcs(&path, "missing").is_none());
        }
        let blank = temp_file("blank.mcd", &vec![0u8; 16 * PS1_BLOCK]);
        assert_eq!(inspect_ps1(&blank), FormatState::Unformatted);
        assert!(list_ps1(&blank).is_none());
        assert_eq!(inspect_ps1(Path::new("Z:/no/such/card.mcd")), FormatState::Unreadable);
    }

    fn dir_entry(mode: u16, length: u32, cluster: u32, name: &str, tod: [u8; 8]) -> Vec<u8> {
        let mut e = psu_record(mode, length, name, &tod, &tod);
        e[0x10..0x14].copy_from_slice(&cluster.to_le_bytes());
        e
    }

    /// A tiny PS2 card without ECC: 512-byte pages, 2 per cluster, 16 clusters.
    /// Absolute clusters: 0 superblock, 1 indirect FAT, 2 FAT, 3.. data (alloc offset 3).
    /// Relative chains: root 0 -> 4, save folder 1 -> 5, a 1500-byte file 2 -> 3.
    pub fn ps2_card() -> Vec<u8> {
        let cs = 1024;
        let mut card = vec![0u8; 16 * cs];
        card[..PS2_MAGIC.len()].copy_from_slice(PS2_MAGIC);
        card[0x28..0x2a].copy_from_slice(&512u16.to_le_bytes());
        card[0x2a..0x2c].copy_from_slice(&2u16.to_le_bytes());
        card[0x30..0x34].copy_from_slice(&16u32.to_le_bytes());
        card[0x34..0x38].copy_from_slice(&3u32.to_le_bytes());
        card[0x50..0x54].copy_from_slice(&1u32.to_le_bytes());
        card[cs..cs + 4].copy_from_slice(&2u32.to_le_bytes());
        for (rel, value) in [(0, 0x8000_0004u32), (1, 0x8000_0005), (2, 0x8000_0003), (3, 0xffff_ffff), (4, 0xffff_ffff), (5, 0xffff_ffff)] {
            card[2 * cs + rel * 4..2 * cs + rel * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        let tod = [0, 30, 15, 12, 24, 12, 0xe8, 0x07]; // 2024-12-24 12:15:30 UTC
        let dir = DF_DIR | DF_EXISTS | 0x7;
        let mut put = |abs: usize, slot: usize, entry: Vec<u8>| card[abs * cs + slot * 512..abs * cs + (slot + 1) * 512].copy_from_slice(&entry);
        put(3, 0, dir_entry(dir, 3, 0, ".", tod));
        put(3, 1, dir_entry(dir, 0, 0, "..", tod));
        put(7, 0, dir_entry(dir, 3, 1, "BASLUS-20294USER", tod));
        put(4, 0, dir_entry(dir, 3, 0, ".", tod));
        put(4, 1, dir_entry(dir, 0, 0, "..", tod));
        put(8, 0, dir_entry(DF_FILE | DF_EXISTS | 0x7, 1500, 2, "icon.sys", tod));
        card[5 * cs] = 0x11;
        card[6 * cs + 475] = 0x22; // byte 1499 of the file
        card
    }

    #[test]
    fn reads_ps2_cards_and_builds_psu() {
        let card = ps2_card();
        let path = temp_file("Mcd001.ps2", &card);
        assert_eq!(inspect_ps2(&path), FormatState::Formatted);
        let saves = list_ps2(&path).unwrap();
        assert_eq!(saves.len(), 1);
        let save = &saves[0];
        assert_eq!((save.folder.as_str(), save.sku.as_deref(), save.file_count, save.size), ("BASLUS-20294USER", Some("SLUS-20294"), 1, 1500));
        assert_eq!(save.modified_ms, 1_735_042_530_000);

        let psu = ps2_psu(&path, "BASLUS-20294USER").unwrap();
        assert_eq!(psu.len(), 4 * 512 + 2048);
        assert_eq!(u32_at(&psu, 4), 3, "folder record counts its files plus . and ..");
        assert_eq!(latin1(&psu[3 * 512 + 0x40..3 * 512 + 0x60]), "icon.sys");
        assert_eq!((psu[4 * 512], psu[4 * 512 + 1499], psu[4 * 512 + 1500]), (0x11, 0x22, 0));

        let mut ecc = Vec::new();
        for page in card.chunks(512) {
            ecc.extend_from_slice(page);
            ecc.extend_from_slice(&[0u8; 16]);
        }
        let ecc_path = temp_file("ecc.ps2", &ecc);
        assert_eq!(list_ps2(&ecc_path).unwrap(), saves, "ECC dumps read the same");
        assert_eq!(inspect_ps2(&temp_file("blank.ps2", &[0u8; 2048])), FormatState::Unformatted);
    }
}
