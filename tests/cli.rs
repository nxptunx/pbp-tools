// SPDX-License-Identifier: GPL-2.0-only

use pbp_tools::{pbp, sfo};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pbp-rs-cli-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_pbp"))
        .args(args)
        .output()
        .expect("failed to run pbp");
    assert!(
        output.status.success(),
        "command {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn full_workflow() {
    let dir = temp_dir();
    let sfo_path = dir.join("PARAM.SFO");
    let icon_path = dir.join("ICON0.PNG");
    let psp_path = dir.join("DATA.PSP");
    let pbp_path = dir.join("sample.pbp");

    let sfo_file = sfo::SfoFile {
        version: sfo::DEFAULT_VERSION,
        entries: vec![
            sfo::new_string_entry("TITLE", "Fixture Game"),
            sfo::new_int_entry("CATEGORY", 1),
        ],
    };
    fs::write(&sfo_path, sfo_file.to_bytes().unwrap()).unwrap();
    fs::write(&icon_path, b"PNGDATA").unwrap();
    fs::write(&psp_path, b"PSPDATA").unwrap();

    let mut slots: [Option<pbp::PbpFile>; 8] = std::array::from_fn(|_| None);
    for (i, path) in [(0, &sfo_path), (1, &icon_path), (6, &psp_path)] {
        let data = fs::read(path).unwrap();
        slots[i] = Some(pbp::PbpFile {
            slot: i,
            name: pbp::SLOT_NAMES[i],
            offset: 0,
            size: data.len(),
            data,
        });
    }
    fs::write(&pbp_path, pbp::build(pbp::DEFAULT_VERSION, &slots).unwrap()).unwrap();

    let sfo_arg = sfo_path.to_str().unwrap();
    let pbp_arg = pbp_path.to_str().unwrap();
    let icon_arg = icon_path.to_str().unwrap();
    let psp_arg = psp_path.to_str().unwrap();

    let info = run(&["info", pbp_arg, "--sfo"]);
    assert!(info.contains("Fixture Game"), "{}", info);
    assert!(info.contains("ICON0.PNG"), "{}", info);

    let out_dir = dir.join("extracted");
    let extract = run(&["extract", pbp_arg, "-o", out_dir.to_str().unwrap()]);
    assert!(extract.contains("Files extracted to"), "{}", extract);
    assert_eq!(fs::read(out_dir.join("ICON0.PNG")).unwrap(), b"PNGDATA");

    let edited = dir.join("edited.sfo");
    run(&[
        "sfo",
        "set",
        sfo_arg,
        "TITLE",
        "Changed<#13><#10>Line2",
        "-o",
        edited.to_str().unwrap(),
    ]);
    let dumped = run(&["sfo", "get", edited.to_str().unwrap(), "TITLE"]);
    assert!(
        dumped.contains("Changed") && dumped.contains("Line2"),
        "{}",
        dumped
    );

    let repacked = dir.join("rebuilt.pbp");
    run(&[
        "pack",
        repacked.to_str().unwrap(),
        edited.to_str().unwrap(),
        icon_arg,
        psp_arg,
    ]);
    let info2 = run(&["info", repacked.to_str().unwrap(), "--sfo"]);
    assert!(info2.contains("Changed\\r\\nLine2"), "{}", info2);

    fs::remove_dir_all(dir).unwrap();
}
