use std::{env, fs, path::PathBuf};

fn word(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn dword(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn resource(output: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    // Win32 RESOURCEHEADER with ordinal type/name, neutral language, DWORD alignment.
    // Both MSVC link.exe and LLVM LLD consume .res files directly.
    dword(
        output,
        data.len().try_into().expect("resource is too large"),
    );
    dword(output, 32);
    word(output, 0xffff);
    word(output, kind);
    word(output, 0xffff);
    word(output, id);
    dword(output, 0);
    word(output, if kind == 0 { 0 } else { 0x1030 });
    word(output, 0);
    dword(output, 0);
    dword(output, 0);
    output.extend_from_slice(data);
    while !output.len().is_multiple_of(4) {
        output.push(0);
    }
}

fn main() {
    println!("cargo:rerun-if-changed=assets/branding/icon.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = fs::read("assets/branding/icon.ico").expect("missing launcher icon");
    assert_eq!(
        icon.get(..4),
        Some([0, 0, 1, 0].as_slice()),
        "invalid ICO header"
    );
    let count = u16::from_le_bytes(icon[4..6].try_into().unwrap());
    assert!((1..=64).contains(&count), "invalid ICO frame count");
    let mut output = Vec::new();
    resource(&mut output, 0, 0, &[]);
    let mut group = icon[..6].to_vec();
    for index in 0..usize::from(count) {
        let start = 6 + 16 * index;
        let entry = icon
            .get(start..start + 16)
            .expect("truncated ICO directory");
        let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        let end = offset.checked_add(length).expect("invalid ICO frame size");
        let data = icon.get(offset..end).expect("truncated ICO image");
        let id = (index + 1) as u16;
        resource(&mut output, 3, id, data); // RT_ICON
        group.extend_from_slice(&entry[..12]);
        word(&mut group, id);
    }
    resource(&mut output, 14, 1, &group); // RT_GROUP_ICON
    let target = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("launcher-icon.res");
    fs::write(&target, output).expect("could not write launcher icon resources");
    println!("cargo:rustc-link-arg-bin=ets-launcher={}", target.display());
}
