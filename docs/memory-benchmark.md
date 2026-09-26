# Wu vs. Zed: memory benchmarks

Wu used less settled process memory than Zed in every measured stage of this comparison: **9.0–10.0% less at idle, 6.7–58.2% less in the workspace workflow, and 3.7–4.4% less with rust-analyzer running.**

Tested on September 26, 2026, using the official Linux ARM64 releases of [Wu 1.0.10](https://github.com/farshed/wu/releases/tag/v1.0.10) (`95d3219`) and [Zed 1.21.0](https://github.com/zed-industries/zed/releases/tag/v1.21.0) (`33c9585`). These were the latest stable releases when the run started. Both downloaded archives matched GitHub's published SHA-256 digests.

> These are software-rendered Linux VPS results. Both editors ran real graphical interfaces through Xvfb and Mesa llvmpipe, which renders on the CPU. The numbers are not estimates of memory use on a hardware-GPU desktop.

## Results

All values are **MiB of total process-tree PSS**, where lower is better. PSS accounts proportionally for shared memory instead of counting shared pages repeatedly. Totals include the editor and its running child processes, including language servers, terminal shells, and compilers.

Each value is the median of three independent runs' settled-memory medians. Percentage reductions are relative to Zed and calculated before rounding. Active stages run sequentially within each suite, so later rows include memory retained from earlier actions—not just the cost of that individual feature.

### Idle

| Scenario | Wu | Zed | Wu reduction |
| --- | ---: | ---: | ---: |
| Welcome window | 382.5 | 420.4 | 9.0% |
| Repository open, no source file open | 381.2 | 423.7 | 10.0% |

### Workspace workflow

A snapshot of Wu's source repository at commit `82c05888a2bfffe6439329070c345100f6f7e96e`, with language servers disabled to measure editor operations separately from language-server work. The source snapshot and generated fixture files match the September 19 benchmark, keeping the workload fixed while updating the editor releases. The repository-open idle scenario also uses a clean clone of this revision.

| Stage | Wu | Zed | Wu reduction |
| --- | ---: | ---: | ---: |
| Open 20 source files | 516.9 | 554.3 | 6.7% |
| Edit, save, and scroll | 518.8 | 557.2 | 6.9% |
| Search the repository | 582.9 | 1395.3 | 58.2% |
| Navigate and search a large log | 832.7 | 1633.9 | 49.0% |
| Generate terminal output | 862.8 | 1664.4 | 48.2% |
| Close tabs, retaining project state | 860.9 | 1631.0 | 47.2% |

The workflow performs 20 verified edit/save cycles, three targeted repository searches, navigation and search in a 200,000-line log (21.9 MiB), and 20,000 lines of integrated-terminal output. The terminal retains 10,000 lines of scrollback.

The largest change from the previous benchmark is repository search: Wu's settled PSS fell from 1234.5 MiB with 1.0.8 to 582.9 MiB with 1.0.10, a **52.8% reduction** between the recorded runs. Its three new search-stage medians were 582.2, 582.9, and 583.0 MiB; Zed's were 1421.1, 1310.9, and 1395.3 MiB. The final search still returned 157 matches across 122 files. Later stages retain search state, so their reductions are not independent measurements of large-file or terminal efficiency.

Closing tabs does not reset the session: the project and terminal remain loaded. Wu's search panel also remains visible, while Zed's search tab closes. That row compares retained workflow state, not identical empty windows, and does not establish a memory leak.

### Rust development workflow

A generated, dependency-free Rust application with 30 modules and 3,000 functions. Both editors use the same Rust 1.97.1 toolchain and rust-analyzer binary, with checking on save enabled.

| Stage | Wu | Zed | Wu reduction |
| --- | ---: | ---: | ---: |
| Open files and allow indexing | 1122.1 | 1174.3 | 4.4% |
| Edit, save, and scroll with checking | 1127.0 | 1175.3 | 4.1% |
| Go-to-definition, completions, and diagnostics | 1180.9 | 1226.6 | 3.7% |
| Fix the error and run Cargo tests | 1184.7 | 1235.0 | 4.1% |

The workflow opens 11 files, performs 20 verified edit/save cycles, navigates to a definition, requests completions, introduces and fixes a type error, and successfully runs `cargo test --offline` in the integrated terminal.

Language-server memory makes up much of this suite's footprint, narrowing the percentage difference. These are settled totals after each stage; compiler processes are included when sampled during the actions. During the language-feature stage, the median of each run's sampled peak was 1346.4 MiB for Wu and 1409.5 MiB for Zed. Sampled peaks can miss brief allocations and exclude initial launch before the actions begin; they are not exact high-water marks.

## How we measured

- **Host:** Ubuntu 26.04 ARM64, six Neoverse-N1 virtual CPUs, 7.7 GiB RAM, and a 1280 × 800 virtual display. No hardware GPU.
- **Isolation:** fresh profiles and project copies for active runs; only one editor runs at a time, with run order alternated. Automatic updates and telemetry are disabled, and no accounts are signed in.
- **Sampling:** Linux `/proc/PID/smaps_rollup` across each editor's process tree. Active runs wait ten seconds initially, then sample workflows at a nominal 0.5-second interval, followed by a five-second settling period and six final samples per stage. The Rust file-opening stage includes 20 additional seconds for initial indexing. Idle runs settle for 30 seconds before ten samples at one-second intervals.
- **Coverage:** 12 active editor sessions, 60 measured stages, and 2,943 samples, plus 12 idle sessions and 120 samples. All measured editor processes had zero swap PSS.
- **Validation:** all 3,519 workspace-fixture and 34 Rust-fixture file hashes match the previous benchmark and every fresh copy. Saved-edit checks, successful test completion, recomputed per-process totals and summary statistics, and rendered-window screenshots verify the workloads. All benchmark editor, language-server, and Xvfb processes exited afterward.

The totals exclude the display server, benchmark harness, shared desktop services, unmapped filesystem cache, and kernel allocations. They measure editor process memory, not the entire desktop's RAM use. Filesystem caches were not flushed.

## Scope and limitations

Three repetitions describe these workloads, not a universal percentage advantage or a statistically established difference. This compares released products with some different defaults and project settings, not identical upstream builds or the isolated effect of removing a feature. The workspace snapshot retains Wu's `.wu/settings.json`, which Zed does not read.

The search workload uses the same targeted queries as before: `pub struct Rope`, `fn offset_to_point`, and `fn focus_handle`. The broad-query timeout observed in a September 19 setup run with Wu 1.0.8 was not retested here; it should not be treated as a result for 1.0.10. No setup runs are included in these tables.

Software-rendering allocations contribute to these results. Hardware-GPU desktops, other operating systems, larger dependency graphs, signed-in AI features, debugging, and long editing sessions may behave differently. This is a memory comparison, not a responsiveness or frame-rate benchmark.

## Reproduce and inspect

The [benchmark runner](../script/memory-benchmark) supports idle, workspace, and Rust scenarios on Linux. The runner was unchanged for this rerun. It requires Python 3.11+, Xvfb, `xauth`, `dbus-run-session`, `xdotool`, `xwininfo`, `xclip`, ImageMagick, and a working Vulkan driver. The Rust suite also requires Cargo and the pinned Rust toolchain with `rust-src` and `rust-analyzer` installed.

From the repository root, the following commands reproduce the scenarios on this VPS using the retained side-by-side installations and pinned repository clone. Adjust installation and fixture paths for other machines; omit `ZED_ALLOW_ROOT` when running as a normal user. To recreate the fixture clone, check out commit `82c05888a2bfffe6439329070c345100f6f7e96e` in a separate clone, leaving the checkout containing the benchmark runner unchanged.

```sh
export WU_BIN=/root/.local/share/wu-memory-benchmark/releases/wu-1.0.10/wu.app/libexec/wu-editor
export ZED_BIN=/root/.local/share/wu-memory-benchmark/releases/zed-1.21.0/zed.app/libexec/zed-editor
export ZED_ALLOW_ROOT=true ZED_ALLOW_EMULATED_GPU=1
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
benchmark_repository=/root/.cache/wu-memory-benchmark/repository-82c0588

run_benchmark() {
  xvfb-run -a -s '-screen 0 1280x800x24 -nolisten tcp' \
    dbus-run-session -- script/memory-benchmark "$@"
}

run_benchmark --runs 3 --settle 30 --samples 10 --interval 1
run_benchmark --runs 3 --settle 30 --samples 10 --interval 1 \
  --path "$benchmark_repository" --trust-project
run_benchmark --scenario workspace --repository "$benchmark_repository" \
  --runs 3 --settle 10 --phase-settle 5 --samples 6 --interval 0.5
run_benchmark --scenario rust --rust-toolchain 1.97.1-aarch64-unknown-linux-gnu \
  --runs 3 --settle 10 --phase-settle 5 --samples 6 --interval 0.5
```

The measured run's artifacts are retained under `/root/.local/state/wu-memory-benchmark/` in `20260926-welcome`, `20260926-project`, `20260926-active-workspace`, and `20260926-active-rust`. Each contains `results.json`, individual samples, executable hashes and versions, logs, profiles, and screenshots. The active results include per-run settled medians and sampled peaks. These raw artifacts are on the VPS, not bundled with this page; the September 19 artifacts remain available separately.
