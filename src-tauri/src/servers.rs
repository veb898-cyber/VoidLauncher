// ==================== Server List (servers.dat) ====================
// Per-instance multiplayer server list stored in the vanilla format:
// `servers.dat` is GZip-compressed NBT whose root compound holds a
// `servers` list of compounds (name / ip / optional icon / acceptTextures /
// hidden). We parse the whole tree, mutate only the list and serialize it
// back, so unknown keys (root level or per server) survive untouched.

use crate::error::{LauncherError, Result};
use crate::instances;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const TAG_END: u8 = 0;
const TAG_BYTE: u8 = 1;
const TAG_SHORT: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_LONG: u8 = 4;
const TAG_FLOAT: u8 = 5;
const TAG_DOUBLE: u8 = 6;
const TAG_BYTE_ARRAY: u8 = 7;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;
const TAG_INT_ARRAY: u8 = 11;
const TAG_LONG_ARRAY: u8 = 12;

/// Depth guard for hostile/corrupt files so parsing cannot recurse forever.
const MAX_DEPTH: usize = 64;
/// Size guard for array payloads (64 MiB is far beyond any real file).
const MAX_ARRAY_LEN: i32 = 64 * 1024 * 1024;

const MAX_NAME_CHARS: usize = 128;
const MAX_IP_CHARS: usize = 255;

/// One entry of the in-game multiplayer list, as shown to the user.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerEntry {
    pub name: String,
    pub ip: String,
    /// Base64 PNG icon as stored by the game (no data-URL prefix).
    pub icon: Option<String>,
    /// `acceptTextures` flag; `None` when the game has not decided yet.
    pub accept_textures: Option<bool>,
    pub hidden: bool,
    /// SHA-256 of the canonical full NBT compound (all fields, including
    /// unknown ones). Used to identify the exact row the user saw.
    #[serde(default)]
    pub fingerprint: String,
}

/// Minimal generic NBT value tree (all vanilla tag types).
#[derive(Debug, Clone, PartialEq)]
enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List { elem_type: u8, items: Vec<Nbt> },
    Compound(Vec<(String, Nbt)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

fn invalid(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.into())
}

// ==================== Parsing ====================

fn read_u8(r: &mut impl Read) -> std::io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_i16_be(r: &mut impl Read) -> std::io::Result<i16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(i16::from_be_bytes(b))
}

fn read_i32_be(r: &mut impl Read) -> std::io::Result<i32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_be_bytes(b))
}

fn read_i64_be(r: &mut impl Read) -> std::io::Result<i64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(i64::from_be_bytes(b))
}

fn read_string(r: &mut impl Read) -> std::io::Result<String> {
    let len = read_i16_be(r)?;
    if len < 0 {
        return Err(invalid("negative string length"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    decode_modified_utf8(&buf).map_err(|_| invalid("invalid modified UTF-8 string"))
}

/// Java `DataInput.readUTF` uses modified UTF-8 (CESU-8 for non-BMP code
/// points, NUL as C0 80), not standard UTF-8. `servers.dat` is written by the
/// game with `DataOutput.writeUTF`, so a name with an emoji must be decoded
/// as two surrogate code units rather than a four-byte UTF-8 sequence.
fn decode_modified_utf8(bytes: &[u8]) -> std::result::Result<String, ()> {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        let first = bytes[index];
        let (unit, advance) = match first {
            0x00..=0x7F => (first as u16, 1),
            0xC0..=0xDF => {
                if index + 1 >= bytes.len() || bytes[index + 1] & 0xC0 != 0x80 {
                    return Err(());
                }
                let unit = ((first as u16 & 0x1F) << 6) | (bytes[index + 1] as u16 & 0x3F);
                if unit != 0 && unit < 0x80 {
                    return Err(());
                }
                (unit, 2)
            }
            0xE0..=0xEF => {
                if index + 2 >= bytes.len()
                    || bytes[index + 1] & 0xC0 != 0x80
                    || bytes[index + 2] & 0xC0 != 0x80
                {
                    return Err(());
                }
                let unit = ((first as u16 & 0x0F) << 12)
                    | ((bytes[index + 1] as u16 & 0x3F) << 6)
                    | (bytes[index + 2] as u16 & 0x3F);
                if unit < 0x800 {
                    return Err(());
                }
                (unit, 3)
            }
            // Tolerate standard UTF-8 four-byte sequences produced by external
            // tools, converting the code point to a surrogate pair.
            0xF0..=0xF4 => {
                if index + 3 >= bytes.len()
                    || bytes[index + 1] & 0xC0 != 0x80
                    || bytes[index + 2] & 0xC0 != 0x80
                    || bytes[index + 3] & 0xC0 != 0x80
                {
                    return Err(());
                }
                let code = ((first as u32 & 0x07) << 18)
                    | ((bytes[index + 1] as u32 & 0x3F) << 12)
                    | ((bytes[index + 2] as u32 & 0x3F) << 6)
                    | (bytes[index + 3] as u32 & 0x3F);
                if !(0x10000..=0x10FFFF).contains(&code) {
                    return Err(());
                }
                let code = code - 0x10000;
                units.push(0xD800 | ((code >> 10) as u16));
                units.push(0xDC00 | ((code & 0x3FF) as u16));
                index += 4;
                continue;
            }
            _ => return Err(()),
        };
        units.push(unit);
        index += advance;
    }
    String::from_utf16(&units).map_err(|_| ())
}

fn encode_modified_utf8(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for unit in s.encode_utf16() {
        match unit {
            0 => {
                out.push(0xC0);
                out.push(0x80);
            }
            0x01..=0x7F => out.push(unit as u8),
            0x80..=0x7FF => {
                out.push(0xC0 | ((unit >> 6) as u8));
                out.push(0x80 | ((unit & 0x3F) as u8));
            }
            _ => {
                out.push(0xE0 | ((unit >> 12) as u8));
                out.push(0x80 | (((unit >> 6) & 0x3F) as u8));
                out.push(0x80 | ((unit & 0x3F) as u8));
            }
        }
    }
    out
}

/// Parse a full GZip-compressed NBT document (root must be a compound).
fn parse_document(r: &mut impl Read) -> std::io::Result<Nbt> {
    let tag = read_u8(r)?;
    if tag != TAG_COMPOUND {
        return Err(invalid("root tag is not a compound"));
    }
    let _root_name = read_string(r)?;
    read_payload(r, TAG_COMPOUND, 0)
}

fn read_payload(r: &mut impl Read, tag: u8, depth: usize) -> std::io::Result<Nbt> {
    if depth > MAX_DEPTH {
        return Err(invalid("NBT nesting too deep"));
    }
    Ok(match tag {
        TAG_BYTE => Nbt::Byte(read_u8(r)? as i8),
        TAG_SHORT => Nbt::Short(read_i16_be(r)?),
        TAG_INT => Nbt::Int(read_i32_be(r)?),
        TAG_LONG => Nbt::Long(read_i64_be(r)?),
        TAG_FLOAT => Nbt::Float(f32::from_bits(read_i32_be(r)? as u32)),
        TAG_DOUBLE => Nbt::Double(f64::from_bits(read_i64_be(r)? as u64)),
        TAG_BYTE_ARRAY => {
            let len = read_i32_be(r)?;
            if !(0..=MAX_ARRAY_LEN).contains(&len) {
                return Err(invalid("byte array length out of range"));
            }
            let mut buf = vec![0u8; len as usize];
            r.read_exact(&mut buf)?;
            Nbt::ByteArray(buf)
        }
        TAG_STRING => Nbt::String(read_string(r)?),
        TAG_LIST => {
            let elem_type = read_u8(r)?;
            let len = read_i32_be(r)?;
            if !(0..=MAX_ARRAY_LEN).contains(&len) {
                return Err(invalid("list length out of range"));
            }
            let mut items = Vec::with_capacity(len.min(4096) as usize);
            for _ in 0..len {
                items.push(read_payload(r, elem_type, depth + 1)?);
            }
            Nbt::List { elem_type, items }
        }
        TAG_COMPOUND => {
            let mut fields = Vec::new();
            loop {
                let t = read_u8(r)?;
                if t == TAG_END {
                    break;
                }
                let name = read_string(r)?;
                let value = read_payload(r, t, depth + 1)?;
                fields.push((name, value));
            }
            Nbt::Compound(fields)
        }
        TAG_INT_ARRAY => {
            let len = read_i32_be(r)?;
            if !(0..=MAX_ARRAY_LEN).contains(&len) {
                return Err(invalid("int array length out of range"));
            }
            let mut items = Vec::with_capacity(len.min(4096) as usize);
            for _ in 0..len {
                items.push(read_i32_be(r)?);
            }
            Nbt::IntArray(items)
        }
        TAG_LONG_ARRAY => {
            let len = read_i32_be(r)?;
            if !(0..=MAX_ARRAY_LEN).contains(&len) {
                return Err(invalid("long array length out of range"));
            }
            let mut items = Vec::with_capacity(len.min(4096) as usize);
            for _ in 0..len {
                items.push(read_i64_be(r)?);
            }
            Nbt::LongArray(items)
        }
        other => return Err(invalid(format!("unknown NBT tag type {}", other))),
    })
}

// ==================== Serialization ====================

fn tag_of(v: &Nbt) -> u8 {
    match v {
        Nbt::Byte(_) => TAG_BYTE,
        Nbt::Short(_) => TAG_SHORT,
        Nbt::Int(_) => TAG_INT,
        Nbt::Long(_) => TAG_LONG,
        Nbt::Float(_) => TAG_FLOAT,
        Nbt::Double(_) => TAG_DOUBLE,
        Nbt::ByteArray(_) => TAG_BYTE_ARRAY,
        Nbt::String(_) => TAG_STRING,
        Nbt::List { .. } => TAG_LIST,
        Nbt::Compound(_) => TAG_COMPOUND,
        Nbt::IntArray(_) => TAG_INT_ARRAY,
        Nbt::LongArray(_) => TAG_LONG_ARRAY,
    }
}

fn write_string(out: &mut Vec<u8>, s: &str) -> std::io::Result<()> {
    let bytes = encode_modified_utf8(s);
    if bytes.len() > u16::MAX as usize {
        return Err(invalid("string too long for NBT"));
    }
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(&bytes);
    Ok(())
}

fn write_payload(v: &Nbt, out: &mut Vec<u8>) -> std::io::Result<()> {
    match v {
        Nbt::Byte(b) => out.push(*b as u8),
        Nbt::Short(s) => out.extend_from_slice(&s.to_be_bytes()),
        Nbt::Int(i) => out.extend_from_slice(&i.to_be_bytes()),
        Nbt::Long(l) => out.extend_from_slice(&l.to_be_bytes()),
        Nbt::Float(f) => out.extend_from_slice(&f.to_bits().to_be_bytes()),
        Nbt::Double(d) => out.extend_from_slice(&d.to_bits().to_be_bytes()),
        Nbt::ByteArray(bytes) => {
            out.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        Nbt::String(s) => write_string(out, s)?,
        Nbt::List { elem_type, items } => {
            out.push(*elem_type);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for item in items {
                write_payload(item, out)?;
            }
        }
        Nbt::Compound(fields) => {
            for (name, value) in fields {
                out.push(tag_of(value));
                write_string(out, name)?;
                write_payload(value, out)?;
            }
            out.push(TAG_END);
        }
        Nbt::IntArray(items) => {
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for i in items {
                out.extend_from_slice(&i.to_be_bytes());
            }
        }
        Nbt::LongArray(items) => {
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for l in items {
                out.extend_from_slice(&l.to_be_bytes());
            }
        }
    }
    Ok(())
}

/// Deterministic compound encoding for row fingerprints. Compound fields are
/// sorted by name (NBT treats their order as insignificant), while list order
/// is preserved because it may carry meaning. Every field, including unknown
/// ones, participates in the hash.
fn write_canonical_compound(fields: &[(String, Nbt)], out: &mut Vec<u8>) -> std::io::Result<()> {
    let mut sorted: Vec<&(String, Nbt)> = fields.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, value) in sorted {
        out.push(tag_of(value));
        write_string(out, name)?;
        write_canonical_value(value, out)?;
    }
    out.push(TAG_END);
    Ok(())
}

fn write_canonical_value(value: &Nbt, out: &mut Vec<u8>) -> std::io::Result<()> {
    match value {
        Nbt::Compound(inner) => write_canonical_compound(inner, out),
        Nbt::List { elem_type, items } => {
            out.push(*elem_type);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for item in items {
                write_canonical_value(item, out)?;
            }
            Ok(())
        }
        _ => write_payload(value, out),
    }
}

fn server_entry_fingerprint(fields: &[(String, Nbt)]) -> String {
    let mut out = Vec::new();
    if write_canonical_compound(fields, &mut out).is_err() {
        return String::new();
    }
    hex::encode(Sha256::digest(&out))
}

fn serialize_document(root: &Nbt) -> std::io::Result<Vec<u8>> {
    let mut plain = Vec::new();
    plain.push(TAG_COMPOUND);
    write_string(&mut plain, "")?;
    write_payload(root, &mut plain)?;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&plain)?;
    gz.finish()
}

// ==================== File operations ====================

/// Path of `servers.dat` for an instance: `<instance>/.minecraft/servers.dat`.
pub fn servers_file(instances_dir: &Path, instance_name: &str) -> Result<PathBuf> {
    let instance = instances::get_instance(instances_dir, instance_name)?;
    Ok(instance.minecraft_dir(instances_dir).join("servers.dat"))
}

/// Cap on the on-disk file (servers.dat is a few KB; anything larger is
/// broken or hostile) and on the decompressed stream (a gzip bomb must not
/// be able to balloon memory during parse).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Exact content version used for optimistic concurrency. The byte hash is
/// used instead of a timestamp because Windows file times can have coarse
/// resolution and are not a reliable way to distinguish two writes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileVersion {
    exists: bool,
    len: u64,
    sha256: [u8; 32],
}

fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn version_of(bytes: &[u8], exists: bool) -> FileVersion {
    FileVersion {
        exists,
        len: bytes.len() as u64,
        sha256: hash_bytes(bytes),
    }
}

fn file_version(path: &Path) -> Result<FileVersion> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(version_of(&[], false));
        }
        Err(e) => return Err(e.into()),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(LauncherError::Conflict(
            "servers.dat grew beyond the safe size while being edited".to_string(),
        ));
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(version_of(&bytes, true)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(version_of(&[], false)),
        Err(e) => Err(e.into()),
    }
}

/// Read `servers.dat` together with the exact version that was parsed.
///
/// `add`, `update` and `delete` pass that version to `save_if_unchanged`, so
/// an external write between the read and the replacement cannot be silently
/// overwritten.
fn load_root_versioned(path: &Path) -> Result<(Nbt, FileVersion)> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(meta) = &meta {
        if meta.len() == 0 {
            // The game treats an empty file like a missing one.
            let bytes = std::fs::read(path)?;
            return Ok((empty_root(), version_of(&bytes, true)));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(LauncherError::Instance("servers.dat is too large".into()));
        }
    }
    let Some(_) = meta else {
        return Ok((empty_root(), version_of(&[], false)));
    };

    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(LauncherError::Instance("servers.dat is too large".into()));
    }
    let version = version_of(&bytes, true);
    if bytes.is_empty() {
        return Ok((empty_root(), version));
    }

    let parsed: std::io::Result<Nbt> = if bytes.starts_with(&[0x1f, 0x8b]) {
        // What the game itself writes: gzip-compressed NBT.
        let mut plain = Vec::new();
        let decoded = flate2::read::GzDecoder::new(bytes.as_slice())
            .take(MAX_FILE_BYTES)
            .read_to_end(&mut plain);
        match decoded {
            Ok(_) => parse_document(&mut plain.as_slice()),
            Err(e) => Err(e),
        }
    } else {
        // Some third-party tools write plain, uncompressed NBT.
        parse_document(&mut bytes.as_slice())
    };
    let root =
        parsed.map_err(|e| LauncherError::Instance(format!("servers.dat is unreadable: {}", e)))?;
    Ok((root, version))
}

fn load_root(path: &Path) -> Result<Nbt> {
    Ok(load_root_versioned(path)?.0)
}

fn empty_root() -> Nbt {
    Nbt::Compound(vec![(
        "servers".to_string(),
        Nbt::List {
            elem_type: TAG_COMPOUND,
            items: Vec::new(),
        },
    )])
}

/// Mutable access to the `servers` list, inserting an empty one when the
/// key is absent. A key of an unexpected type is reported instead of being
/// silently overwritten.
fn servers_list_mut(root: &mut Nbt) -> Result<&mut Vec<Nbt>> {
    let Nbt::Compound(fields) = root else {
        return Err(LauncherError::Instance(
            "servers.dat root is not a compound".into(),
        ));
    };
    if !fields.iter().any(|(k, _)| k == "servers") {
        fields.push((
            "servers".to_string(),
            Nbt::List {
                elem_type: TAG_COMPOUND,
                items: Vec::new(),
            },
        ));
    }
    match fields.iter_mut().find(|(k, _)| k == "servers") {
        Some((_, Nbt::List { elem_type, items })) => {
            if items.is_empty() {
                // The NBT spec allows an empty list to declare element type 0
                // (TAG_End), and Mojang's own writer does that for a freshly
                // emptied list. Appending compounds to it would produce a
                // list that the game's parser rejects, so the declared type
                // is normalized before the first write.
                *elem_type = TAG_COMPOUND;
                Ok(items)
            } else if *elem_type == TAG_COMPOUND {
                Ok(items)
            } else {
                Err(LauncherError::Instance(
                    "servers.dat has an unexpected servers entry".into(),
                ))
            }
        }
        _ => Err(LauncherError::Instance(
            "servers.dat has an unexpected servers entry".into(),
        )),
    }
}

fn entry_from_nbt(v: &Nbt) -> Option<ServerEntry> {
    let Nbt::Compound(fields) = v else {
        return None;
    };
    let mut name = String::new();
    let mut ip = String::new();
    let mut icon = None;
    let mut accept_textures = None;
    let mut hidden = false;
    for (key, value) in fields {
        match (key.as_str(), value) {
            ("name", Nbt::String(s)) => name = s.clone(),
            ("ip", Nbt::String(s)) => ip = s.clone(),
            ("icon", Nbt::String(s)) => icon = Some(s.clone()),
            ("acceptTextures", Nbt::Byte(b)) => accept_textures = Some(*b != 0),
            ("hidden", Nbt::Byte(b)) => hidden = *b != 0,
            _ => {}
        }
    }
    Some(ServerEntry {
        name,
        ip,
        icon,
        accept_textures,
        hidden,
        fingerprint: server_entry_fingerprint(fields),
    })
}

fn list_from_root(root: &Nbt) -> Vec<ServerEntry> {
    let Nbt::Compound(fields) = root else {
        return Vec::new();
    };
    fields
        .iter()
        .find(|(k, _)| k == "servers")
        .and_then(|(_, v)| match v {
            Nbt::List { items, .. } => Some(items),
            _ => None,
        })
        .map(|items| items.iter().filter_map(entry_from_nbt).collect())
        .unwrap_or_default()
}

fn validate_entry(name: &str, ip: &str) -> Result<(String, String)> {
    let name = name.trim();
    let ip = ip.trim();
    if name.is_empty() {
        return Err(LauncherError::Instance(
            "The server name must not be empty".into(),
        ));
    }
    if ip.is_empty() {
        return Err(LauncherError::Instance(
            "The server address must not be empty".into(),
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(LauncherError::Instance(format!(
            "The server name is too long (max {} characters)",
            MAX_NAME_CHARS
        )));
    }
    if ip.chars().count() > MAX_IP_CHARS {
        return Err(LauncherError::Instance(
            "The server address is too long".into(),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(LauncherError::Instance(
            "The server name contains control characters".into(),
        ));
    }
    if ip.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(LauncherError::Instance(
            "The server address must not contain spaces".into(),
        ));
    }
    Ok((name.to_string(), ip.to_string()))
}

#[cfg(test)]
fn save(path: &Path, root: &Nbt) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let bytes = serialize_document(root)
        .map_err(|e| LauncherError::Instance(format!("Failed to encode servers.dat: {}", e)))?;
    instances::write_file_atomic(path, &bytes)
}

/// Write a mutated tree only if the file still matches the version that was
/// read. The check is repeated immediately before the atomic replacement to
/// shrink the external-write window. A conflict is reported and the newer
/// file is left untouched; no merge and no blind retry are performed.
///
/// A residual window between the last hash check and the final rename is
/// unavoidable: Windows has no atomic compare-and-swap rename, and a real
/// byte-range lock would block Minecraft's own save (or fail it), risking
/// data loss. The window is therefore kept to the shortest possible sequence
/// of operations and is documented rather than "protected" by a lock the game
/// does not cooperate with.
fn save_if_unchanged(path: &Path, root: &Nbt, expected: &FileVersion) -> Result<()> {
    if &file_version(path)? != expected {
        return Err(LauncherError::Conflict(
            "servers.dat was changed by another program".to_string(),
        ));
    }
    let bytes = serialize_document(root)
        .map_err(|e| LauncherError::Instance(format!("Failed to encode servers.dat: {}", e)))?;
    if &file_version(path)? != expected {
        return Err(LauncherError::Conflict(
            "servers.dat was changed by another program".to_string(),
        ));
    }
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    instances::write_file_atomic(path, &bytes)
}

/// Read the server list. A missing file yields an empty list.
pub fn list(path: &Path) -> Result<Vec<ServerEntry>> {
    let root = load_root(path)?;
    Ok(list_from_root(&root))
}

/// Append a server and return the updated list.
pub fn add(path: &Path, name: &str, ip: &str) -> Result<Vec<ServerEntry>> {
    let (name, ip) = validate_entry(name, ip)?;
    let (mut root, version) = load_root_versioned(path)?;
    let items = servers_list_mut(&mut root)?;
    let mut fields = vec![
        ("name".to_string(), Nbt::String(name)),
        ("ip".to_string(), Nbt::String(ip)),
    ];
    fields.push(("acceptTextures".to_string(), Nbt::Byte(1)));
    items.push(Nbt::Compound(fields));
    save_if_unchanged(path, &root, &version)?;
    Ok(list_from_root(&root))
}

/// Rename/repoint the server at `index` (icon and flags are preserved).
/// Test-only: production callers must supply a row identity.
#[cfg(test)]
pub fn update(path: &Path, index: usize, name: &str, ip: &str) -> Result<Vec<ServerEntry>> {
    update_impl(path, index, None, name, ip)
}

/// Update only when the current row still matches the name/address the user
/// saw. Kept for callers that predate full-row fingerprints.
pub fn update_checked(
    path: &Path,
    index: usize,
    expected_name: &str,
    expected_ip: &str,
    name: &str,
    ip: &str,
) -> Result<Vec<ServerEntry>> {
    update_impl(
        path,
        index,
        Some(ExpectedIdentity::NameIp(expected_name, expected_ip)),
        name,
        ip,
    )
}

/// Update only when the complete NBT compound (all fields, including unknown
/// ones) still matches the fingerprint shown by the UI.
pub fn update_fingerprint(
    path: &Path,
    index: usize,
    expected_fingerprint: &str,
    name: &str,
    ip: &str,
) -> Result<Vec<ServerEntry>> {
    update_impl(
        path,
        index,
        Some(ExpectedIdentity::Fingerprint(expected_fingerprint)),
        name,
        ip,
    )
}

enum ExpectedIdentity<'a> {
    NameIp(&'a str, &'a str),
    Fingerprint(&'a str),
}

fn update_impl(
    path: &Path,
    index: usize,
    expected: Option<ExpectedIdentity<'_>>,
    name: &str,
    ip: &str,
) -> Result<Vec<ServerEntry>> {
    let (name, ip) = validate_entry(name, ip)?;
    let (mut root, version) = load_root_versioned(path)?;
    let items = servers_list_mut(&mut root)?;
    ensure_expected_identity(items, index, expected)?;
    let Some(Nbt::Compound(fields)) = items.get_mut(index) else {
        return Err(LauncherError::Instance(format!(
            "Server #{} does not exist",
            index
        )));
    };
    set_string_field(fields, "name", name);
    set_string_field(fields, "ip", ip);
    save_if_unchanged(path, &root, &version)?;
    Ok(list_from_root(&root))
}

/// Remove the server at `index` and return the updated list.
/// Test-only: production callers must supply a row identity.
#[cfg(test)]
pub fn delete(path: &Path, index: usize) -> Result<Vec<ServerEntry>> {
    delete_impl(path, index, None)
}

/// Delete only when the current row still matches the name/address the user
/// saw. Kept for callers that predate full-row fingerprints.
pub fn delete_checked(
    path: &Path,
    index: usize,
    expected_name: &str,
    expected_ip: &str,
) -> Result<Vec<ServerEntry>> {
    delete_impl(
        path,
        index,
        Some(ExpectedIdentity::NameIp(expected_name, expected_ip)),
    )
}

/// Delete only when the complete NBT compound still matches the fingerprint
/// shown by the UI.
pub fn delete_fingerprint(
    path: &Path,
    index: usize,
    expected_fingerprint: &str,
) -> Result<Vec<ServerEntry>> {
    delete_impl(
        path,
        index,
        Some(ExpectedIdentity::Fingerprint(expected_fingerprint)),
    )
}

fn delete_impl(
    path: &Path,
    index: usize,
    expected: Option<ExpectedIdentity<'_>>,
) -> Result<Vec<ServerEntry>> {
    let (mut root, version) = load_root_versioned(path)?;
    let items = servers_list_mut(&mut root)?;
    if index >= items.len() {
        return Err(LauncherError::Instance(format!(
            "Server #{} does not exist",
            index
        )));
    }
    ensure_expected_identity(items, index, expected)?;
    items.remove(index);
    save_if_unchanged(path, &root, &version)?;
    Ok(list_from_root(&root))
}

fn entry_name_ip(item: &Nbt) -> (String, String) {
    let Nbt::Compound(fields) = item else {
        return (String::new(), String::new());
    };
    let field = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, value)| match value {
                Nbt::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default()
    };
    (field("name"), field("ip"))
}

fn entry_fingerprint(item: &Nbt) -> Option<String> {
    let Nbt::Compound(fields) = item else {
        return None;
    };
    Some(server_entry_fingerprint(fields))
}

/// The target row must be the unique row matching the identity the user saw.
/// A missing or duplicated match is a conflict: silently editing/deleting one
/// of several identical rows would be unpredictable.
fn ensure_expected_identity(
    items: &[Nbt],
    index: usize,
    expected: Option<ExpectedIdentity<'_>>,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if index >= items.len() {
        return Err(LauncherError::Instance(format!(
            "Server #{} does not exist",
            index
        )));
    }

    let matches: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| match &expected {
            ExpectedIdentity::NameIp(expected_name, expected_ip) => {
                let (name, ip) = entry_name_ip(item);
                name == *expected_name && ip == *expected_ip
            }
            ExpectedIdentity::Fingerprint(expected_fingerprint) => {
                entry_fingerprint(item).as_deref() == Some(*expected_fingerprint)
            }
        })
        .map(|(idx, _)| idx)
        .collect();

    if matches.len() != 1 || matches[0] != index {
        return Err(LauncherError::Conflict(
            "the selected server entry is stale or ambiguous".to_string(),
        ));
    }
    Ok(())
}

/// Set (or append) a string field on a server compound; a field with a
/// different type is replaced, nothing else is touched.
fn set_string_field(fields: &mut Vec<(String, Nbt)>, key: &str, value: String) {
    match fields.iter_mut().find(|(k, _)| k == key) {
        Some((_, v)) => *v = Nbt::String(value),
        None => fields.push((key.to_string(), Nbt::String(value))),
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "voidlauncher_servers_test_{}_{}.dat",
            std::process::id(),
            name
        ))
    }

    #[test]
    fn modified_utf8_round_trips_emoji_and_nul() {
        for value in ["Test 😀", "nul\0inside", "обычное имя", ""] {
            let encoded = encode_modified_utf8(value);
            assert_eq!(
                decode_modified_utf8(&encoded).expect("modified UTF-8 decodes"),
                value
            );
        }
    }

    #[test]
    fn modified_utf8_uses_cesu8_for_non_bmp_characters() {
        let encoded = encode_modified_utf8("😀");
        // High and low surrogate, six bytes total; never a four-byte UTF-8
        // sequence (the game's readUTF would reject that).
        assert_eq!(encoded.len(), 6);
        assert_eq!(&encoded[..3], &[0xED, 0xA0, 0xBD]);
        assert_eq!(&encoded[3..], &[0xED, 0xB8, 0x80]);
        assert!(!encoded.iter().any(|byte| *byte >= 0xF0));
    }

    #[test]
    fn modified_utf8_tolerates_standard_four_byte_utf8() {
        let decoded = decode_modified_utf8("😀".as_bytes()).expect("standard UTF-8 is accepted");
        assert_eq!(decoded, "😀");
    }

    #[test]
    fn server_name_with_emoji_round_trips_through_servers_dat() {
        let path = temp_path("emoji");
        add(&path, "Test 😀", "example.com").expect("server with emoji added");
        let listed = list(&path).expect("servers.dat readable");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Test 😀");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nbt_roundtrip_preserves_all_tags() {
        let root = Nbt::Compound(vec![
            ("byte".into(), Nbt::Byte(-7)),
            ("short".into(), Nbt::Short(-1234)),
            ("int".into(), Nbt::Int(-123456)),
            ("long".into(), Nbt::Long(-1234567890)),
            ("float".into(), Nbt::Float(1.5)),
            ("double".into(), Nbt::Double(-2.25)),
            ("bytes".into(), Nbt::ByteArray(vec![1, 2, 3])),
            ("str".into(), Nbt::String("héllo".into())),
            (
                "list".into(),
                Nbt::List {
                    elem_type: TAG_INT,
                    items: vec![Nbt::Int(1), Nbt::Int(2)],
                },
            ),
            (
                "empty_list".into(),
                Nbt::List {
                    elem_type: TAG_COMPOUND,
                    items: vec![],
                },
            ),
            (
                "compound".into(),
                Nbt::Compound(vec![("inner".into(), Nbt::String("x".into()))]),
            ),
            ("ints".into(), Nbt::IntArray(vec![5, 6])),
            ("longs".into(), Nbt::LongArray(vec![7, 8])),
        ]);
        let bytes = serialize_document(&root).unwrap();
        let mut plain = Vec::new();
        flate2::read::GzDecoder::new(std::io::Cursor::new(bytes))
            .read_to_end(&mut plain)
            .unwrap();
        let parsed = parse_document(&mut std::io::Cursor::new(plain)).unwrap();
        assert_eq!(root, parsed);
    }

    #[test]
    fn add_list_update_delete_roundtrip() {
        let path = temp_path("crud");
        let _ = std::fs::remove_file(&path);

        assert_eq!(list(&path).unwrap(), Vec::new());

        let servers = add(&path, "  My Server ", " play.example.com ").unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "My Server");
        assert_eq!(servers[0].ip, "play.example.com");
        assert_eq!(servers[0].accept_textures, Some(true));

        let servers = add(&path, "Second", "second.example.com:25566").unwrap();
        assert_eq!(servers.len(), 2);

        let servers = update(&path, 0, "Renamed", "new.example.com").unwrap();
        assert_eq!(servers[0].name, "Renamed");
        assert_eq!(servers[0].ip, "new.example.com");
        assert_eq!(servers[1].name, "Second");

        let servers = delete(&path, 1).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "Renamed");

        // The file really contains two reads of the same state.
        assert_eq!(list(&path).unwrap(), servers);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_root_and_server_fields_survive() {
        let path = temp_path("preserve");
        let root = Nbt::Compound(vec![
            ("customRoot".into(), Nbt::Int(42)),
            (
                "servers".into(),
                Nbt::List {
                    elem_type: TAG_COMPOUND,
                    items: vec![Nbt::Compound(vec![
                        ("name".into(), Nbt::String("A".into())),
                        ("ip".into(), Nbt::String("a.example.com".into())),
                        ("hidden".into(), Nbt::Byte(1)),
                        ("futureField".into(), Nbt::String("keep".into())),
                    ])],
                },
            ),
        ]);
        let _ = save(&path, &root);

        let servers = add(&path, "B", "b.example.com").unwrap();
        assert_eq!(servers.len(), 2);
        assert!(servers[0].hidden);

        let servers = update(&path, 0, "A2", "a2.example.com").unwrap();
        assert_eq!(servers[0].name, "A2");
        let servers = delete(&path, 1).unwrap();
        assert_eq!(servers.len(), 1);

        let stem = std::fs::File::open(&path).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(stem);
        let parsed = parse_document(&mut decoder).unwrap();
        let Nbt::Compound(fields) = &parsed else {
            panic!()
        };
        assert!(fields.iter().any(|(k, _)| k == "customRoot"));
        let Nbt::List { items, .. } = &fields.iter().find(|(k, _)| k == "servers").unwrap().1
        else {
            panic!()
        };
        let Nbt::Compound(first) = &items[0] else {
            panic!()
        };
        assert!(first
            .iter()
            .any(|(k, v)| k == "futureField" && *v == Nbt::String("keep".into())));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn external_change_between_read_and_write_is_detected() {
        let path = temp_path("conflict");
        let _ = std::fs::remove_file(&path);
        add(&path, "A", "a.example.com").unwrap();

        // The caller parses the current file and keeps its exact version.
        let (root, version) = load_root_versioned(&path).unwrap();

        // Another process writes a newer list between that read and the save.
        add(&path, "External", "external.example.com").unwrap();

        let err = save_if_unchanged(&path, &root, &version).unwrap_err();
        assert!(
            matches!(err, LauncherError::Conflict(_)),
            "expected a structured conflict, got: {err}"
        );

        // The newer file is intact; the stale tree was not written.
        let names: Vec<String> = list(&path).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["A", "External"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fingerprints_distinguish_rows_with_the_same_name_and_address() {
        let path = temp_path("fingerprint_distinct");
        let _ = std::fs::remove_file(&path);
        let root = Nbt::Compound(vec![(
            "servers".to_string(),
            Nbt::List {
                elem_type: TAG_COMPOUND,
                items: vec![
                    Nbt::Compound(vec![
                        ("name".into(), Nbt::String("Same".into())),
                        ("ip".into(), Nbt::String("same.example.com".into())),
                        ("futureField".into(), Nbt::String("one".into())),
                    ]),
                    Nbt::Compound(vec![
                        ("name".into(), Nbt::String("Same".into())),
                        ("ip".into(), Nbt::String("same.example.com".into())),
                        ("futureField".into(), Nbt::String("two".into())),
                    ]),
                ],
            },
        )]);
        save(&path, &root).unwrap();
        let entries = list(&path).unwrap();
        assert_eq!(entries[0].name, entries[1].name);
        assert_eq!(entries[0].ip, entries[1].ip);
        assert_ne!(entries[0].fingerprint, entries[1].fingerprint);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fingerprint_change_in_an_unknown_field_rejects_a_stale_update() {
        let path = temp_path("fingerprint_changed");
        let _ = std::fs::remove_file(&path);
        let mut root = Nbt::Compound(vec![(
            "servers".to_string(),
            Nbt::List {
                elem_type: TAG_COMPOUND,
                items: vec![Nbt::Compound(vec![
                    ("name".into(), Nbt::String("A".into())),
                    ("ip".into(), Nbt::String("a.example.com".into())),
                    ("futureField".into(), Nbt::String("old".into())),
                ])],
            },
        )]);
        save(&path, &root).unwrap();
        let old_fingerprint = list(&path).unwrap()[0].fingerprint.clone();

        // Simulate another writer changing only an unknown field.
        let Nbt::Compound(fields) = &mut root else {
            panic!()
        };
        let Nbt::List { items, .. } = &mut fields[0].1 else {
            panic!()
        };
        let Nbt::Compound(server) = &mut items[0] else {
            panic!()
        };
        for (key, value) in server.iter_mut() {
            if key == "futureField" {
                *value = Nbt::String("new".into());
            }
        }
        save(&path, &root).unwrap();

        let err =
            update_fingerprint(&path, 0, &old_fingerprint, "A2", "a2.example.com").unwrap_err();
        assert!(matches!(err, LauncherError::Conflict(_)));
        let entries = list(&path).unwrap();
        assert_eq!(entries[0].name, "A");
        assert_eq!(entries[0].ip, "a.example.com");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn identical_duplicate_rows_are_ambiguous_and_not_modified() {
        let path = temp_path("fingerprint_duplicate");
        let _ = std::fs::remove_file(&path);
        let server = || {
            Nbt::Compound(vec![
                ("name".into(), Nbt::String("Same".into())),
                ("ip".into(), Nbt::String("same.example.com".into())),
            ])
        };
        let root = Nbt::Compound(vec![(
            "servers".to_string(),
            Nbt::List {
                elem_type: TAG_COMPOUND,
                items: vec![server(), server()],
            },
        )]);
        save(&path, &root).unwrap();
        let fingerprint = list(&path).unwrap()[0].fingerprint.clone();
        assert_eq!(list(&path).unwrap()[0].fingerprint, fingerprint);

        let err = update_fingerprint(&path, 0, &fingerprint, "Changed", "changed.example.com")
            .unwrap_err();
        assert!(matches!(err, LauncherError::Conflict(_)));
        let err = delete_fingerprint(&path, 0, &fingerprint).unwrap_err();
        assert!(matches!(err, LauncherError::Conflict(_)));
        assert_eq!(list(&path).unwrap().len(), 2);
        assert_eq!(list(&path).unwrap()[0].name, "Same");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn checked_edit_rejects_a_stale_row_identity() {
        let path = temp_path("checked");
        let _ = std::fs::remove_file(&path);
        add(&path, "A", "a.example.com").unwrap();
        add(&path, "B", "b.example.com").unwrap();

        // The user selected row 0 when it was B, but the file now has A first.
        let err = update_checked(
            &path,
            0,
            "B",
            "b.example.com",
            "Renamed",
            "renamed.example.com",
        )
        .unwrap_err();
        assert!(matches!(err, LauncherError::Conflict(_)));

        let err = delete_checked(&path, 0, "B", "b.example.com").unwrap_err();
        assert!(matches!(err, LauncherError::Conflict(_)));

        let names: Vec<String> = list(&path).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["A", "B"]);

        let updated =
            update_checked(&path, 0, "A", "a.example.com", "A2", "a2.example.com").unwrap();
        assert_eq!(updated[0].name, "A2");
        delete_checked(&path, 1, "B", "b.example.com").unwrap();
        assert_eq!(list(&path).unwrap().len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deleted_entries_are_not_restored_by_a_later_write() {
        let path = temp_path("no_resurrect");
        let _ = std::fs::remove_file(&path);
        add(&path, "A", "a.example.com").unwrap();
        add(&path, "B", "b.example.com").unwrap();
        delete(&path, 0).unwrap();
        add(&path, "C", "c.example.com").unwrap();

        let names: Vec<String> = list(&path).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["B", "C"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupted_file_reports_an_error() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"this is not gzip data").unwrap();
        let err = list(&path).unwrap_err().to_string();
        assert!(err.contains("servers.dat"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plain_uncompressed_nbt_is_read_and_written_back_as_gzip() {
        // Some third-party tools write servers.dat without gzip; the entry
        // must load, and every write must go back in the game's own format.
        let path = temp_path("plain");
        let _ = std::fs::remove_file(&path);
        let root = Nbt::Compound(vec![(
            "servers".to_string(),
            Nbt::List {
                elem_type: TAG_COMPOUND,
                items: vec![Nbt::Compound(vec![
                    ("name".to_string(), Nbt::String("Plain".into())),
                    ("ip".to_string(), Nbt::String("plain.example.com".into())),
                ])],
            },
        )]);
        let mut plain = Vec::new();
        plain.push(TAG_COMPOUND);
        write_string(&mut plain, "").unwrap();
        write_payload(&root, &mut plain).unwrap();
        std::fs::write(&path, &plain).unwrap();

        let listed = list(&path).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Plain");
        assert_eq!(listed[0].ip, "plain.example.com");

        add(&path, "Second", "second.example.com").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(&[0x1f, 0x8b]));
        assert_eq!(list(&path).unwrap().len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_file_is_treated_as_an_empty_list() {
        let path = temp_path("empty");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"").unwrap();
        assert!(list(&path).unwrap().is_empty());
        assert_eq!(add(&path, "First", "first.example.com").unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_list_with_end_element_type_is_normalized() {
        // An empty `servers` list may legitimately declare element type 0
        // (TAG_End). Adding the first server must switch it to TAG_COMPOUND,
        // otherwise the game's parser rejects the file.
        let path = temp_path("empty_list_type");
        let _ = std::fs::remove_file(&path);
        let root = Nbt::Compound(vec![(
            "servers".to_string(),
            Nbt::List {
                elem_type: TAG_END,
                items: vec![],
            },
        )]);
        let _ = save(&path, &root);

        let servers = add(&path, "Fresh", "fresh.example.com").unwrap();
        assert_eq!(servers.len(), 1);
        let parsed = load_root(&path).unwrap();
        let Nbt::Compound(fields) = &parsed else {
            panic!()
        };
        let Nbt::List { elem_type, items } =
            &fields.iter().find(|(k, _)| k == "servers").unwrap().1
        else {
            panic!()
        };
        assert_eq!(*elem_type, TAG_COMPOUND);
        assert_eq!(items.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn foreign_list_type_is_reported_not_corrupted() {
        // A `servers` key holding a list of some other type cannot receive
        // compound entries: the edit must fail cleanly, leaving no file.
        let path = temp_path("wrong_list_type");
        let _ = std::fs::remove_file(&path);
        let root = Nbt::Compound(vec![(
            "servers".to_string(),
            Nbt::List {
                elem_type: TAG_INT,
                items: vec![Nbt::Int(1)],
            },
        )]);
        let _ = save(&path, &root);
        let err = add(&path, "X", "x.example.com").unwrap_err().to_string();
        assert!(err.contains("unexpected servers entry"));
        // The original file is untouched.
        let parsed = load_root(&path).unwrap();
        let Nbt::Compound(fields) = &parsed else {
            panic!()
        };
        let Nbt::List { elem_type, items } =
            &fields.iter().find(|(k, _)| k == "servers").unwrap().1
        else {
            panic!()
        };
        assert_eq!(*elem_type, TAG_INT);
        assert_eq!(items.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_order_survives_restart_like_rereads() {
        // The order shown to the user is the order in the file; reopening the
        // file (a launcher or game restart) must reproduce it exactly, and an
        // in-place edit must not move the entry.
        let path = temp_path("order");
        let _ = std::fs::remove_file(&path);
        add(&path, "A", "a.example.com").unwrap();
        add(&path, "B", "b.example.com").unwrap();
        add(&path, "C", "c.example.com").unwrap();
        update(&path, 1, "B2", "b2.example.com").unwrap();
        let names: Vec<String> = list(&path).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["A", "B2", "C"]);
        delete(&path, 0).unwrap();
        let names: Vec<String> = list(&path).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["B2", "C"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validation_rejects_bad_input() {
        let path = temp_path("validate");
        let _ = std::fs::remove_file(&path);
        assert!(add(&path, "  ", "host").is_err());
        assert!(add(&path, "name", "   ").is_err());
        assert!(add(&path, "name", "bad host with spaces").is_err());
        assert!(add(&path, &"n".repeat(200), "host").is_err());
        assert!(update(&path, 0, "name", "host").is_err());
        assert!(delete(&path, 0).is_err());
        assert!(!path.exists());
    }
}
