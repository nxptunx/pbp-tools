// SPDX-License-Identifier: GPL-2.0-only

use pbp_tools::{pbp, sfo};

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

const PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {}", err);
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_help();
        return Ok(());
    }

    match args[0].as_str() {
        "-V" | "--version" => {
            println!("pbp-tools {}", PROGRAM_VERSION);
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        "info" => cmd_info(&args[1..]),
        "extract" => cmd_extract(&args[1..]),
        "pack" => cmd_pack(&args[1..]),
        "sfo" => cmd_sfo(&args[1..]),
        other => Err(format!(
            "unknown command '{}'. Run `pbp help` for usage.",
            other
        )),
    }
}

// ---------------------------------------------------------------------------
// PBP commands
// ---------------------------------------------------------------------------

fn cmd_info(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &[], &["--sfo", "--structure"])?;
    if parsed.positional.len() != 1 {
        return Err("usage: pbp info <file.pbp> [--sfo] [--structure]".to_string());
    }
    let path = Path::new(&parsed.positional[0]);
    let archive = pbp::PbpArchive::from_file(path)?;

    println!("PBP:        {}", path.display());
    println!("Magic:      {}", pbp::hex(&archive.magic));
    println!(
        "Version:    0x{:08X} ({})",
        archive.version,
        pbp::hex(&archive.version.to_le_bytes())
    );
    println!("Files:      {}", archive.present().count());
    println!();
    println!("  {:<11} {:>10} {:>10}  Status", "Slot", "Offset", "Size");
    println!("  {}", "-".repeat(48));
    for (i, file) in archive.files.iter().enumerate() {
        match file {
            Some(f) => println!("  {:<11} {:>10} {:>10}  present", f.name, f.offset, f.size),
            None => println!(
                "  {:<11} {:>10} {:>10}  empty",
                pbp::SLOT_NAMES[i],
                archive.offsets[i],
                0
            ),
        }
    }

    if parsed.flags.contains("--sfo") {
        let sfo_bytes = &archive.files[0]
            .as_ref()
            .ok_or("PBP has no PARAM.SFO")?
            .data;
        let sfo = sfo::SfoFile::parse(sfo_bytes)?;
        println!();
        println!("Embedded PARAM.SFO:");
        print_sfo(&sfo, parsed.flags.contains("--structure"));
    }
    Ok(())
}

fn cmd_extract(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &["--output", "--only"], &["--force"])?;
    if parsed.positional.len() != 1 {
        return Err(
            "usage: pbp extract <file.pbp> [-o DIR] [--only NAME[,NAME...]] [--force]".to_string(),
        );
    }
    let path = Path::new(&parsed.positional[0]);
    let archive = pbp::PbpArchive::from_file(path)?;

    let selected: Option<Vec<usize>> = match parsed.values.get("--only") {
        Some(spec) => {
            let mut slots = Vec::new();
            for name in spec.split(',') {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                let idx = pbp::slot_for_filename(name)
                    .ok_or_else(|| format!("unknown PBP slot '{}'", name))?;
                if !slots.contains(&idx) {
                    slots.push(idx);
                }
            }
            if slots.is_empty() {
                return Err("--only needs at least one slot name".to_string());
            }
            Some(slots)
        }
        None => None,
    };

    let explicit_dir = parsed.values.get("--output").map(PathBuf::from);
    let force = parsed.flags.contains("--force");
    let dir = match explicit_dir.clone() {
        Some(d) => {
            if d.exists() && !force {
                return Err(format!(
                    "output directory {} already exists; choose another -o or pass --force",
                    d.display()
                ));
            }
            d
        }
        None => next_extract_dir()?,
    };

    fs::create_dir_all(&dir).map_err(|e| format!("unable to create {}: {}", dir.display(), e))?;

    let mut written = Vec::new();
    for file in archive.present() {
        if let Some(slots) = &selected {
            if !slots.contains(&file.slot) {
                continue;
            }
        }
        let out_path = dir.join(file.name);
        fs::write(&out_path, &file.data)
            .map_err(|e| format!("unable to write {}: {}", out_path.display(), e))?;
        written.push(out_path);
    }

    if written.is_empty() {
        return Err("no files matched the selection".to_string());
    }
    println!("Files extracted to {}", dir.display());
    for path in written {
        println!("  {}", path.display());
    }
    Ok(())
}

fn cmd_pack(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &["--pbp-version"], &["--skip-sfo-check"])?;
    if parsed.positional.len() < 3 {
        return Err(
            "usage: pbp pack <out.pbp> <PARAM.SFO> <file> [file...] [--pbp-version N] [--skip-sfo-check]"
                .to_string(),
        );
    }

    let out_path = Path::new(&parsed.positional[0]);
    let inputs = &parsed.positional[1..];
    let has_param = inputs.iter().any(|p| pbp::slot_for_filename(p) == Some(0));
    // Convenience: an unrecognised first input is treated as PARAM.SFO, so
    // `pbp pack out.pbp edited.sfo ...` works after an SFO edit/rename.
    let force_first_as_param = !has_param && pbp::slot_for_filename(&inputs[0]).is_none();

    let mut files: [Option<pbp::PbpFile>; 8] = std::array::from_fn(|_| None);

    for (input_index, input) in inputs.iter().enumerate() {
        let path = Path::new(input);
        let idx = match pbp::slot_for_filename(input) {
            Some(i) => i,
            None if force_first_as_param && input_index == 0 => 0,
            None => {
                return Err(format!(
                    "file '{}' does not match a PBP slot; expected one of: {}",
                    input,
                    pbp::SLOT_NAMES.join(", ")
                ))
            }
        };
        if files[idx].is_some() {
            return Err(format!("duplicate slot: {}", pbp::SLOT_NAMES[idx]));
        }
        let data =
            fs::read(path).map_err(|e| format!("unable to read {}: {}", path.display(), e))?;
        if data.is_empty() {
            return Err(format!(
                "{} is empty; empty slots cannot be packed",
                path.display()
            ));
        }
        files[idx] = Some(pbp::PbpFile {
            slot: idx,
            name: pbp::SLOT_NAMES[idx],
            offset: 0,
            size: data.len(),
            data,
        });
    }

    if files[0].is_none() {
        return Err("one of the input files must be PARAM.SFO".to_string());
    }
    if pbp::present_count(&files) < 2 {
        return Err("Every PBP archive needs at least a PARAM.SFO and one other file.".to_string());
    }

    let version = match parsed.values.get("--pbp-version") {
        Some(v) => parse_u32(v, "PBP version")?,
        None => pbp::DEFAULT_VERSION,
    };

    if !parsed.flags.contains("--skip-sfo-check") {
        let sfo_bytes = &files[0].as_ref().unwrap().data;
        sfo::SfoFile::parse(sfo_bytes).map_err(|e| {
            format!(
                "PARAM.SFO is invalid: {}. Use --skip-sfo-check to pack it anyway.",
                e
            )
        })?;
    }

    let bytes = pbp::build(version, &files)?;
    fs::write(out_path, &bytes)
        .map_err(|e| format!("unable to write {}: {}", out_path.display(), e))?;
    println!(
        "Wrote {} ({} bytes, {} files)",
        out_path.display(),
        bytes.len(),
        pbp::present_count(&files)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// SFO commands
// ---------------------------------------------------------------------------

fn cmd_sfo(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        print_sfo_help();
        return Ok(());
    }
    match args[0].as_str() {
        "info" | "dump" => cmd_sfo_info(&args[1..]),
        "get" => cmd_sfo_get(&args[1..]),
        "set" => cmd_sfo_set(&args[1..]),
        "add" => cmd_sfo_add(&args[1..]),
        "delete" | "del" | "rm" => cmd_sfo_delete(&args[1..]),
        "extract" => cmd_sfo_extract(&args[1..]),
        "help" | "-h" | "--help" => {
            print_sfo_help();
            Ok(())
        }
        other => Err(format!(
            "unknown SFO command '{}'. Run `pbp sfo help` for usage.",
            other
        )),
    }
}

fn cmd_sfo_info(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &[], &["--structure"])?;
    if parsed.positional.len() != 1 {
        return Err("usage: pbp sfo info <file.sfo> [--structure]".to_string());
    }
    let path = Path::new(&parsed.positional[0]);
    let file = sfo::SfoFile::from_file(path)?;
    println!("SFO:     {}", path.display());
    println!(
        "Version: 0x{:08X} ({})",
        file.version,
        pbp::hex(&file.version.to_le_bytes())
    );
    println!("Entries: {}", file.entries.len());
    println!();
    print_sfo(&file, parsed.flags.contains("--structure"));
    Ok(())
}

fn cmd_sfo_get(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &[], &["--hex"])?;
    if parsed.positional.len() != 2 {
        return Err("usage: pbp sfo get <file.sfo> <key> [--hex]".to_string());
    }
    let file = sfo::SfoFile::from_file(Path::new(&parsed.positional[0]))?;
    let key = &parsed.positional[1];
    let entry = file
        .get(key)
        .ok_or_else(|| format!("key '{}' not found in SFO", key))?;
    if parsed.flags.contains("--hex") {
        println!("{}", pbp::hex(&entry.data));
    } else {
        println!("{}", entry.display_value());
    }
    Ok(())
}

fn cmd_sfo_set(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &["--output"], &[])?;
    if parsed.positional.len() != 3 {
        return Err("usage: pbp sfo set <file.sfo> <key> <value> [-o OUT]".to_string());
    }
    let path = Path::new(&parsed.positional[0]);
    let key = &parsed.positional[1];
    let value = &parsed.positional[2];
    let mut file = sfo::SfoFile::from_file(path)?;
    let new_data = {
        let entry = file
            .get(key)
            .ok_or_else(|| format!("key '{}' not found; use `sfo add` to create it", key))?;
        sfo::value_from_type(entry, value)?
    };
    file.get_mut(key).unwrap().data = new_data;
    write_sfo_result(&file, &parsed, path)
}

fn cmd_sfo_add(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &["--output"], &[])?;
    if parsed.positional.len() != 4 {
        return Err(
            "usage: pbp sfo add <file.sfo> <key> <type:0|2|4> <value> [-o OUT]".to_string(),
        );
    }
    let path = Path::new(&parsed.positional[0]);
    let key = &parsed.positional[1];
    let data_type = sfo::parse_type(&parsed.positional[2])?;
    let value = &parsed.positional[3];
    let mut file = sfo::SfoFile::from_file(path)?;
    if file.get(key).is_some() {
        return Err(format!("key '{}' already exists; use `sfo set`", key));
    }
    let data = match data_type {
        sfo::TYPE_STRING => sfo::encode_string(key, value),
        sfo::TYPE_INT => {
            let temp = sfo::new_int_entry(key, 0);
            sfo::value_from_type(&temp, value)?
        }
        sfo::TYPE_BINARY => sfo::parse_binary(value)?,
        other => return Err(format!("unsupported data type {}", other)),
    };
    file.entries.push(sfo::SfoEntry {
        key: key.clone(),
        alignment: sfo::DEFAULT_ALIGNMENT,
        data_type,
        data,
        raw_padded_len: 0,
        raw_value_offset: 0,
        raw_key_offset: 0,
    });
    write_sfo_result(&file, &parsed, path)
}

fn cmd_sfo_delete(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &["--output"], &[])?;
    if parsed.positional.len() != 2 {
        return Err("usage: pbp sfo delete <file.sfo> <key> [-o OUT]".to_string());
    }
    let path = Path::new(&parsed.positional[0]);
    let key = &parsed.positional[1];
    let mut file = sfo::SfoFile::from_file(path)?;
    let before = file.entries.len();
    file.entries.retain(|e| e.key != *key);
    if file.entries.len() == before {
        return Err(format!("key '{}' not found in SFO", key));
    }
    write_sfo_result(&file, &parsed, path)
}

fn cmd_sfo_extract(args: &[String]) -> Result<(), String> {
    let parsed = parse_options(args, &["--output"], &[])?;
    if parsed.positional.len() != 1 {
        return Err("usage: pbp sfo extract <file.pbp> [-o PARAM.SFO]".to_string());
    }
    let path = Path::new(&parsed.positional[0]);
    let archive = pbp::PbpArchive::from_file(path)?;
    let data = &archive.files[0]
        .as_ref()
        .ok_or("PBP has no PARAM.SFO")?
        .data;
    let out = parsed
        .values
        .get("--output")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("PARAM.SFO"));
    fs::write(&out, data).map_err(|e| format!("unable to write {}: {}", out.display(), e))?;
    println!("Wrote {}", out.display());
    Ok(())
}

fn write_sfo_result(file: &sfo::SfoFile, parsed: &Parsed, input: &Path) -> Result<(), String> {
    let out = parsed
        .values
        .get("--output")
        .map(PathBuf::from)
        .unwrap_or_else(|| input.to_path_buf());
    file.write_file(&out)?;
    println!("Wrote {}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

fn print_sfo(file: &sfo::SfoFile, structure: bool) {
    println!(
        "  {:<3} {:<24} {:<8} {:<8}  {}",
        "#", "Key", "Type", "Size", "Value"
    );
    println!("  {}", "-".repeat(68));
    for (i, entry) in file.entries.iter().enumerate() {
        println!(
            "  {:<3} {:<24} {:<8} {:<8}  {}",
            i,
            entry.key,
            entry.type_name(),
            entry.data.len(),
            escape_display(&entry.display_value())
        );
    }
    if structure {
        println!();
        println!("  Raw structure (as read from the file):");
        println!(
            "  {:<3} {:<24} {:>7} {:>4} {:>4} {:>10} {:>10} {:>10}",
            "#", "Key", "KeyOff", "Align", "Type", "DataLen", "PaddedLen", "ValueOff"
        );
        for (i, entry) in file.entries.iter().enumerate() {
            println!(
                "  {:<3} {:<24} {:>7} {:>4} {:>4} {:>10} {:>10} {:>10}",
                i,
                entry.key,
                entry.raw_key_offset,
                entry.alignment,
                entry.data_type,
                entry.data.len(),
                entry.raw_padded_len,
                entry.raw_value_offset
            );
        }
    }
}

fn escape_display(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn next_extract_dir() -> Result<PathBuf, String> {
    for n in 1..=9999 {
        let path = PathBuf::from(format!("Extract{}", n));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("could not find an available ExtractN directory".to_string())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Parsed {
    positional: Vec<String>,
    values: HashMap<String, String>,
    flags: HashSet<String>,
}

fn parse_options(
    args: &[String],
    value_options: &[&str],
    bool_options: &[&str],
) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut i = 0;
    let mut options_enabled = true;

    while i < args.len() {
        let arg = &args[i];
        if options_enabled && arg == "--" {
            options_enabled = false;
            i += 1;
            continue;
        }

        if options_enabled && arg.starts_with("--") {
            let (name, inline) = match arg.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (arg.clone(), None),
            };
            if value_options.contains(&name.as_str()) {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("option {} needs a value", name))?
                    }
                };
                parsed.values.insert(name, value);
            } else if bool_options.contains(&name.as_str()) {
                if inline.is_some() {
                    return Err(format!("option {} does not take a value", name));
                }
                parsed.flags.insert(name);
            } else {
                return Err(format!("unknown option '{}'", name));
            }
        } else if options_enabled && arg.starts_with('-') && arg.len() > 1 {
            let name = match arg.as_str() {
                "-o" => "--output",
                "-f" => "--force",
                "-h" => "--help",
                "--" => unreachable!(),
                other => return Err(format!("unknown option '{}'", other)),
            };
            if value_options.contains(&name) {
                i += 1;
                let value = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| format!("option {} needs a value", arg))?;
                parsed.values.insert(name.to_string(), value);
            } else {
                parsed.flags.insert(name.to_string());
            }
        } else {
            parsed.positional.push(arg.clone());
        }
        i += 1;
    }
    Ok(parsed)
}

fn parse_u32(value: &str, label: &str) -> Result<u32, String> {
    let cleaned = value.trim().replace('_', "");
    let parsed = if let Some(hexpart) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        u32::from_str_radix(hexpart, 16)
    } else {
        cleaned.parse::<u32>()
    }
    .map_err(|e| format!("invalid {} '{}': {}", label, value, e))?;
    Ok(parsed)
}

fn print_help() {
    println!(
        "pbp {} - a little CLI for PSP archives and SFO files",
        PROGRAM_VERSION
    );
    println!();
    println!("Usage:");
    println!("  pbp info <file.pbp> [--sfo] [--structure]");
    println!("  pbp extract <file.pbp> [-o DIR] [--only NAME[,NAME...]] [--force]");
    println!("  pbp pack <out.pbp> <PARAM.SFO> <file> [file...] [--pbp-version N]");
    println!("           [--skip-sfo-check]");
    println!("  pbp sfo <command> [options]");
    println!();
    println!("PBP slots: {}", pbp::SLOT_NAMES.join(", "));
    println!();
    println!("SFO commands:");
    println!("  pbp sfo info <file.sfo> [--structure]");
    println!("  pbp sfo get <file.sfo> <key> [--hex]");
    println!("  pbp sfo set <file.sfo> <key> <value> [-o OUT]");
    println!("  pbp sfo add <file.sfo> <key> <0|2|4> <value> [-o OUT]");
    println!("  pbp sfo delete <file.sfo> <key> [-o OUT]");
    println!("  pbp sfo extract <file.pbp> [-o PARAM.SFO]");
    println!();
    println!("Notes:");
    println!("  * No -o? We'll make Extract1, Extract2, ... automaticaly for you.");
    println!("  * SFO strings understand <#decimal>, so <#13><#10> means CRLF.");
    println!("  * Binary values take hex (41 42, 0x4142) or @path to read a file.");
    println!("  * SFO files get rebuilt aligned and sorted when you save. Trust me.");
}

fn print_sfo_help() {
    println!("SFO commands:");
    println!("  info <file.sfo> [--structure]       List entries and optionally raw structure");
    println!("  get <file.sfo> <key> [--hex]        Print one value");
    println!("  set <file.sfo> <key> <value>        Change an existing value (type preserved)");
    println!("  add <file.sfo> <key> <type> <value> Add an entry; type 0=binary, 2=string, 4=int");
    println!("  delete <file.sfo> <key>             Remove an entry");
    println!("  extract <file.pbp> [-o PARAM.SFO]   Pull PARAM.SFO out of a PBP archive");
    println!();
    println!(
        "All modifying commands accept -o/--output. Without it they overwrite the input file."
    );
}
