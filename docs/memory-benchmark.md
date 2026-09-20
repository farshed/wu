# Wu vs. Zed: memory benchmarks

Wu used less settled process memory than Zed in every measured stage of this comparison: **9.4–9.9% less at idle, 6.0–10.6% less in the workspace workflow, and 1.8–2.7% less with rust-analyzer running.**

Tested using the official Linux ARM64 releases of [Wu 1.0.8](https://github.com/farshed/wu/releases/tag/v1.0.8) and [Zed 1.20.2](https://github.com/zed-industries/zed/releases/tag/v1.20.2).

> These are software-rendered Linux VPS results. Both editors ran real graphical interfaces through Xvfb and Mesa llvmpipe, which renders on the CPU. The numbers are not estimates of memory use on a hardware-GPU desktop.

## Results

All values are **MiB of total process-tree PSS**, where lower is better. PSS accounts proportionally for shared memory instead of counting shared pages repeatedly. Totals include the editor and its running child processes, including language servers, terminal shells, and compilers.

Each value is the median of three independent runs' settled-memory medians. Percentage reductions are relative to Zed and calculated before rounding. Active stages run sequentially within each suite, so later rows include memory retained from earlier actions—not just the cost of that individual feature.

### Idle

| Scenario | Wu | Zed | Wu reduction |
| --- | ---: | ---: | ---: |
| Welcome window | 380.2 | 419.6 | 9.4% |
| Repository open, no source file open | 380.8 | 422.4 | 9.9% |

### Workspace workflow

A snapshot of Wu's source repository, with language servers disabled to measure editor operations separately from language-server work.

| Stage | Wu | Zed | Wu reduction |
| --- | ---: | ---: | ---: |
| Open 20 source files | 520.2 | 553.4 | 6.0% |
| Edit, save, and scroll | 521.0 | 556.6 | 6.4% |
| Search the repository | 1234.5 | 1380.3 | 10.6% |
| Navigate and search a large log | 1466.0 | 1627.2 | 9.9% |
| Generate terminal output | 1496.7 | 1657.8 | 9.7% |
| Close tabs, retaining project state | 1473.7 | 1632.3 | 9.7% |

The workflow performs 20 verified edit/save cycles, three targeted repository searches, navigation and search in a 200,000-line log (21.9 MiB), and 20,000 lines of integrated-terminal output. The terminal retains 10,000 lines of scrollback.

Closing tabs does not reset the session: the project and terminal remain loaded. Wu's search panel also remains visible, while Zed's search tab closes. That row compares retained workflow state, not identical empty windows, and does not establish a memory leak.

### Rust development workflow

A generated, dependency-free Rust application with 30 modules and 3,000 functions. Both editors use the same Rust 1.97.1 toolchain and rust-analyzer binary, with checking on save enabled.

| Stage | Wu | Zed | Wu reduction |
| --- | ---: | ---: | ---: |
| Open files and allow indexing | 1142.1 | 1174.1 | 2.7% |
| Edit, save, and scroll with checking | 1146.8 | 1176.7 | 2.5% |
| Go-to-definition, completions, and diagnostics | 1201.1 | 1223.4 | 1.8% |
| Fix the error and run Cargo tests | 1205.5 | 1227.3 | 1.8% |

The workflow opens 11 files, performs 20 verified edit/save cycles, navigates to a definition, requests completions, introduces and fixes a type error, and successfully runs `cargo test --offline` in the integrated terminal.

Language-server memory makes up much of this suite's footprint, narrowing the percentage difference. These are settled totals after each stage; compiler processes are included when sampled during the actions. The [full report](memory-benchmark.md#rust-language-server-suite) also includes sampled peaks, which were nearly equal during the language-feature stage: 1413.9 MiB for Wu and 1416.3 MiB for Zed.

## How we measured

- **Host:** Ubuntu 26.04 ARM64, six Neoverse-N1 virtual CPUs, 7.7 GiB RAM, and a 1280 × 800 virtual display. No hardware GPU.
- **Isolation:** fresh profiles and project copies for active runs; only one editor runs at a time, with run order alternated. Automatic updates and telemetry are disabled, and no accounts are signed in.
- **Sampling:** Linux `/proc/PID/smaps_rollup` across each editor's process tree. Active workflows are sampled at a nominal 0.5-second interval, followed by a five-second settling period and six final samples per stage. Idle runs settle for 30 seconds before ten samples.
- **Coverage:** 12 active editor sessions, 60 measured stages, and 2,949 samples, plus 12 idle sessions and 120 samples. All measured editor processes had zero swap PSS.
- **Validation:** matching input-file hashes across fresh fixtures, saved-edit checks, successful test completion, per-process accounting checks, and rendered-window screenshots.

The totals exclude the display server, benchmark harness, shared desktop services, unmapped filesystem cache, and kernel allocations. They measure editor process memory, not the entire desktop's RAM use. Filesystem caches were not flushed.

## Scope and limitations

Three repetitions describe these workloads, not a universal percentage advantage or a statistically established difference. This compares released products with some different defaults and project settings, not identical upstream builds or the isolated effect of removing a feature. The workspace snapshot retains Wu's `.wu/settings.json`, which Zed does not read.

The search workload uses targeted queries. A broad-query setup run left Wu unable to service the next file-open request within the timeout; setup runs are excluded from the results. See the [full protocol](memory-benchmark.md#active-workload-protocol) for details.

Software-rendering allocations contribute to these results. Hardware-GPU desktops, other operating systems, larger dependency graphs, signed-in AI features, debugging, and long editing sessions may behave differently. This is a memory comparison, not a responsiveness or frame-rate benchmark.

## Reproduce and inspect

See the [benchmark runner](../script/memory-benchmark). It supports idle, workspace, and Rust scenarios on Linux.
