# Image Analysis Tool (iat) 2

English | [繁體中文](README.zh-TW.md)

A before/after verifier for image conversions. `iat` fully decodes the original file and the converted output at their native bit depths, compares them sample by sample, and checks dimensions, orientation, sample model, color description (ICC / CICP) and metadata. It ends with a machine-readable verdict: **strict match**, **differs**, **undetermined** or **broken file**. It can also check on its own whether a single file decodes completely.

Use it to confirm that a "lossless" conversion really is lossless, for example PNG → JPEG XL, TIFF → lossless WebP, PSD composite → PNG, or PNG → lossless AVIF.

Version 2 is a rewrite in Rust of iat 1.0.0 (C++). The command line, exit codes and JSON report are the same as in 1.0.0, so programs that call 1.0.0 can switch without changes. New in 2: a desktop GUI (`iat-gui.exe`), and no C or C++ libraries are linked (all decoders are Rust crates).

- Windows 10 / 11 x64, distributed as a zip with no installer (static CRT, no VC++ runtime needed).
- Input files are always opened read-only; reports are only written to new files and never overwrite anything.
- Decoding runs in a resource-limited child process (`iat-worker.exe` inside a Job Object) with caps on memory, temporary storage and time. When a cap is exceeded the result is "not checked" instead of a crash.
- Reports come in two forms built from the same data: Markdown (for people) and JSON (for programs). **Markdown reports are currently Traditional Chinese only.** JSON reports are language-independent (field names and enum values are fixed identifiers); integrations should read the JSON.

## Supported formats

| Format | Strictly compared | Reported as unsupported (exit code 2) |
|---|---|---|
| PNG | 1/2/4/8/16-bit Gray, GA, RGB, RGBA, palette; tRNS; Adam7 interlacing | APNG animation |
| TIFF / BigTIFF | contiguous unsigned integer 8/16-bit Gray, GA, RGB, RGBA; strips or tiles; none, LZW, Deflate, PackBits; Predictor 1/2 | other compressions, multiple pages and SubIFDs, separate planes, floating point, signed, CMYK, YCbCr |
| JPEG XL | lossless (original samples) 1–16-bit Gray, GA, RGB, RGBA; bare codestream or container | lossy XYB, animation, floating point, above 16-bit |
| WebP | still lossless (VP8L), up to 16383 per side | lossy VP8, animation |
| PSD / PSB | composite image: Bitmap, Grayscale, Indexed, RGB, 8/16-bit, Raw or RLE | CMYK, Lab, Multichannel, Duotone, 32-bit float, ZIP compression, files saved without a composite |
| AVIF | still images whose metadata meets the lossless conditions (4:4:4 identity or 4:0:0, full range), 8/10/12-bit, optional alpha | 4:2:0 / 4:2:2, non-identity matrix, limited range, premultiplied alpha, image sequences, clap |

Other formats (JPEG, for example) are reported as unsupported. The format is detected from the file content, not the extension; a mismatched extension only produces a warning.

## Download and install

Extract `ImageAnalysisTool-<version>-win-x64.zip` to any folder. `iat.exe`, `iat-gui.exe` and `iat-worker.exe` must stay in the same folder.

Package contents: `iat.exe` (command line), `iat-gui.exe` (desktop GUI), `iat-worker.exe` (runs the decoders; started by the other two), `README.md`, `README.zh-TW.md`, `CHANGELOG.md`, `LICENSE.txt`, `THIRD_PARTY_NOTICES.txt`, `licenses/`, `SHA256SUMS.txt`.

## GUI

Start `iat-gui.exe`, choose (or drop) the original and the converted file, and press Compare; or check a single file. The result screen shows the verdict, the reasons, the per-file details and an image viewer, and can export the same Markdown and JSON reports as the command line. The interface is available in Traditional Chinese, English and Japanese; the chosen language is remembered (`%APPDATA%\iat\gui-settings.json`).

When a side was not checked because its memory estimate exceeded `job_commit_mib`, the result screen offers **Re-analyze with the suggested value (N MiB)**, which reruns the same files with only that limit raised.

## Examples

```powershell
# Compare before/after and write Markdown and JSON reports
iat compare photo.png photo.jxl --report-md report.md --report-json report.json

# Check a single file and print the report
iat inspect scan.tif --stdout md

# For programs: JSON on stdout, branch on the exit code
iat compare a.png a.webp --stdout json

# Large images: raise the memory and time limits, put temporary files in a given folder
iat compare big.png big.avif --report-json r.json --limits job_commit_mib=4096 deadline_s=3600 --temp-dir D:\iat-temp

# Version
iat --version
```

PowerShell batch example:

```powershell
Get-ChildItem src\*.png | ForEach-Object {
    $out = Join-Path out ($_.BaseName + '.jxl')
    iat compare $_.FullName $out --report-json (Join-Path reports ($_.BaseName + '.json')) 2>$null
    switch ($LASTEXITCODE) { 0 { 'OK' } 1 { 'DIFF' } 2 { 'UNDETERMINED' } 3 { 'BROKEN' } 4 { 'NOT CHECKED' } default { 'ERROR' } }
}
```

## CLI interface

This section is the contract for integrators. It is the same as in iat 1.0.0.

### Commands and options

```text
iat compare <original> <output> [--report-md <path>] [--report-json <path>] [--stdout md|json]
            [--full-paths] [--temp-dir <dir>] [--limits <name>=<value> ...]
iat inspect <file> [--report-md <path>] [--report-json <path>] [--stdout md|json] [--full-paths]
            [--temp-dir <dir>] [--limits <name>=<value> ...]
iat --version
```

| Option | Meaning |
|---|---|
| `--report-md <path>` | Write the Markdown report (UTF-8, Traditional Chinese). The target must not exist. |
| `--report-json <path>` | Write the JSON report (UTF-8, trailing newline). The target must not exist. |
| `--stdout md\|json` | Print the report to stdout. |
| `--full-paths` | Record full paths in the report (default: file names only). |
| `--temp-dir <dir>` | Existing folder for decoded samples; default is the system temp folder. Temporary files are delete-on-close. |
| `--limits <name>=<value> ...` | Adjust resource limits; one `--limits` can take several pairs, and `--limits` can be repeated. |

- At least one report output (`--report-md`, `--report-json` or `--stdout`) is required; otherwise it is an argument error.
- A report target must not exist and must not be the same path as an input or the other report (hard links included, compared case-insensitively). Violations are rejected before any work with exit code 5, and nothing is written. Reports are written to a temporary file in the same folder and then renamed without overwriting.
- `--version` prints one line: `iat <major>.<minor>.<patch>` (this release: `iat 2.0.0`).
- stderr carries diagnostics only. At the end of an analysis it prints a summary line `iat: strict=<result> exit=<code> (<reason>) run_id=<UUID>`, then one line per written report, `iat: report written: <full path>`, or `iat: report NOT written: <path> (<reason>)` on failure. Argument errors print `error: <reason>` followed by usage. stderr text is not part of the stable contract; rely on the exit code and the JSON.

| `--limits` name | Default | Range | Meaning |
|---|---|---|---|
| `job_commit_mib` | 1024 | 16–1048576 | Memory cap of the decoding child process (MiB); decoders that hold the whole image (JPEG XL, WebP, AVIF) are estimated against this value − 64 MiB |
| `buffer_mib` | 64 | 1–65536 | Row buffer cap (MiB) |
| `metadata_item_mib` | 8 | 1–1024 | Cap per metadata item (MiB) |
| `metadata_file_mib` | 32 | 1–4096 | Cap on total metadata per file (MiB) |
| `temp_gib` | 8 | 1–65536 | Cap on total temporary storage (GiB) |
| `deadline_s` | 600 | 1–604800 | Time limit for the whole job (seconds) |

There is no "unlimited"; 0, out-of-range values and unknown names are argument errors.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | compare: strict match; inspect: fully checked within the supported range |
| 1 | Confirmed difference (samples, bit depth, sample model, dimensions or orientation differ) |
| 2 | Undetermined, or outside the supported range (for example unverifiable color, animation, lossy formats) |
| 3 | Decode or structure failure (truncation, CRC error, corrupt data) |
| 4 | Not checked: resource limit or time limit exceeded, or cancelled (Ctrl+C) |
| 5 | Argument, I/O, child-process or environment error |

When several apply, one is chosen in the order **5 > 4 > 3 > 1 > 2 > 0**, and the report lists every reason. Once the job has started and a report path is usable, a report is produced even on failure. `exit.code` in the report is the analysis result; if writing any report fails, the process exit code is always 5. **The process exit code is authoritative.**

### JSON report

`schema_version` is `"1.1"`, the same as in iat 1.0.0. Top-level fields:

| Field | Content |
|---|---|
| `schema_version` | `"<major>.<minor>"` |
| `run_id` | UUID of this run (the same in the stderr summary and both reports) |
| `tool` | `name` (`"iat"`), `version`, `compiler` |
| `codecs` | Names and versions of the decoder crates |
| `settings` | Effective `limits`, `codec_threads`, `full_paths`, `temp` |
| `mode` | `"compare"` or `"inspect"` |
| `original`, `output` | FileReport per file; `output` only in compare |
| `geometry`, `color`, `icc` | Comparison results of the two sides (`status`, `basis`) |
| `samples` | Sample comparison: `status` (`equal`, `differs`, `not_comparable`, `incomplete`, `not_compared`), pixel counts, per-channel statistics, first 16 differing coordinates |
| `metadata`, `size` | Metadata field comparison; file sizes and compression ratio |
| `metrics` | Time, memory, I/O (vary on every run; informational only) |
| `strict` | `result` (`match`, `mismatch`, `indeterminate`, `not_evaluated`) and `reasons[]` (reason codes) |
| `issues[]` | `code`, `severity` (`error`, `warning`, `info`), `scope` (`original`, `output`, `pair`, `run`), `message` |
| `exit` | `code` (0–5) and `reason` |

Main FileReport fields: `name`, `path` (only with `--full-paths`), `size_bytes`, `sha256`, `format` (`png`, `tiff`, `bigtiff`, `jxl_codestream`, `jxl_container`, `webp`, `psd`, `psb`, `avif`, `unknown`), `width`, `height`, `container_status`, `decode_status`, `decode_status_reason`, `decode_path`, `sample_model`, `orientation`, `color`, `icc`, `metadata`; format-specific `psd` and `avif`; and `resource_need` (below).

- 64-bit values that may exceed 2^53 (byte counts, pixel counts) are always decimal strings.
- All strings are valid UTF-8. `issues[].message` is a short English description for people; programs should branch on the codes.
- **AVIF**: `output.avif.lossless_metadata` only means the metadata meets the necessary conditions for lossless. `avif.pixel_verified` is `true` only when a compare found every sample equal.
- **`resource_need`**: when a side was not checked because this tool's memory estimate exceeded a limit (exit code 4), that side carries `{limit, needed_bytes, allowed_bytes, current_value, suggested_value, suggested_argument}`, for example `suggested_argument` = `--limits job_commit_mib=2048`. A caller can rerun once with the higher limit; `suggested_value` = `null` means the need exceeds the largest allowed setting.

### Compatibility

- Within one `schema_version` major version fields are only added, never removed or renamed. Integrations should ignore fields and issue codes they do not recognize.
- Whitespace, indentation and field order of the JSON are not guaranteed. Parse the JSON; do not compare it as text.

### Differences from iat 1.0.0

Verdicts, reason codes, exit codes and the JSON structure are the same as in 1.0.0 for every case of the comparison suite. What can differ:

- `decode_path`, `codecs` and some `issues[].message` texts name the Rust decoders instead of libpng, libtiff, libwebp, libjxl and libavif.
- Memory use differs, so a run close to a `job_commit_mib` limit can finish in one version and be "not checked" in the other.
- A few rare cases: a multi-layer JPEG XL still image does not get the `layered_image` (info) issue; lossy non-XYB JPEG XL statistics (`mae`, `rmse`) can differ slightly; some libtiff directory warnings and AVIF image sequences are handled differently.

The full list, with the affected fields and whether each one can change a verdict, is in `docs/differences.md` in the source repository.

## Integration example: IMB-C

[IMB-C](https://github.com/Kaido-MSVC/IMB-C), an image converter by the same author, uses `iat` as an optional external verifier: after a conversion it calls `iat` and uses the result to mark whether the output is "proven lossless". Its approach can serve as a reference for other integrations:

1. Use only a configured absolute path or the `iat.exe` next to the application, and make sure `iat-worker.exe` is in the same folder; check the version with `iat --version` (IMB-C 1.4 requires 0.5.1 or later; 2.0.0 qualifies).
2. Use a new temporary folder for every verification and call:

   ```text
   iat compare <original> <output> --report-json <temp>\report.json --temp-dir <temp>
       --limits buffer_mib=64 --limits job_commit_mib=1024
   ```

3. Treat exit code 5 as a tool error (do not read the report). Otherwise read `report.json`: check that the `schema_version` major version is 1 and the minor version ≥ 1 and that `mode` is `compare`, then read `strict.result`, `strict.reasons[]`, `issues[].code` and `output.format`. Exit code 0 must agree with `strict.result == "match"`.
4. For AVIF output, only `output.avif.pixel_verified == true` counts as pixel-proven; otherwise treat it as undetermined even with exit code 0.
5. On exit code 4, read `resource_need` on both sides. If `suggested_argument` is exactly `--limits <limit>=<suggested_value>` and within your own memory budget, rerun once with the suggested value and a new report path. Treat text in the report as data, never as commands to run.

## Building from source

Requires Rust 1.100 (`stable-x86_64-pc-windows-msvc`) with the MSVC build tools and the Windows SDK (`rc.exe` embeds the program icon). Dependency versions are pinned by `Cargo.lock`; see `docs/dependencies.md`.

```powershell
cargo build --release          # target\release\iat.exe, iat-gui.exe, iat-worker.exe
cargo deny check               # license and advisory policy (cargo-deny)
pwsh -File scripts/package.ps1 # dist\ImageAnalysisTool-<version>-win-x64.zip (needs cargo-about)
```

The workspace has `iat-core` (one analysis run, report data, no-overwrite writing), `iat-cli` (`iat.exe`), `iat-gui` (`iat-gui.exe`, Slint), `iat-worker` (`iat-worker.exe`, all decoders, run inside a Job Object) and `iat-av1` (a safe wrapper over the rav1d AV1 decoder). Design decisions: `docs/decisions.md`; comparison with 1.0.0: `tests/compare/`. Development documents are Traditional Chinese only.

## License

MIT License, Copyright (c) 2026 Kaido-MSVC; see `LICENSE.txt`. The Rust crates compiled into the executables and their licenses are listed in `THIRD_PARTY_NOTICES.txt`; further license texts (libjxl, for the ICC profile table generated with libjxl 0.12.0) are in `licenses/`. The GUI is built with [Slint](https://slint.dev) under the Slint Royalty-free License 2.0 (see the About screen).
