# pbp

A tiny command-line tool for PSP `EBOOT.PBP` files and the `PARAM.SFO` tucked inside them.

I got tired of Windows-only utilities for this, so here's one that just works on Linux and Windows. No GUI to babysit, no pile of DLLs, no dependencies at all - it's plain Rust and the standard library. Builds offline, runs anywhere Rust runs.

## Build

```sh
cargo build --release
./target/release/pbp --help
```

For Windows:

```sh
cargo build --release --target x86_64-pc-windows-gnu
# or, if you prefer MSVC
cargo build --release --target x86_64-pc-windows-msvc
```

Prebuilt binaries from this workspace are sitting in `dist/`:

- `dist/pbp-linux-aarch64`
- `dist/pbp-windows-x86_64.exe`

## Commands

```text
pbp info <file.pbp> [--sfo] [--structure]
pbp extract <file.pbp> [-o DIR] [--only NAME[,NAME...]] [--force]
pbp pack <out.pbp> <PARAM.SFO> <file> [file...] [--pbp-version N] [--skip-sfo-check]

pbp sfo info <file.sfo> [--structure]
pbp sfo get <file.sfo> <key> [--hex]
pbp sfo set <file.sfo> <key> <value> [-o OUT]
pbp sfo add <file.sfo> <key> <0|2|4> <value> [-o OUT]
pbp sfo delete <file.sfo> <key> [-o OUT]
pbp sfo extract <file.pbp> [-o PARAM.SFO]
```

Slots are `PARAM.SFO`, `ICON0.PNG`, `ICON1.PMF`, `PIC0.PNG`, `PIC1.PNG`, `SND0.AT3`, `DATA.PSP` and `DATA.PSAR`.

## Things I use it for

Have a look at what's inside:

```sh
pbp info EBOOT.PBP --sfo
```

Pull everything out. Without `-o` it makes `Extract1`, then `Extract2`, and so on:

```sh
pbp extract EBOOT.PBP
```

Just want the icon and the game binary:

```sh
pbp extract EBOOT.PBP -o out --only icon0.png,data.psp
```

Put it back together after poking at the SFO:

```sh
pbp pack rebuilt.pbp PARAM.SFO ICON0.PNG DATA.PSP
```

Edit a title, including the old `<#decimal>` trick for line breaks:

```sh
pbp sfo set PARAM.SFO TITLE 'My Game<#13><#10>Second line'
```

Change a number or a binary field:

```sh
pbp sfo set PARAM.SFO PARENTAL_LEVEL 5
pbp sfo set PARAM.SFO SAVEDATA_PARAMS 0x01020304
```

Add and remove keys:

```sh
pbp sfo add PARAM.SFO CUSTOM_INT 4 42
pbp sfo delete PARAM.SFO CUSTOM_INT -o cleaned.sfo
```

## The formats, quickly

A PBP is about as simple as an archive gets. Forty bytes of header, then up to eight files glued together with no separators. The header holds a magic value, a version word, and eight offsets - one for each slot. Empty slots just repeat the previous real offset. Odd, but once you spot it the whole thing makes sense.

SFO files are a key/value table: a 20-byte header, a row of 16-byte index entries, a bunch of NUL-terminated keys, then the values. Values are padded out so everything lands on a 4-byte boundary. Types are `0` for binary, `2` for text and `4` for a 32-bit number.

## Small print

- You can recieve a quick overview with `info`, including the embedded SFO if you pass `--sfo`.
- Each present member lands as a seperate file in the output folder, named after its slot.
- `pack` figures out slots from filenames. If the first input isn't named like a slot, we assume it's the SFO - saves you a rename most of the time.
- `pack` checks the SFO before writing. `--skip-sfo-check` is there if you're feeling brave.
- SFO writes rebuild a clean, aligned file with keys sorted, which is what the PSP expects.
- `Category = "MS"` is stored without its trailing NUL. Weird, but that's how it is, so that's what we do.
- Empty slots are skipped when packing. You need at least two files.
- It's a terminal tool, so no image preview window or drag-and-drop. Sorry.
- If an error occured reading a real EBOOT, `xxd -l 64 file.pbp` usually tells the story.

## License

GNU General Public License v2.0 only (`GPL-2.0-only`). See [LICENSE](LICENSE) for the full text.
