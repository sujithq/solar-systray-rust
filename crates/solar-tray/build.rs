//! Embeds the executable icon and version information as a compiled `.res` file, generated in
//! plain Rust so no resource compiler (rc.exe) or extra crate is needed.

use std::{env, fs, path::PathBuf};

#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

const ICON_SIZES: [u32; 7] = [16, 20, 24, 32, 48, 64, 128];
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const LANG_EN_US: u16 = 0x0409;
const CODEPAGE_UNICODE: u16 = 0x04B0;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("solar-systray.res");
    fs::write(&out, resources()).expect("write solar-systray.res");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

fn resources() -> Vec<u8> {
    let mut res = Vec::new();
    entry(&mut res, 0, 0, 0, &[]);

    let spec = icon::IconSpec {
        face: icon::Face::Symbol { glyph: icon::Glyph::Sun, color: icon::PRODUCING },
        go: false,
        warning: false,
    };
    let mut group = Vec::new();
    put16(&mut group, 0);
    put16(&mut group, 1);
    put16(&mut group, ICON_SIZES.len() as u16);
    for (index, &size) in ICON_SIZES.iter().enumerate() {
        let id = index as u16 + 1;
        let image = icon_image(&icon::render(&spec, size), size);
        group.push(size as u8);
        group.push(size as u8);
        group.push(0);
        group.push(0);
        put16(&mut group, 1);
        put16(&mut group, 32);
        put32(&mut group, image.len() as u32);
        put16(&mut group, id);
        entry(&mut res, RT_ICON, id, 0x1010, &image);
    }
    entry(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    entry(&mut res, RT_VERSION, 1, 0x0030, &version_info());
    res
}

/// Appends one RESOURCEHEADER (ordinal type and name) plus its DWORD-aligned data.
fn entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    put32(res, data.len() as u32);
    put32(res, 32);
    put16(res, 0xFFFF);
    put16(res, kind);
    put16(res, 0xFFFF);
    put16(res, name);
    put32(res, 0);
    put16(res, flags);
    put16(res, if kind == 0 { 0 } else { LANG_EN_US });
    put32(res, 0);
    put32(res, 0);
    res.extend_from_slice(data);
    pad4(res);
}

/// An ICO image: BITMAPINFOHEADER, bottom-up BGRA rows, then the 1-bit AND mask.
fn icon_image(pixels: &[[u8; 4]], size: u32) -> Vec<u8> {
    let n = size as usize;
    let mask_stride = n.div_ceil(32) * 4;
    let mut out = Vec::new();
    put32(&mut out, 40);
    put32(&mut out, size);
    put32(&mut out, size * 2);
    put16(&mut out, 1);
    put16(&mut out, 32);
    put32(&mut out, 0);
    put32(&mut out, (n * n * 4 + mask_stride * n) as u32);
    for _ in 0..4 {
        put32(&mut out, 0);
    }
    for y in (0..n).rev() {
        for &[r, g, b, a] in &pixels[y * n..(y + 1) * n] {
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    for y in (0..n).rev() {
        let mut row = vec![0u8; mask_stride];
        for x in 0..n {
            if pixels[y * n + x][3] == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

fn version_info() -> Vec<u8> {
    let part = |name: &str| env::var(name).ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    let (major, minor, patch) =
        (part("CARGO_PKG_VERSION_MAJOR"), part("CARGO_PKG_VERSION_MINOR"), part("CARGO_PKG_VERSION_PATCH"));
    let version_ms = (major << 16) | (minor & 0xFFFF);
    let version_ls = patch << 16;

    let mut fixed = Vec::new();
    for value in [
        0xFEEF_04BD, // signature
        0x0001_0000, // structure version
        version_ms,
        version_ls,
        version_ms,
        version_ls,
        0x3F,        // flags mask
        0,           // flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0,
    ] {
        put32(&mut fixed, value);
    }

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let strings: Vec<Vec<u8>> = [
        ("FileDescription", "Solar Systray"),
        ("FileVersion", version.as_str()),
        ("InternalName", "solar-systray"),
        ("OriginalFilename", "solar-systray.exe"),
        ("ProductName", "Solar Systray"),
        ("ProductVersion", version.as_str()),
    ]
    .iter()
    .map(|(key, value)| {
        let mut text = Vec::new();
        put_wide(&mut text, value);
        node(key, &text, (value.encode_utf16().count() + 1) as u16, 1, &[])
    })
    .collect();
    let table_key = format!("{LANG_EN_US:04X}{CODEPAGE_UNICODE:04X}");
    let table = node(&table_key, &[], 0, 1, &strings);
    let string_info = node("StringFileInfo", &[], 0, 1, &[table]);

    let mut translation = Vec::new();
    put16(&mut translation, LANG_EN_US);
    put16(&mut translation, CODEPAGE_UNICODE);
    let var = node("Translation", &translation, 4, 0, &[]);
    let var_info = node("VarFileInfo", &[], 0, 1, &[var]);

    node("VS_VERSION_INFO", &fixed, fixed.len() as u16, 0, &[string_info, var_info])
}

/// One version-info block: wLength, wValueLength, wType, key, value, then aligned children.
fn node(key: &str, value: &[u8], value_length: u16, kind: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0u8; 6];
    put_wide(&mut out, key);
    pad4(&mut out);
    out.extend_from_slice(value);
    for child in children {
        pad4(&mut out);
        out.extend_from_slice(child);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out[2..4].copy_from_slice(&value_length.to_le_bytes());
    out[4..6].copy_from_slice(&kind.to_le_bytes());
    out
}

fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Null-terminated UTF-16LE.
fn put_wide(out: &mut Vec<u8>, text: &str) {
    for unit in text.encode_utf16().chain([0]) {
        put16(out, unit);
    }
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}
