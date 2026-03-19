# **dovi_tool** [![CI](https://github.com/quietvoid/dovi_tool/workflows/CI/badge.svg)](https://github.com/quietvoid/dovi_tool/actions/workflows/ci.yml) [![Artifacts](https://github.com/quietvoid/dovi_tool/workflows/Artifacts/badge.svg)](https://github.com/quietvoid/dovi_tool/actions/workflows/release.yml)

**`dovi_tool`** is a CLI tool combining multiple utilities for working with **Dolby Vision in AV1** video bitstreams.

Supports both raw AV1 OBU streams and IVF-containerized AV1 (e.g. output from `aomenc`/`libaom`).

The **`dolby_vision`** crate is also hosted in this repo, see [README](dolby_vision/README.md) for use as a Rust/C lib.
The C compatible library is also known as **`libdovi`**, refer to the same document for building/installing.

&nbsp;

## **Building**

### **Toolchain**

Minimum Rust version: **1.88.0**

### **Dependencies**

On Linux, [fontconfig](https://github.com/yeslogic/fontconfig-rs#dependencies) is required.
Alternatively bypass system fonts with `--no-default-features --features internal-font`.

```console
cargo build --release
```

&nbsp;

## Usage

```
dovi_tool [OPTIONS] <SUBCOMMAND>
```

### Global options

| Flag | Description |
|------|-------------|
| `-m`, `--mode` | RPU conversion mode (see below) |
| `-c`, `--crop` | Set active area offsets to 0 (remove letterbox bars) |
| `--edit-config` | Path to an editor config JSON file (applied during `convert`) |

#### Conversion modes (`-m`)

| Mode | Description |
|------|-------------|
| `0` | Parse RPU, rewrite untouched |
| `1` | Convert to MEL compatible |
| `2` | Convert to profile 8.1 (removes luma/chroma mapping) |
| `3` | Convert profile 5 to 8.1 |
| `4` | Convert to profile 8.4 |
| `5` | Convert to profile 8.1, preserving luma/chroma mapping |

&nbsp;

---

# Dolby Vision metadata utilities

These commands operate on **RPU binary files** — they do not touch AV1 bitstreams.

## Commands

* ### **info**

    Prints the parsed RPU data as JSON for a specific frame.
    Frame indices start at 0.

    **Example (frame 124)**:
    ```console
    dovi_tool info -i RPU.bin -f 123
    ```

&nbsp;

* ### **generate**

    Generates a binary RPU from different sources.

    #### From a CMv2.9 or CMv4.0 Dolby Vision XML file

    ```console
    dovi_tool generate --xml dolbyvision_metadata.xml -o RPU_from_xml.bin
    ```

    #### From a generator config JSON

    See [generator.md](docs/generator.md) or [examples](assets/generator_examples).

    ```console
    dovi_tool generate -j assets/generator_examples/default_cmv40.json -o RPU_generated.bin
    ```

    #### From an HDR10+ metadata JSON

    L1 metadata is derived from HDR10+. Requires scene information.

    ```console
    dovi_tool generate -j assets/generator_examples/default_cmv40.json \
        --hdr10plus-json hdr10plus_metadata.json -o RPU_from_hdr10plus.bin
    ```

    #### From a madVR HDR measurement file

    ```console
    dovi_tool generate -j assets/generator_examples/default_cmv40.json \
        --madvr-file madmeasure-output.bin -o RPU_from_madVR.bin
    ```

&nbsp;

* ### **editor**

    Edits a binary RPU according to a JSON config.
    See [editor.md](docs/editor.md) or [examples](assets/editor_examples).
    All indices start at 0 and are inclusive.

    ```console
    dovi_tool editor -i RPU.bin -j assets/editor_examples/mode.json -o RPU_mode2.bin
    ```

&nbsp;

* ### **export**

    Exports a binary RPU file to text/JSON.

    - `-d`, `--data` — export parameters (`key=output` format):
      - `all` — full RPU list as JSON
      - `scenes` — frame indices where `scene_refresh_flag = 1`
      - `level5` — L5 metadata as an editor config JSON

    ```console
    dovi_tool export -i RPU.bin -d all=RPU_export.json

    dovi_tool export -i RPU.bin -d scenes,level5=L5.json
    ```

&nbsp;

* ### **plot**

    Plots RPU metadata as a PNG graph.

    **Flags:**
    - `-t`, `--title` — title at the top
    - `-s`, `--start` — frame range start
    - `-e`, `--end` — frame range end (inclusive)
    - `-p`, `--plot-type` — DV level to plot: `l1` (default), `l2`, `l8`, `l8-saturation`, `l8-hue`
    - `--target-nits` — target brightness for L2/L8 plots: `100` (default), `300`, `600`, `1000`, `2000`, `4000`
    - `--trims` — trim parameters for L2/L8 plots

    ```console
    dovi_tool plot RPU.bin -t "Dolby Vision L1 plot" -o L1_plot.png

    dovi_tool plot RPU.bin -p l2 --target-nits 1000
    ```

&nbsp;

---

# AV1 bitstream commands

These commands read and write **AV1 bitstreams** (raw OBU or IVF container).
Both formats are auto-detected — no flag needed.

## Commands

* ### **extract-rpu**

    Extracts Dolby Vision RPU data from an AV1 bitstream and writes it to a binary RPU file.

    **Supported input formats:**
    - Raw AV1 OBU stream (`.obu`, `.av1`)
    - IVF container (`.ivf`)

    **Supports Dolby Vision profiles 5, 7, 8.1, 8.4.**

    **Flags:**
    - `-l`, `--limit` — stop after N OBUs

    **Examples:**
    ```console
    dovi_tool extract-rpu video.ivf -o RPU.bin

    dovi_tool extract-rpu video.obu -o RPU.bin

    # Pipe from ffmpeg
    ffmpeg -i input.mkv -map 0:v:0 -c copy -f ivf - | dovi_tool extract-rpu - -o RPU.bin
    ```

&nbsp;

* ### **inject-rpu**

    Interleaves Dolby Vision RPU `OBU_METADATA` units into an AV1 bitstream, one per temporal unit.

    The RPU OBU is placed immediately after the `OBU_TEMPORAL_DELIMITER` of each temporal unit.
    Any existing DoVi RPU OBUs in the input are replaced.

    Global `--mode` / `--crop` / `--edit-config` options have no effect during injection.
    Apply them using the `convert` command or `editor` command separately.

    **Mismatch handling:**
    - If the RPU file has **more** entries than video frames, excess is ignored with a warning.
    - If the RPU file has **fewer** entries than video frames, the last RPU is duplicated with a warning.

    **Examples:**
    ```console
    dovi_tool inject-rpu -i video.ivf --rpu-in RPU.bin -o injected_output.ivf

    dovi_tool inject-rpu -i video.obu --rpu-in RPU.bin -o injected_output.av1
    ```

&nbsp;

* ### **convert**

    Converts the Dolby Vision RPU within an AV1 bitstream in a single pass.
    Reads each RPU, applies the conversion, and writes back the modified bitstream.

    Use `-m` / `--mode`, `--crop`, and `--edit-config` to control what conversion is applied.

    **Examples:**
    ```console
    # Convert profile 7 FEL to profile 8.1
    dovi_tool -m 2 convert video.ivf -o converted.ivf

    # Crop letterbox bars
    dovi_tool --crop convert video.ivf -o cropped.ivf
    ```

&nbsp;

* ### **remove**

    Removes all Dolby Vision RPU `OBU_METADATA` units from an AV1 bitstream.
    Outputs to `BL.av1` by default.

    **Examples:**
    ```console
    dovi_tool remove video.ivf -o BL.ivf

    dovi_tool remove video.obu -o BL.av1
    ```

&nbsp;

---

# Architecture and implementation details

This section documents the AV1 migration and internal design.
The original tool worked with **HEVC** bitstreams (NAL units / UNSPEC62 RPU NALs).
The tool was fully rewritten to target **AV1** bitstreams with `OBU_METADATA` units.

---

## AV1 concepts (same as hdr10plus_tool)

### OBU (Open Bitstream Unit)

The fundamental unit of an AV1 bitstream.

```
┌──────────────────────────────────────────────────┐
│  Header byte                                     │
│    bit 7   : forbidden_zero_bit = 0              │
│    bits 6-3: obu_type (4 bits)                   │
│    bit 2   : obu_extension_flag                  │
│    bit 1   : obu_has_size_field                  │
│    bit 0   : reserved = 0                        │
├──────────────────────────────────────────────────┤
│  Optional extension byte (if extension_flag=1)   │
│    bits 7-5: temporal_id                         │
│    bits 4-3: spatial_id                          │
│    bits 2-0: reserved                            │
├──────────────────────────────────────────────────┤
│  Payload size (LEB128, only if size_field=1)     │
├──────────────────────────────────────────────────┤
│  Payload                                         │
└──────────────────────────────────────────────────┘
```

**Only the Low Overhead Bitstream Format is supported** (`obu_has_size_field = 1`).

### OBU types

| Constant | Value | Meaning |
|----------|-------|---------|
| `OBU_SEQUENCE_HEADER` | 1 | Codec configuration |
| `OBU_TEMPORAL_DELIMITER` | 2 | Marks the start of a new temporal unit |
| `OBU_FRAME_HEADER` | 3 | Frame header |
| `OBU_METADATA` | 5 | Metadata (DoVi, HDR10+, …) |
| `OBU_FRAME` | 6 | Combined frame header + tile data |

### Temporal unit

A temporal unit (TU) = one display moment. TUs are delimited by `OBU_TEMPORAL_DELIMITER` in raw streams. In IVF, each frame is one TU.

AV1 is always in **display order** — no B-frame reordering. One RPU per temporal unit.

### LEB128

OBU payload sizes and `metadata_type` fields are encoded as unsigned LEB128.

---

## Dolby Vision in AV1

Dolby Vision RPUs are carried as `OBU_METADATA` with `metadata_type = 4` (METADATA_TYPE_ITUT_T35),
wrapped in an **EMDF (Extensible Metadata Delivery Format) container**.

### Complete byte layout of an AV1 DoVi RPU OBU

```
OBU header byte  = 0x2A     (obu_type=5, has_size_field=1)
OBU payload size (LEB128)
  metadata_type  (LEB128)  = 0x04
  country_code   (u8)      = 0xB5   (United States)
  terminal_provider_code   (u16 BE) = 0x003B
  terminal_provider_oriented_code  (u32 BE) = 0x00000800
  <EMDF container>
    emdf_version            (2 bits) = 0
    key_id                  (3 bits) = 6
    emdf_payload_id         (5 bits) = 31
    emdf_payload_id_ext     (variable) = 225
    flags                   (5 bits) = smploffste=0, duratione=0, groupide=0, codecdatae=0, discard_unknown_payload=1
    emdf_payload_size       (variable)
    <RPU bytes starting with 0x19>
    emdf_payload_id         (5 bits) = 0
    emdf_protection         (2+2+8 bits) = 1, 0, 0
```

### Key difference from HDR10+

HDR10+ uses a simple T.35 header + raw bitstream payload.
Dolby Vision uses **EMDF wrapping** — the RPU bytes are encapsulated in an EMDF container inside the T.35 payload. All EMDF parsing/writing is handled by the `dolby_vision` library (`dolby_vision/src/av1/`).

### Detection signature

To identify a Dolby Vision RPU OBU, check:
1. `obu_type == 5` (OBU_METADATA)
2. `metadata_type == 4` (LEB128, ITU-T T.35)
3. `country_code == 0xB5`
4. Next 9 bytes == `ITU_T35_DOVI_RPU_PAYLOAD_HEADER`:
   `[0x00, 0x3B, 0x00, 0x00, 0x08, 0x00, 0x37, 0xCD, 0x08]`

This is implemented in `extract_dovi_t35_payload()` in `src/dovi/av1_parser.rs`.

---

## IVF container format

IVF is a simple container for raw AV1 bitstreams produced by encoders such as `aomenc`.

### File header (32 bytes)

```
bytes  0– 3:  magic "DKIF"
bytes  4– 5:  version (u16 LE)
bytes  6– 7:  header_size = 32 (u16 LE)
bytes  8–11:  codec FourCC (e.g. "AV01")
bytes 12–13:  width (u16 LE)
bytes 14–15:  height (u16 LE)
bytes 16–19:  timebase denominator (u32 LE)
bytes 20–23:  timebase numerator (u32 LE)
bytes 24–27:  frame count (u32 LE)
bytes 28–31:  reserved
```

### Frame header (12 bytes, repeated per temporal unit)

```
bytes 0– 3:  frame_size (u32 LE)  — bytes of OBU data that follow
bytes 4–11:  timestamp (u64 LE)   — in stream timebase units
```

**Detection:** The first 4 bytes are peeked using `BufRead::fill_buf()` without consuming.
If they match `DKIF`, the 32-byte header is consumed and IVF mode is active.

---

## RPU binary file format

The `.bin` RPU file is the interchange format between `extract-rpu` and `inject-rpu`.
It uses the same format as HEVC UNSPEC62 NAL units, for compatibility with other tools:

```
[ 00 00 00 01 ]  4-byte start code
[ 7C 01 ]        HEVC NAL unit header (nal_unit_type = 62 = UNSPEC62)
[ 19 ... 80 ]    RPU data (prefix byte 0x19, CRC32, final byte 0x80)
```

Repeated for each frame. This binary format is read by `dolby_vision::rpu::utils::parse_rpu_file()` which finds `[0,0,0,1]` start codes and calls `DoviRpu::parse_unspec62_nalu()`.

**Why HEVC-format for AV1?** The RPU binary itself (`0x19 … 0x80`) is codec-agnostic. Storing it in the HEVC NAL wrapper is simply a convention — the data round-trips correctly through `parse_rpu_file` → `inject-rpu` (which re-encodes as AV1 EMDF OBU).

---

## Library API used

All low-level RPU parsing and encoding is in the `dolby_vision` crate:

| Method | Description |
|--------|-------------|
| `DoviRpu::parse_itu_t35_dovi_metadata_obu(data)` | Parse T.35 payload (from `0xB5`) → `DoviRpu` |
| `rpu.write_av1_rpu_metadata_obu_t35_complete()` | Encode RPU → `[0xB5, <EMDF payload>]` |
| `rpu.write_hevc_unspec62_nalu()` | Encode RPU → `[0x7C, 0x01, <RPU bytes>]` |
| `rpu.convert_with_mode(mode)` | Apply profile conversion in-place |
| `rpu.crop()` | Zero active area offsets in-place |
| `parse_rpu_file(path)` | Read `.bin` RPU file → `Vec<DoviRpu>` |
| `ITU_T35_DOVI_RPU_PAYLOAD_HEADER` | 9-byte constant for DoVi T.35 detection |

The EMDF container is handled internally by `dolby_vision/src/av1/emdf.rs`.

---

## AV1 parser (`src/dovi/av1_parser.rs`)

### `Obu::read_from<R: Read>`

Reads one complete OBU from a byte stream:
1. Read 1-byte header (check forbidden bit).
2. If `extension_flag`, read 1 more byte (temporal_id, spatial_id).
3. Bail if `has_size_field == 0` (unsupported).
4. Read LEB128 payload size.
5. Read payload bytes.
6. Store header + size bytes + payload in `raw_bytes` for lossless pass-through.

### `extract_dovi_t35_payload(obu_payload)`

Checks metadata_type = 4, country_code = 0xB5, and the 9-byte `ITU_T35_DOVI_RPU_PAYLOAD_HEADER`.
Returns a slice starting at `0xB5` suitable for `DoviRpu::parse_itu_t35_dovi_metadata_obu`.

### `build_dovi_obu(rpu)`

1. Call `rpu.write_av1_rpu_metadata_obu_t35_complete()` → `[0xB5, <EMDF>]`.
2. Prepend `metadata_type = 4` as LEB128 → OBU payload.
3. Encode payload length as LEB128.
4. Prepend header byte `0x2A` (type=OBU_METADATA, has_size_field=1).

---

## inject-rpu logic (`src/dovi/rpu_injector.rs`)

### Container detection

```
open input → try_read_ivf_file_header()
  DKIF found → write IVF header → inject_ivf()
  not found  →                    inject_raw()
```

### IVF injection (`inject_ivf`)

```
for each IVF frame:
  read 12-byte frame header
  read frame_size bytes
  parse OBUs from frame data
  build_dovi_obu(rpus[tu_index])
  build_output_frame(obus, encoded_obu)
  write updated IVF frame header (new frame_size)
  write output frame bytes
```

### Raw stream injection (`inject_raw`)

State machine buffering one TU at a time:

```
current_td = None, pending = []

for each OBU (or EOF):
  if (EOF or TD) and current_td is set:
    write td.raw_bytes
    write build_dovi_obu(rpus[tu_index])
    write pending OBUs, skipping is_dovi_rpu_obu()

  if OBU is TD: current_td = OBU; pending.clear()
  elif current_td set: pending.push(OBU)
  else: write OBU (pre-stream passthrough)
```

### `build_output_frame`

For IVF frames: same logic as raw inject_raw flush but operating on a `Vec<Obu>`:
- Find position of `OBU_TEMPORAL_DELIMITER` → inject after it (or at position 0).
- Drop any existing `is_dovi_rpu_obu`.

---

## convert logic (`src/dovi/converter.rs`)

Single-pass in-place conversion:

```
for each temporal unit:
  for each OBU:
    if is_dovi_rpu_obu:
      t35 = extract_dovi_t35_payload(obu.payload)
      rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35)
      convert_rpu_with_opts(&options, &mut rpu)   ← apply mode/crop/edit_config
      new_obu = build_dovi_obu(&rpu)
      write new_obu
    else:
      write obu.raw_bytes unchanged
```

`convert_rpu_with_opts` (in `src/dovi/mod.rs`):
1. If `edit_config` is set: apply `edit_config.execute_single_rpu(&mut rpu)`.
2. Else if `mode` is set: `rpu.convert_with_mode(mode)`.
3. If `crop`: `rpu.crop()`.

---

## File structure

```
dolby_vision/
  src/
    av1/mod.rs          — DoVi T.35 parse/encode + EMDF wrapper/unwrapper
    av1/emdf.rs         — EMDF container read/write
    rpu/dovi_rpu.rs     — DoviRpu: parse, encode, convert, crop
    rpu/utils.rs        — parse_rpu_file (reads .bin RPU files)

src/
  dovi/
    av1_parser.rs       — Obu, LEB128, DoVi detection, build_dovi_obu, IVF support
    mod.rs              — CliOptions, initialize_progress_bar, convert_rpu_with_opts, write_rpu_file
    rpu_extractor.rs    — extract-rpu: AV1 IVF + raw → RPU.bin
    rpu_injector.rs     — inject-rpu: RPU.bin + AV1 → injected AV1
    remover.rs          — remove: strip DoVi OBUs from AV1
    converter.rs        — convert: in-place RPU profile conversion in AV1
    editor.rs           — editor: edit RPU binary files (pure metadata)
    exporter.rs         — export: RPU binary → JSON
    generator.rs        — generate: create RPU from config/XML/HDR10+/madVR
    plotter.rs          — plot: RPU metadata → PNG
    rpu_info.rs         — info: print RPU frame data
  commands/
    mod.rs              — Commands enum, ConversionModeCli
    extract_rpu.rs      — ExtractRpuArgs
    inject_rpu.rs       — InjectRpuArgs
    convert.rs          — ConvertArgs
    remove.rs           — RemoveArgs
    editor.rs           — EditorArgs
    export.rs           — ExportArgs
    generate.rs         — GenerateArgs
    info.rs             — InfoArgs
    plot.rs             — PlotArgs
  main.rs               — CLI entry point (clap)
```

---

## Removed from the HEVC version

| Item | Reason |
|------|--------|
| `demux` command | Dual-layer HEVC demux — not applicable to AV1 (AV1 DV is always single-layer) |
| `mux` command | Dual-layer HEVC mux — not applicable to AV1 |
| `general_read_write.rs` | HEVC-specific DoviProcessor/DoviWriter pipeline |
| `hdr10plus_utils.rs` | HEVC SEI HDR10+ stripping |
| `hevc_parser` dependency | Replaced by `src/dovi/av1_parser.rs` |
| `--drop-hdr10plus` flag | HDR10+ is in a separate OBU and does not interfere with DoVi in AV1 |
| `--start-code` flag | HEVC Annex B vs 4-byte — not applicable to AV1 |
| `--discard` flag on convert | Discarding the EL — not applicable to single-layer AV1 |
| `--no-add-aud` flag on inject | AUD NALUs — HEVC concept, not applicable to AV1 |

---

## Sample workflow

```console
# Encode AV1 to IVF (example with aomenc)
aomenc input.y4m --ivf -o video.ivf

# Extract existing RPU (if Dolby Vision is already present)
dovi_tool extract-rpu video.ivf -o RPU.bin

# Inspect an RPU frame
dovi_tool info -i RPU.bin -f 0

# Generate RPU from config
dovi_tool generate -j assets/generator_examples/default_cmv40.json -o RPU_generated.bin

# Inject RPU into AV1
dovi_tool inject-rpu -i video.ivf --rpu-in RPU_generated.bin -o injected_output.ivf

# Verify injection (extract back)
dovi_tool extract-rpu injected_output.ivf -o RPU_verify.bin

# Convert profile 7 to 8.1 in-place
dovi_tool -m 2 convert injected_output.ivf -o converted.ivf

# Remove Dolby Vision RPU entirely
dovi_tool remove injected_output.ivf -o clean_output.ivf

# Mux to MKV
mkvmerge -o output.mkv injected_output.ivf
```
